#!/usr/bin/env bash
# cJSON 1.7.18 -- the control canary.
#
# Two sources, headers at the package root, a public `system_libs = ["m"]`
# the consumer's float assertions actually need, and nothing conditional. Its
# job is to fail only when something broad is broken, which is what makes a
# red cjson meaningfully different from a red libuv or zstd.
#
# Usage: ci/canary/cjson/run.sh [work-dir]
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
. "$here/../lib.sh"

# Only the auto-generated GitHub archive exists for cJSON -- there is no
# release asset. Pinned by hash regardless: if GitHub ever changes its
# archive generation again, this goes red with a checksum mismatch rather
# than with a mystery compile error.
URL="https://github.com/DaveGamble/cJSON/archive/refs/tags/v1.7.18.tar.gz"
SHA256=3aa806844a03442c00769b83e99970be70fbef03735ff898f4811dd03b9f5ee5

canary_standard_run \
  "$here" "$URL" "$SHA256" "cJSON-1.7.18" "cjson" "cjson-canary" "2" \
  "${1:-${TMPDIR:-/tmp}/harbour-canary-cjson}"
