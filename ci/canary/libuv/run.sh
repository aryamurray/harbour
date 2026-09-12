#!/usr/bin/env bash
# libuv 1.51.0 -- the platform-selection canary.
#
# libuv is here because its source list genuinely differs per OS and it has
# no portable fallback: `uv__platform_loop_init` lives in kqueue.c on macOS
# and linux.c on Linux, so a `[[targets.uv.when]]` block that stops matching
# is a link failure rather than a subtly wrong library. The consumer then
# drives a real TCP echo round trip through the selected event loop, because
# "it linked" and "the loop works" are different claims.
#
# 31 unconditional sources plus 6 (macOS) or 4 (Linux) per-OS ones.
#
# Usage: ci/canary/libuv/run.sh [work-dir]
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
. "$here/../lib.sh"

# libuv's own dist tarball, not a GitHub auto-generated archive: it is an
# immutable published artifact.
URL="https://dist.libuv.org/dist/v1.51.0/libuv-v1.51.0.tar.gz"
SHA256=5f0557b90b1106de71951a3c3931de5e0430d78da1d9a10287ebc7a3f78ef8eb

# 37 on macOS (31 + 6), 35 on Linux (31 + 4). Both spelled out rather than
# left unchecked: the whole point of this canary is that the per-OS block
# fired, and a count is what detects it having quietly stopped.
canary_standard_run \
  "$here" "$URL" "$SHA256" "libuv-v1.51.0" "libuv" "libuv-canary" "37|35" \
  "${1:-${TMPDIR:-/tmp}/harbour-canary-libuv}"
