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
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
work_root="${1:-${TMPDIR:-/tmp}/harbour-canaries}"

# Cheapest and broadest first, so a systemic breakage is reported in seconds
# rather than after two minutes of downloads. `curl-config` is third because
# it downloads nothing but spends ~100 compiler invocations answering curl's
# configure questions. `openssl` and `curl` are last and in that order: they
# are the two most expensive, openssl by download and because it runs
# generators, curl by compute -- 196 translation units on top of 108 probes.
# Putting them at the end means a failure anywhere cheaper is visible before
# either finishes, and a failure in one of these two is specific.
CANARIES=(cjson zlib curl-config libuv zstd openssl curl)

mkdir -p "$work_root"
declare -a failed=()
declare -a passed=()

for c in "${CANARIES[@]}"; do
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
