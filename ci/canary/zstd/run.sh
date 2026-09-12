#!/usr/bin/env bash
# zstd 1.5.7 -- the mixed-language, multi-directory canary.
#
# 37 C files across five directories plus one `.S` file selected by
# `[[targets.zstd.when]] arch = "x86_64"`. That assembly file is the reason
# this canary asserts *decompressed bytes* rather than exit status: an `.S`
# missing from the archive still links, because zstd falls back to its C
# Huffman path, so the only witness that the assembly was assembled is the
# round trip producing the original buffer.
#
# Usage: ci/canary/zstd/run.sh [work-dir]
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
. "$here/../lib.sh"

# zstd's published release asset, not a GitHub auto-generated archive.
URL="https://github.com/facebook/zstd/releases/download/v1.5.7/zstd-1.5.7.tar.gz"
SHA256=eb33e51f49a15e023950cd7825ca74a4a2b43db8354825ac24fc1b7ee09e6fa3

# 38 on x86_64 (37 C + 1 assembly), 37 everywhere else. The difference *is*
# the feature under test, so both are spelled out; a single number would
# either fail on one architecture or stop checking.
canary_standard_run \
  "$here" "$URL" "$SHA256" "zstd-1.5.7" "zstd" "zstd-canary" "38|37" \
  "${1:-${TMPDIR:-/tmp}/harbour-canary-zstd}"
