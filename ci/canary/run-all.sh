#!/usr/bin/env bash
# Run every canary, reporting each one's result and failing if any failed.
#
# Runs them all rather than stopping at the first failure, because *which*
# canaries are red is the diagnostic. zlib and cjson red together means
# something broad; zstd alone means the architecture-conditional assembly
# block; libuv alone means per-OS source selection; openssl alone means
# `prebuild` generators or per-platform generator selection. Stopping early
# throws that away.
#
# Usage: ci/canary/run-all.sh [work-root]
#        CANARIES="cjson zlib" ci/canary/run-all.sh [work-root]
#
# `CANARIES` (space-separated names) selects a SUBSET, in the order given, and
# replaces the default list below. Windows runs only the canaries whose shims
# have a Windows source set (see ci/canary/README.md), and a platform that runs
# fewer canaries should say so on its command line rather than carry a second
# copy of this script. Every name must be a canary that exists, and an empty
# selection is an error: a typo that silently ran nothing would report success.
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Default to a per-process directory. `canary_standard_run` does `rm -rf` on
# its work dir, so a shared default means two concurrent runs delete each
# other's trees. That failure does not look like a race: it surfaced as a
# truncated tarball that had already passed its sha256 check, because the
# other run replaced the file in the window between verifying and
# extracting it. Pass an explicit path to keep one.
work_root="${1:-${TMPDIR:-/tmp}/harbour-canaries.$$}"

# `crc32c` and `snappy` are four-file C++ libraries that take about three seconds
# each, so they sit with the cheap ones.
# Cheapest and broadest first, so a systemic breakage is reported in seconds
# rather than after two minutes of downloads. `curl-config` is third because
# it downloads nothing but spends ~100 compiler invocations answering curl's
# configure questions; `cpython-config` follows it and answers ~475 of CPython's,
# measured in parallel. `openssl` and `curl` are last and in that order: they
# are the two most expensive, openssl by download and because it runs
# generators, curl by compute -- 196 translation units on top of 108 probes.
# Putting them at the end means a failure anywhere cheaper is visible before
# either finishes, and a failure in one of these two is specific. `luajit` sits
# before them: it runs a generator that is itself compiled, so a failure there
# alone points at `HARBOUR_HOST_CC` or at a per-platform generator block.
# `cpython` is last: it builds the interpreter three times (feature off, feature
# on, nothing changed), so it is the slowest, and nothing else depends on it.
DEFAULT_CANARIES=(cjson zlib crc32c snappy curl-config cpython-config libuv zstd luajit openssl curl cpython)

if [ -n "${CANARIES:-}" ]; then
  read -r -a SELECTED <<< "$CANARIES"
else
  SELECTED=("${DEFAULT_CANARIES[@]}")
fi
if [ "${#SELECTED[@]}" -eq 0 ]; then
  echo "no canaries selected" >&2
  exit 2
fi
for c in "${SELECTED[@]}"; do
  if [ ! -f "$here/$c/run.sh" ]; then
    echo "unknown canary '$c' (no $here/$c/run.sh); known: ${DEFAULT_CANARIES[*]}" >&2
    exit 2
  fi
done

mkdir -p "$work_root"
declare -a failed=()
declare -a passed=()

for c in "${SELECTED[@]}"; do
  echo
  echo "######## canary: $c ########"
  if bash "$here/$c/run.sh" "$work_root/$c"; then
    passed+=("$c")
  else
    failed+=("$c")
  fi
done

echo
echo "######## summary ########"
for c in "${passed[@]:-}"; do
  [ -n "$c" ] && echo "  pass  $c"
done
for c in "${failed[@]:-}"; do
  [ -n "$c" ] && echo "  FAIL  $c"
done

if [ "${#failed[@]}" -gt 0 ]; then
  echo
  echo "${#failed[@]} canary/canaries failed: ${failed[*]}" >&2
  exit 1
fi
echo
echo "all ${#passed[@]} canaries passed"
