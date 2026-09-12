# Shared canary machinery. Sourced by each `ci/canary/<pkg>/run.sh`.
#
# Every canary does the same five things: fetch a pinned upstream tarball,
# drop a committed manifest into it, build the library, assert a second build
# reuses everything, then build and *run* a consumer that reports `OK`.
#
# One implementation rather than one per package, for the reason this repo
# keeps rediscovering: the 2026-09-07 schema audit found ten defects and every
# one of them was a single fact with two hand-maintained consumers that had
# drifted. Four copies of "assert the rebuild was a no-op" would drift the
# same way, and the copy that drifted would be the one that stopped checking.
#
# Why the manifests are committed rather than generated: a change to one then
# shows up in review as a diff. Why upstream sources are *not* committed: a
# vendored tarball is a fork nobody remembers taking. The manifests name every
# source individually, so if an extraction drops a file Harbour rejects the
# manifest instead of quietly building a smaller library.

set -euo pipefail

# Resolve the repo root and the harbour binary once.
#
# `$1` is a canary's own directory, `ci/canary/<pkg>`, so the repo is three
# levels up.
canary_repo_root() {
  (cd "$1/../../.." && pwd)
}

canary_harbour() {
  local repo=$1 harbour
  harbour="$repo/target/debug/harbour"
  if [ ! -x "$harbour" ]; then
    harbour="$repo/target/release/harbour"
  fi
  if [ ! -x "$harbour" ]; then
    echo "no harbour binary; run \`cargo build\` first" >&2
    exit 2
  fi
  echo "$harbour"
}

# sha256, portably. Linux runners have sha256sum; macOS has shasum.
canary_verify_sha256() {
  local file=$1 want=$2
  if command -v sha256sum >/dev/null 2>&1; then
    echo "$want  $file" | sha256sum -c -
  else
    echo "$want  $file" | shasum -a 256 -c -
  fi
}

# Fetch, verify and extract, leaving the tree at `$work/upstream`.
#
# Pinned by hash, not just by version tag. A canary that silently follows
# whatever upstream publishes turns a supply-chain change into a Harbour bug
# report, and an interrupted download into a mystery compile error.
canary_fetch() {
  local url=$1 sha=$2 topdir=$3
  echo "== fetching $url"
  curl -fsSL --retry 3 --retry-delay 2 -o src.tar.gz "$url"
  echo "== verifying sha256"
  canary_verify_sha256 src.tar.gz "$sha"
  tar xzf src.tar.gz
  if [ ! -d "$topdir" ]; then
    echo "== canary FAILED: expected \`$topdir/\` in the tarball; found:" >&2
    tar tzf src.tar.gz | head -3 >&2
    exit 1
  fi
  mv "$topdir" upstream
}

# Build the library, then build it again and require that nothing recompiled.
#
# The second build is worth asserting on its own. A fingerprint that always
# reports "dirty" costs nothing visible on a three-file fixture and everything
# on a thirty-source library; the reverse -- wrongly reporting "clean" -- is
# how a stale object survives a rebuild. Asserted on the build log *and* on
# the absence of `Compiling`, because `Compiling 3 file(s) (0 up to date)`
# contains the string "up to date" even when everything was recompiled.
canary_build_library() {
  local harbour=$1 name=$2
  echo "== building $name"
  (cd upstream && "$harbour" build)

  echo "== rebuilding $name (nothing changed)"
  local rebuild
  rebuild="$(cd upstream && "$harbour" build 2>&1)"
  echo "$rebuild"
  case "$rebuild" in
    *"up to date"*) ;;
    *) echo "== canary FAILED: second build of $name reported no cached objects" >&2; exit 1 ;;
  esac
  if echo "$rebuild" | grep -q "Compiling"; then
    echo "== canary FAILED: second build of $name recompiled something" >&2
    exit 1
  fi
}

# Build the consumer and run it, requiring `OK` on stdout.
#
# Exit status is not enough on its own: every bug these jobs exist to catch
# produced a *successful build*. The consumer prints `OK ...` only after its
# assertions have actually run.
canary_run_consumer() {
  local harbour=$1 exe_name=$2
  echo "== building and running the consumer"
  (cd consumer && "$harbour" build)

  local exe="consumer/.harbour/target/debug/bin/$exe_name"
  [ -x "$exe" ] || exe="$exe.exe"

  # `output=$(...)` under `set -e` would abort here with nothing printed if
  # the consumer exited non-zero, which is precisely the case worth reading:
  # the consumer reports *which* check failed on stdout. Capture first,
  # report always, decide afterwards.
  set +e
  local output status
  output="$("$exe" 2>&1)"
  status=$?
  set -e
  echo "$output"
  echo "== consumer exited $status"

  case "$output" in
    OK*) ;;
    *) echo "== canary FAILED: consumer did not report OK" >&2; exit 1 ;;
  esac
  [ "$status" -eq 0 ] || exit "$status"
}

# How many object files the library build produced.
#
# A count, not a list: the interesting failure is a `[[targets.X.when]]`
# block that stopped matching, which shows up as a library that links, passes
# its consumer, and is missing a platform's event loop or its assembly fast
# path. `harbour build` reports "Compiling N file(s)" only on the first
# build, so this reads the tree instead.
canary_object_count() {
  find upstream/.harbour -name '*.o' -o -name '*.obj' 2>/dev/null | wc -l | tr -d ' '
}

# Assert the translation-unit count, so a silently-shrinking source list is
# visible. `expected` may be a `|`-separated set when it legitimately differs
# per platform (zstd compiles one extra assembly file on x86_64).
canary_expect_objects() {
  local expected=$1 got
  got="$(canary_object_count)"
  echo "== compiled $got translation unit(s) (expected $expected)"
  case "|$expected|" in
    *"|$got|"*) ;;
    *)
      echo "== canary FAILED: compiled $got translation units, expected $expected." >&2
      echo "   A source list that silently shrank still links and still passes" >&2
      echo "   its consumer; that is why this is asserted separately." >&2
      exit 1
      ;;
  esac
}

# The standard body of a canary: everything above, in order.
canary_standard_run() {
  local here=$1 url=$2 sha=$3 topdir=$4 name=$5 exe=$6 objects=$7 work=$8
  local repo harbour
  repo="$(canary_repo_root "$here")"
  harbour="$(canary_harbour "$repo")"

  rm -rf "$work"
  mkdir -p "$work"
  cd "$work"

  canary_fetch "$url" "$sha" "$topdir"
  cp "$here/Harbour.toml" upstream/Harbour.toml
  cp -r "$here/consumer" consumer

  canary_build_library "$harbour" "$name"
  canary_expect_objects "$objects"
  canary_run_consumer "$harbour" "$exe"
  echo "== canary passed ($name)"
}
