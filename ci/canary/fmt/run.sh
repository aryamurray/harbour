#!/usr/bin/env bash
# {fmt} 11.0.2 -- the C++ canary.
#
# Every other canary is C. This one is a compiled C++ library (two `.cc`
# files) with a consumer that is itself mixed C and C++, so it is the only
# place in CI that exercises, end to end: a C++ compile at a declared
# standard, archiving C++ objects, the resolver's C++ standard unification
# across a path dependency, and -- the sharp one -- *which driver links the
# program*. `main.cc` needs the C++ runtime; `legacy.c` does not. A C driver
# without `-lstdc++` fails on `__cxa_*` and vtable symbols, not quietly.
#
# Usage: ci/canary/fmt/run.sh [work-dir]
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
. "$here/../lib.sh"

# fmt's published release asset. It is a .zip (there is no .tar.gz asset, and
# GitHub's auto-generated archive is a different file), which `canary_fetch`
# extracts with `unzip`.
URL="https://github.com/fmtlib/fmt/releases/download/11.0.2/fmt-11.0.2.zip"
SHA256=40fc58bebcf38c759e11a7bd8fdc163507d2423ef5058bba7f26280c5b9c5465

# The consumer asks for C++20 and fmt's target declares 17. Standalone, fmt
# builds at 17 (the library log above); inside the consumer's graph the one
# C++ standard is the maximum, so fmt's two objects and the consumer's
# `main.cc` all compile at 20 -- three `-std=c++20` -- while `legacy.c` is
# compiled as C and carries no `-std=c++` at all. If the unification stopped
# raising fmt, the count would be 1; if the C file were compiled as C++ it
# would be 4.
canary_consumer_assertions() {
  local n
  n="$(grep -c '"-std=c++20"' consumer/.harbour/compile_commands.json || true)"
  echo "== -std=c++20 on $n translation unit(s) in the consumer graph (expected 3)"
  if [ "$n" != 3 ]; then
    echo "== canary FAILED: the graph's C++ standard did not reach exactly fmt's" >&2
    echo "   two objects and main.cc (and not the C file)." >&2
    exit 1
  fi
}

canary_standard_run \
  "$here" "$URL" "$SHA256" "fmt-11.0.2" "fmt" "fmt-canary" "2" \
  "${1:-${TMPDIR:-/tmp}/harbour-canary-fmt.$$}"
