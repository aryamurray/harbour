#!/usr/bin/env bash
# Run every canary, reporting each one's result and failing if any failed.
#
# Runs them all rather than stopping at the first failure, because *which*
# canaries are red is the diagnostic. zlib and cjson red together means
# something broad; zstd alone means the architecture-conditional assembly
# block; libuv alone means per-OS source selection. Stopping early throws
# that away.
#
# Usage: ci/canary/run-all.sh [work-root]
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
work_root="${1:-${TMPDIR:-/tmp}/harbour-canaries}"

# Cheapest and broadest first, so a systemic breakage is reported in seconds
# rather than after two minutes of downloads.
CANARIES=(cjson zlib libuv zstd)

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
