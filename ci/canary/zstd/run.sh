#!/usr/bin/env bash
# zstd 1.5.7 -- the mixed-language, multi-directory canary.
#
# 37 C files across five directories plus one `.S` file selected by
# `[[targets.zstd.when]] arch = "x86_64"`. That assembly file is the reason
# this canary asserts *decompressed bytes* rather than exit status -- and the
# reason it also asserts symbols. An archive that lost the assembly still
# links and still round-trips byte-exactly, because zstd falls back to its C
# Huffman path, so the decompressed bytes cannot tell you which loop ran.
# `canary_extra_assertions` below is what can, and the comment there names
# the two breakages that proved the bytes and the count are not enough.
#
# Usage: ci/canary/zstd/run.sh [work-dir]
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
. "$here/../lib.sh"

# zstd's published release asset, not a GitHub auto-generated archive.
URL="https://github.com/facebook/zstd/releases/download/v1.5.7/zstd-1.5.7.tar.gz"
SHA256=eb33e51f49a15e023950cd7825ca74a4a2b43db8354825ac24fc1b7ee09e6fa3

# The Huffman loop `huf_decompress_amd64.S` implements, and that
# `huf_decompress.c` calls through a function pointer when
# `ZSTD_ENABLE_ASM_X86_64_BMI2` is on.
ASM_SYM=HUF_decompress4X1_usingDTable_internal_fast_asm_loop

# Symbol-level assertions, run between the object count and the consumer.
#
# Added because the round trip and the count are both blind to the failure
# that matters here, demonstrated by breaking this manifest in two ways:
#
#   * Adding `ZSTD_DISABLE_ASM` to the x86_64 defines. The whole body of
#     `huf_decompress_amd64.S` is inside `#if ZSTD_ENABLE_ASM_X86_64_BMI2`,
#     so it assembles to an **empty object**: still 38 translation units,
#     `huf_decompress_amd64.o` still present, every round trip still
#     byte-exact, and the accelerated Huffman loop simply gone. Only
#     `canary_defines_symbol` sees it.
#   * Deleting the `.S` from the `arch = "x86_64"` block. The count drops to
#     37 -- which the `"38|37"` set *accepts*, because 37 is legitimate on
#     aarch64 -- so the count check passes there too, and only
#     `canary_require_object` catches it.
#
# The aarch64 side is asserted as the mirror image rather than skipped: the
# object must be absent and `huf_decompress.o` must not mention the symbol at
# all, since zstd's own guard tests `defined(__x86_64__)`. That is what would
# catch the manifest growing an `arch`-less assembly source.
canary_extra_assertions() {
  case "$(uname -m)" in
    x86_64 | amd64)
      canary_require_object huf_decompress_amd64.o
      canary_defines_symbol huf_decompress_amd64.o "$ASM_SYM"
      canary_references_symbol huf_decompress.o "$ASM_SYM"
      ;;
    *)
      canary_refuse_object huf_decompress_amd64.o
      canary_lacks_symbol huf_decompress.o "$ASM_SYM"
      ;;
  esac
}

# 38 on x86_64 (37 C + 1 assembly), 37 everywhere else. The difference *is*
# the feature under test, so both are spelled out; a single number would
# either fail on one architecture or stop checking.
canary_standard_run \
  "$here" "$URL" "$SHA256" "zstd-1.5.7" "zstd" "zstd-canary" "38|37" \
  "${1:-${TMPDIR:-/tmp}/harbour-canary-zstd}"
