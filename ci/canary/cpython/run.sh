#!/usr/bin/env bash
# CPython 3.14.9 -- the deepest generated-source chain, and an optional
# dependency whose feature changes a generated file.
#
# What it asserts, in the order it asserts it:
#
#   1. The two-stage bootstrap produced all 24 frozen-module headers, and each
#      is **byte-identical** to the one CPython's own build makes (a SHA-256
#      per header, the same on macOS and Linux). A count of files would pass
#      with a bootstrap that froze the wrong source.
#   2. The interpreter Harbour linked runs, reports 3.14.9, and computes.
#   3. With the `zlib` feature off there is no `zlib` module and no
#      `zlibmodule.o`; with it on, `import zlib` round-trips data. The feature
#      changes a *generated* file (`Modules/config.c`), so this is the check
#      that generators see the feature set.
#   4. A build with nothing changed skips both generators. This is the one
#      that matters for cost: the bootstrap is ~190 host compiles, and
#      without `inputs` it ran on every build. (Changing a *feature* still
#      re-runs both: see the note at the second build.)
#
# The standard library is read from the source tree (`PYTHONPATH`): an
# installed layout is out of scope, and a build-tree interpreter that cannot
# find `Lib/` fails at startup, so every run exercises that.
#
# Usage: ci/canary/cpython/run.sh [work-dir]
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
. "$here/../lib.sh"

# python.org's release tarball. The hash is of that file as downloaded; the
# project publishes MD5s and signatures rather than SHA-256.
URL="https://www.python.org/ftp/python/3.14.9/Python-3.14.9.tgz"
SHA256=8b47e85ee67e93945e93620a051a8f44bfc1e9735ed67075d166f4a74b7a060b

# The zlib this canary's optional dependency is built from: the same pinned
# release, and the same shim, as `ci/canary/zlib`. Keep in step with it.
ZLIB_URL="https://github.com/madler/zlib/releases/download/v1.3.1/zlib-1.3.1.tar.gz"
ZLIB_SHA256=9a93b2b7dfdac77ceba5a558a580e74667dd6fede4585b91eefb60f03b72df23

work="${1:-${TMPDIR:-/tmp}/harbour-canary-cpython.$$}"
repo="$(canary_repo_root "$here")"
harbour="$(canary_harbour "$repo")"

# Translation units of the interpreter itself: 190 C files, plus the perf
# trampoline's assembly on Linux. The bootstrap's own host objects live
# elsewhere and are not counted.
case "$(uname -s)" in
  Darwin) BASE_OBJECTS=190 ;;
  *) BASE_OBJECTS=191 ;;
esac

rm -rf "$work"
mkdir -p "$work"
cd "$work"

canary_fetch "$URL" "$SHA256" "Python-3.14.9"
cp -R "$here/overlay/." upstream/

# --- the optional dependency, as a real git repository -----------------------
mkdir zlib-src
(
  cd zlib-src
  canary_fetch "$ZLIB_URL" "$ZLIB_SHA256" "zlib-1.3.1"
  cp "$here/../zlib/Harbour.toml" upstream/Harbour.toml
  cd upstream
  git init -q
  git add -A
  git -c user.name=canary -c user.email=canary@example.com commit -q -m zlib
  git tag v1.3.1
)
sed "s|@ZLIB_REPO@|file://$work/zlib-src/upstream|" "$here/Harbour.toml" > upstream/Harbour.toml

# Lines of $2 matching $1. `grep -c` exits 1 on a count of zero, which under
# `set -e` would end the script silently before the assertion could say why.
count_lines() { grep -c "$1" <<<"$2" || true; }

# The interpreter, with the stdlib taken from the source tree.
run_python() {
  PYTHONHOME=/nonexistent PYTHONPATH="$work/upstream/Lib" \
    "$work/upstream/.harbour/target/debug/bin/python" "$@"
}

# Translation units in the interpreter's own object directory.
python_objects() {
  find upstream/.harbour/target/debug/obj/python -name '*.o' | wc -l | tr -d ' '
}

# The shared `canary_find_object` takes the first directory named `obj` under
# `.harbour`, and once the zlib dependency is built there are two. Pinned to the
# interpreter's own here so the check cannot answer for zlib's objects.
object_present() {
  [ -n "$(find upstream/.harbour/target/debug/obj/python -name "$1" -print -quit)" ]
}

assert_objects() {
  local want=$1 got
  got="$(python_objects)"
  echo "== compiled $got translation unit(s) (expected $want)"
  if [ "$got" != "$want" ]; then
    echo "== canary FAILED: expected $want translation units, got $got" >&2
    exit 1
  fi
}

# All 24 headers, byte for byte.
assert_frozen_headers() {
  local count
  count="$(ls upstream/Python/frozen_modules/*.h | wc -l | tr -d ' ')"
  if [ "$count" != 24 ]; then
    echo "== canary FAILED: bootstrap produced $count frozen headers, expected 24" >&2
    exit 1
  fi
  (
    cd upstream/Python/frozen_modules
    if command -v sha256sum >/dev/null 2>&1; then
      sha256sum -c --quiet "$here/expected-frozen.sha256"
    else
      shasum -a 256 -c --quiet "$here/expected-frozen.sha256"
    fi
  ) || {
    echo "== canary FAILED: a frozen header differs from CPython's own build." >&2
    echo "   The bootstrap ran and wrote it, so a count cannot see this; the" >&2
    echo "   digest is what says it was frozen from the right source." >&2
    exit 1
  }
  echo "ok   24 frozen headers, byte-identical to CPython's own"
}

# Build, keeping the log. On failure show the end of it: a bare "build failed"
# from CI is not something anyone can act on.
build() {
  if ! build_log="$(cd upstream && "$harbour" build 2>&1)"; then
    echo "$build_log" | grep -v INFO | tail -40 >&2
    echo "== canary FAILED: harbour build failed" >&2
    exit 1
  fi
}

# --- build 1: feature off -----------------------------------------------------
echo "== building cpython (zlib off)"
t0=$(date +%s)
build
echo "== cold build: $(( $(date +%s) - t0 ))s"

assert_frozen_headers
assert_objects "$BASE_OBJECTS"
object_present zlibmodule.o && {
  echo "== canary FAILED: zlibmodule.o was compiled with the feature off" >&2
  exit 1
}
echo "ok   no zlibmodule.o with the feature off"

out="$(run_python -c "import sys; print(sys.version.split()[0], sum(i * i for i in range(1000)))")"
echo "$out"
[ "$out" = "3.14.9 332833500" ] || { echo "== canary FAILED: interpreter said '$out'" >&2; exit 1; }

if run_python -c "import zlib" 2>zlib-off.err; then
  echo "== canary FAILED: zlib imported with the feature off" >&2
  exit 1
fi
grep -q "No module named 'zlib'" zlib-off.err || {
  echo "== canary FAILED: expected 'No module named zlib', got:" >&2
  cat zlib-off.err >&2
  exit 1
}
echo "ok   zlib absent with the feature off"

# --- build 2: feature on ------------------------------------------------------
# `harbour build` has no --features, so the root package's come from `default`.
sed -i.bak 's/^default = \[\]/default = ["zlib"]/' upstream/Harbour.toml
echo "== building cpython (zlib on)"
t0=$(date +%s)
build
log="$build_log"
echo "== rebuild with the feature: $(( $(date +%s) - t0 ))s"

# Both generators re-run here, and that is a known limitation rather than a
# result to assert on: a step's skip digest covers its whole environment, and
# every generator's environment carries HARBOUR_FEATURES, so a feature change
# invalidates every `inputs`-declaring step in the package -- including the
# bootstrap, which does not read it. What must hold is the correctness the
# `config.c` check above already proves: the module table *was* regenerated.
# Reported so a future change that makes this selective shows up in the log.
echo "== generators on enabling the feature: ran $(count_lines "Running pre-build step" "$log"), skipped $(count_lines "Skipping pre-build step" "$log")"

grep -q '"zlib", PyInit_zlib' upstream/generated/config.c || {
  echo "== canary FAILED: config.c does not list zlib; the generator did not see the feature" >&2
  exit 1
}
assert_frozen_headers
assert_objects "$(( BASE_OBJECTS + 1 ))"
object_present zlibmodule.o || {
  echo "== canary FAILED: zlibmodule.o was not compiled with the feature on" >&2
  exit 1
}
echo "ok   zlibmodule.o compiled with the feature on"

out="$(run_python -c "
import zlib
data = b'harbour ' * 1000
packed = zlib.compress(data, 9)
assert zlib.decompress(packed) == data
assert len(packed) < len(data) // 20
print('zlib', zlib.ZLIB_VERSION, len(data), '->', len(packed))
")"
echo "$out"
case "$out" in
  "zlib 1.3.1 8000 -> "*) ;;
  *) echo "== canary FAILED: zlib said '$out'" >&2; exit 1 ;;
esac

# --- build 3: nothing changed -------------------------------------------------
echo "== rebuilding cpython (nothing changed)"
t0=$(date +%s)
log="$(cd upstream && "$harbour" build 2>&1)"
echo "== no-op build: $(( $(date +%s) - t0 ))s"
skipped="$(count_lines "Skipping pre-build step" "$log")"
if [ "$skipped" != 2 ] || echo "$log" | grep -q "Running pre-build step"; then
  echo "$log" | grep -E "pre-build step" >&2
  echo "== canary FAILED: expected both generators skipped on a build with nothing changed" >&2
  exit 1
fi
if echo "$log" | grep -q "Compiling"; then
  echo "== canary FAILED: a build with nothing changed recompiled something" >&2
  exit 1
fi
echo "ok   both generators skipped, nothing recompiled"

echo "== canary passed (cpython)"
