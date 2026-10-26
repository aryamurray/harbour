#!/usr/bin/env bash
# Redis 7.2.16 -- a server, built from nine targets in one package.
#
# What it asserts, in the order it asserts it:
#
#   1. Every translation unit upstream's `make` compiles for `redis-server`,
#      `redis-cli` and `redis-benchmark` -- 145, counting each shared one once
#      -- is compiled. The count is the guard against a source list that
#      shrank: a server missing `lolwut6.c` still links and still answers PING.
#   2. A build with nothing changed recompiles nothing.
#   3. The server Harbour linked reports the pinned version, then serves
#      commands over TCP: PING, SET/GET, INCR, and Lua. The Lua checks are
#      aimed at what Redis bundles rather than at stock Lua 5.1: cjson, cmsgpack,
#      struct and bit are compiled into `liblua.a` by the same target and none
#      of them is reachable from a plain interpreter.
#   4. `redis-cli` and `redis-benchmark` -- the two programs that link hiredis,
#      one with linenoise and one with hdr_histogram -- talk to it.
#   5. `SHUTDOWN` makes the server exit with status 0.
#
# The server listens on a port the kernel chose, on 127.0.0.1, with persistence
# off and its working directory inside the canary's own directory.
#
# Usage: ci/canary/redis/run.sh [work-dir]
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
. "$here/../lib.sh"

# download.redis.io's release tarball. The project publishes the SHA-256 of
# each release in github.com/redis/redis-hashes; this is that value.
URL="https://download.redis.io/releases/redis-7.2.16.tar.gz"
SHA256=960a8ec15e34ff40e57ff16837b26b33bd81f2da6d24497bb63de532a323a18e
VERSION=7.2.16

# 145 on every platform: Redis has no per-OS source. The event loop is chosen
# inside `ae.c` by a macro, so a Linux and a macOS build compile the same files.
OBJECTS=145

work="${1:-${TMPDIR:-/tmp}/harbour-canary-redis.$$}"
repo="$(canary_repo_root "$here")"
harbour="$(canary_harbour "$repo")"

rm -rf "$work"
mkdir -p "$work"
cd "$work"

canary_fetch "$URL" "$SHA256" "redis-$VERSION"
cp -R "$here/overlay/." upstream/
cp "$here/Harbour.toml" upstream/Harbour.toml

canary_build_library "$harbour" redis
canary_expect_objects "$OBJECTS"

bin="$work/upstream/.harbour/target/debug/bin"
for exe in redis-server redis-cli redis-benchmark; do
  [ -x "$bin/$exe" ] || { echo "== canary FAILED: $exe was not built" >&2; exit 1; }
done

# The kernel picks the port, so two canaries running at once cannot collide.
# Bound and released immediately; the window before the server binds it is the
# usual one for this approach and a collision there fails loudly (the server
# refuses to start), it does not pass wrongly.
port="$(python3 -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')"
data="$work/data"
mkdir -p "$data"

fail() {
  echo "== canary FAILED: $*" >&2
  echo "-- server log --" >&2
  cat "$data/redis.log" >&2 2>/dev/null || true
  exit 1
}

cli() { "$bin/redis-cli" -h 127.0.0.1 -p "$port" "$@"; }

# Compare one reply with the expected one. Captured then tested, never piped,
# for the reason given in canary_defines_symbol.
expect() {
  local want=$1 got
  shift
  got="$(cli "$@")" || fail "'$*' exited non-zero (got: $got)"
  if [ "$got" != "$want" ]; then
    fail "'$*' replied '$got', expected '$want'"
  fi
  echo "ok   $* -> $want"
}

echo "== redis-server --version"
version_line="$("$bin/redis-server" --version)"
echo "$version_line"
case "$version_line" in
  *"v=$VERSION "*) ;;
  *) fail "expected version $VERSION in: $version_line" ;;
esac
# Stated, not hidden: this build uses libc malloc where upstream's Linux default
# is the bundled jemalloc (see the note at the top of Harbour.toml).
case "$version_line" in
  *"malloc=libc"*) ;;
  *) fail "expected malloc=libc in: $version_line" ;;
esac
echo "== redis-cli --version"
case "$("$bin/redis-cli" --version)" in
  "redis-cli $VERSION") ;;
  *) fail "redis-cli did not report $VERSION" ;;
esac

echo "== starting redis-server on port $port"
"$bin/redis-server" --port "$port" --bind 127.0.0.1 --dir "$data" \
  --save "" --appendonly no --logfile "$data/redis.log" &
server_pid=$!
# Stop the server whatever happens next, so a failed assertion cannot leave a
# listener behind on a CI runner.
trap 'kill "$server_pid" 2>/dev/null || true' EXIT

# Wait for it to answer rather than sleeping a fixed time.
ready=0
for _ in $(seq 1 100); do
  if [ "$(cli ping 2>/dev/null)" = "PONG" ]; then ready=1; break; fi
  kill -0 "$server_pid" 2>/dev/null || fail "the server exited before it answered"
  sleep 0.1
done
[ "$ready" -eq 1 ] || fail "the server did not answer PING within 10 s"

expect PONG ping
expect OK set canary value
expect value get canary
expect 1 incr counter
expect 2 incr counter

# The event loop is chosen inside `ae.c` by a macro, with no source selected per
# platform, so this is the one place a build that picked the wrong one -- or
# fell back to `select(2)`, which still passes everything else -- is visible.
case "$(uname -s)" in
  Linux) loop=epoll ;;
  Darwin) loop=kqueue ;;
  *) fail "no expected event loop for $(uname -s)" ;;
esac
got="$(cli info server | tr -d '\r' | sed -n 's/^multiplexing_api://p')"
[ "$got" = "$loop" ] || fail "expected the $loop event loop, INFO reports '$got'"
echo "ok   event loop: $got"

# Lua. `redis.call` is the server's own binding, so the first proves the script
# engine reaches the data model; the others are Redis's additions to Lua.
expect 3 eval "return redis.call('incr', 'counter')" 0
expect "[1,2,3]" eval "return cjson.encode({1,2,3})" 0
expect 7 eval "return cmsgpack.unpack(cmsgpack.pack({7}))[1]" 0
expect 8 eval "return bit.band(12, 10)" 0
expect 258 eval "return struct.unpack('>H', struct.pack('>H', 258))" 0

echo "== redis-benchmark (links hiredis and hdr_histogram)"
bench="$("$bin/redis-benchmark" -h 127.0.0.1 -p "$port" -q -n 2000 -c 5 -t ping_mbulk,set 2>&1)" \
  || fail "redis-benchmark exited non-zero: $bench"
echo "$bench" | tr '\r' '\n' | tail -3
case "$bench" in
  *"requests per second"*) ;;
  *) fail "redis-benchmark did not report a rate: $bench" ;;
esac

echo "== shutting down"
cli shutdown nosave >/dev/null 2>&1 || true
set +e
for _ in $(seq 1 50); do
  kill -0 "$server_pid" 2>/dev/null || break
  sleep 0.1
done
if kill -0 "$server_pid" 2>/dev/null; then
  fail "the server was still running 5 s after SHUTDOWN"
fi
wait "$server_pid"
status=$?
set -e
[ "$status" -eq 0 ] || fail "the server exited with status $status after SHUTDOWN"
trap - EXIT
echo "== canary passed (redis $VERSION)"
