#!/usr/bin/env bash
# crc32c 1.1.0 -- the per-source-flags canary for the Redpanda feasibility study.
#
# Redpanda checksums every Kafka batch with this library. Its own build keeps
# two translation units (`crc32c_sse42.cc`, `crc32c_arm64.cc`) apart from the
# rest because they need an ISA flag the portable code must not have, and
# Harbour has no per-source flags, so the shim splits them into sibling
# targets (see Harbour.toml). This canary exists to prove that split still
# produces the right objects.
#
# The CRC values cannot show that: the portable and accelerated paths compute
# the same function, so a build whose `arch` blocks never matched (every
# `HAVE_*` define gone at once) still returns every correct checksum -- just
# slowly. Demonstrated by renaming `aarch64` to `aarch65` in the manifest: the
# build succeeds and the consumer prints OK. Losing the define from only the
# ISA target is louder (the dispatcher then references a symbol nobody
# defines, and the link fails). The quiet case is the same shape as zstd's
# assembly Huffman loop and needs the same remedy: assert which object
# *defines* the accelerated entry point.
#
# Usage: ci/canary/crc32c/run.sh [work-dir]
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
. "$here/../lib.sh"

# The auto-generated GitHub archive: google/crc32c publishes no release asset.
URL="https://github.com/google/crc32c/archive/refs/tags/1.1.0.tar.gz"
SHA256=49de137bf1c2eb6268d5122674f7dd1524b9148ba65c7b85c5ae4b9be104a25a

SSE42_SYM='_ZN6crc32c10ExtendSse42EjPKhm'
ARM64_SYM='_ZN6crc32c11ExtendArm64EjPKhm'

# The compile command Harbour recorded for `crc32c.cc`, one line.
crc32c_command() {
  python3 -I - upstream/.harbour/compile_commands.json <<'PY'
import json, sys
for entry in json.load(open(sys.argv[1])):
    if entry["file"].endswith("/crc32c.cc"):
        print(entry.get("command") or " ".join(entry["arguments"]))
        break
PY
}

# `HAVE_STRONG_GETAUXVAL` is the one answer here that really differs per OS,
# and it is a *measured* one (`probes.named`). On aarch64 Linux it is what lets
# `CanUseArm64Linux()` ask the kernel whether the CPU has CRC32 instructions;
# without it that function returns false, the accelerated object is linked and
# never called, and every checksum is still correct. macOS has no
# <sys/auxv.h>, so there the macro must be *absent* -- a probe that answered
# yes on a Mac would be as wrong as one that answered no on Linux.
canary_getauxval_assertion() {
  local cmd
  cmd="$(crc32c_command)"
  [ -n "$cmd" ] || {
    echo "== canary FAILED: no compile command recorded for crc32c.cc" >&2
    exit 1
  }
  case "$(uname -s)-$(uname -m)" in
    Linux-aarch64 | Linux-arm64)
      case " $cmd " in
        *" -DHAVE_STRONG_GETAUXVAL=1 "*) echo "ok   crc32c.cc is compiled with -DHAVE_STRONG_GETAUXVAL=1" ;;
        *)
          echo "== canary FAILED: aarch64 Linux build lacks -DHAVE_STRONG_GETAUXVAL=1;" >&2
          echo "   the accelerated CRC would be linked and never called." >&2
          echo "   command: $cmd" >&2
          exit 1
          ;;
      esac
      ;;
    Darwin-*)
      case " $cmd " in
        *"HAVE_STRONG_GETAUXVAL"*)
          echo "== canary FAILED: a macOS build claims getauxval, which it does not have" >&2
          exit 1
          ;;
        *) echo "ok   crc32c.cc does not claim getauxval on macOS" ;;
      esac
      ;;
  esac
}

# On each architecture exactly one accelerated object holds code and the
# other is an empty translation unit. Both are asserted: the live one defines
# its entry point, and the dead one does not, which is what catches an `arch`
# block that matches the wrong machine (an ISA flag leaking onto a target that
# runs on CPUs without it).
canary_extra_assertions() {
  canary_require_object crc32c_sse42.o
  canary_require_object crc32c_arm64.o
  case "$(uname -m)" in
    x86_64 | amd64)
      canary_defines_symbol crc32c_sse42.o "$SSE42_SYM"
      canary_lacks_symbol crc32c_arm64.o "$ARM64_SYM"
      canary_references_symbol crc32c.o "$SSE42_SYM"
      ;;
    arm64 | aarch64)
      canary_defines_symbol crc32c_arm64.o "$ARM64_SYM"
      canary_lacks_symbol crc32c_sse42.o "$SSE42_SYM"
      canary_references_symbol crc32c.o "$ARM64_SYM"
      ;;
  esac
  canary_getauxval_assertion
}

# Four translation units on every architecture: the dispatcher, the portable
# code and the two ISA groups, one of which is empty.
canary_standard_run \
  "$here" "$URL" "$SHA256" "crc32c-1.1.0" "crc32c" "crc32c-canary" "4" \
  "${1:-${TMPDIR:-/tmp}/harbour-canary-crc32c.$$}"
