#!/usr/bin/env bash
# Build a real upstream library with Harbour and run something against it.
#
# Nothing else in CI builds third-party code, which is why the archive bug
# (`ar r` matching members by name, so a renamed source left its stale object
# behind) was found by hand on a real package instead of by a test. The
# fixtures in tests/cli_integration.rs are two- and three-file projects; a
# library with fifteen translation units, a public header its consumers must
# find, and platform-conditional defines exercises paths those cannot.
#
# zlib is the first canary because it has no dependencies, is small enough to
# build in seconds, and has a behaviour worth asserting: compress a buffer,
# decompress it, compare. Deliberately *not* openssl or curl -- openssl is
# 1100+ sources per platform and would dominate the CI bill.
#
# Usage: ci/canary/zlib/run.sh [work-dir]
set -euo pipefail

ZLIB_VERSION=1.3.1
ZLIB_URL="https://github.com/madler/zlib/releases/download/v${ZLIB_VERSION}/zlib-${ZLIB_VERSION}.tar.gz"
# Pinned by hash, not just by version tag: a canary that silently follows
# whatever upstream publishes turns a supply-chain change into a Harbour bug
# report, and an interrupted download into a mystery compile error.
ZLIB_SHA256=9a93b2b7dfdac77ceba5a558a580e74667dd6fede4585b91eefb60f03b72df23

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../../.." && pwd)"
work="${1:-${TMPDIR:-/tmp}/harbour-canary-zlib}"

harbour="$repo/target/debug/harbour"
if [ ! -x "$harbour" ]; then
  harbour="$repo/target/release/harbour"
fi
if [ ! -x "$harbour" ]; then
  echo "no harbour binary; run \`cargo build\` first" >&2
  exit 2
fi

rm -rf "$work"
mkdir -p "$work"
cd "$work"

echo "== fetching zlib $ZLIB_VERSION"
curl -fsSL --retry 3 --retry-delay 2 -o zlib.tar.gz "$ZLIB_URL"

echo "== verifying sha256"
if command -v sha256sum >/dev/null 2>&1; then
  echo "$ZLIB_SHA256  zlib.tar.gz" | sha256sum -c -
else
  echo "$ZLIB_SHA256  zlib.tar.gz" | shasum -a 256 -c -
fi

tar xzf zlib.tar.gz
mv "zlib-$ZLIB_VERSION" upstream

# The shim is committed rather than generated, so a change to it shows up in
# review as a diff. It names every source individually; if the extraction
# above dropped a file, Harbour rejects the manifest instead of quietly
# building a smaller library.
cp "$here/Harbour.toml" upstream/Harbour.toml
cp -r "$here/consumer" consumer

echo "== building zlib"
(cd upstream && "$harbour" build)

# Build it a second time with nothing changed. On a real package this is
# worth asserting on its own: a fingerprint that always reports "dirty"
# costs nothing visible on a three-file fixture and everything on a
# fifteen-source library, and the reverse -- wrongly reporting "clean" --
# is how a stale object survives a rebuild.
echo "== rebuilding zlib (nothing changed)"
rebuild="$(cd upstream && "$harbour" build 2>&1)"
echo "$rebuild"
case "$rebuild" in
  *"up to date"*) ;;
  *) echo "== canary FAILED: second build of zlib reported no cached objects" >&2; exit 1 ;;
esac
if echo "$rebuild" | grep -q "Compiling"; then
  echo "== canary FAILED: second build of zlib recompiled something" >&2
  exit 1
fi

echo "== building and running the consumer"
(cd consumer && "$harbour" build)

exe="consumer/.harbour/target/debug/bin/zlib-canary"
[ -x "$exe" ] || exe="$exe.exe"

# `output=$(...)` under `set -e` would abort here with nothing printed if the
# consumer exited non-zero, which is precisely the case worth reading: the
# consumer reports *which* check failed on stdout. Capture first, report
# always, decide afterwards.
set +e
output="$("$exe" 2>&1)"
status=$?
set -e
echo "$output"
echo "== consumer exited $status"

# Exit status is not enough on its own: every bug this job exists to catch
# produced a successful build. The consumer prints `OK ...` only after the
# round trip and both checksums have matched.
case "$output" in
  OK*) echo "== canary passed" ;;
  *) echo "== canary FAILED: consumer did not report OK" >&2; exit 1 ;;
esac
[ "$status" -eq 0 ] || exit "$status"
