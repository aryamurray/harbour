#!/usr/bin/env bash
# snappy 1.2.1 -- the C++ canary for the Redpanda feasibility study.
#
# Four C++ sources, a generated public header (carried in `overlay/`), and a
# `cmake/config.h.in` made entirely of `#cmakedefine01` macros the sources
# test with `#if`. An undefined macro is 0 there, so a shim that answered none
# of upstream's questions still compiles and still round-trips every byte --
# on fallback code that skips `__builtin_expect`, prefetch and the
# count-trailing-zeros intrinsic. The decompressed bytes cannot tell the two
# builds apart, which is why `canary_extra_assertions` reads the compile
# commands Harbour wrote instead.
#
# Usage: ci/canary/snappy/run.sh [work-dir]
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
. "$here/../lib.sh"

# The auto-generated GitHub archive: google/snappy publishes no release asset.
# Pinned by hash, as for cJSON.
URL="https://github.com/google/snappy/archive/refs/tags/1.2.1.tar.gz"
SHA256=736aeb64d86566d2236ddffa2865ee5d7a82d26c9016b36218fcc27ea4f09f86

# The compile command Harbour recorded for `snappy.cc`, one line.
snappy_command() {
  python3 -I - upstream/.harbour/compile_commands.json <<'PY'
import json, sys
for entry in json.load(open(sys.argv[1])):
    if entry["file"].endswith("/snappy.cc"):
        print(entry.get("command") or " ".join(entry["arguments"]))
        break
PY
}

# Every fact the shim asserts about the compiler must reach the compiler.
# `HAVE_BUILTIN_EXPECT` and its three siblings are the ones that vanish
# silently: the library still builds and every byte still round-trips.
canary_extra_assertions() {
  local cmd
  cmd="$(snappy_command)"
  [ -n "$cmd" ] || {
    echo "== canary FAILED: no compile command recorded for snappy.cc" >&2
    exit 1
  }
  local want
  for want in HAVE_BUILTIN_EXPECT=1 HAVE_BUILTIN_CTZ=1 HAVE_BUILTIN_PREFETCH=1 \
    HAVE_ATTRIBUTE_ALWAYS_INLINE=1 HAVE_FUNC_MMAP=1 HAVE_FUNC_SYSCONF=1 \
    HAVE_SYS_MMAN_H=1 HAVE_UNISTD_H=1; do
    case " $cmd " in
      *" -D$want "*) echo "ok   snappy.cc is compiled with -D$want" ;;
      *)
        echo "== canary FAILED: snappy.cc is not compiled with -D$want." >&2
        echo "   The library still builds and still round-trips; it is running" >&2
        echo "   on the portable fallback for whatever this macro selects." >&2
        echo "   command: $cmd" >&2
        exit 1
        ;;
    esac
  done
  # NEON is the aarch64 baseline; x86_64 must NOT claim an ISA the shim does
  # not pass a -m flag for, or the binary would fault on an older CPU.
  case "$(uname -m)" in
    arm64 | aarch64)
      case " $cmd " in
        *" -DSNAPPY_HAVE_NEON=1 "*) echo "ok   snappy.cc is compiled with -DSNAPPY_HAVE_NEON=1" ;;
        *)
          echo "== canary FAILED: aarch64 build lacks -DSNAPPY_HAVE_NEON=1" >&2
          exit 1
          ;;
      esac
      ;;
    *)
      case " $cmd " in
        *" -DSNAPPY_HAVE_SSSE3"* | *" -DSNAPPY_HAVE_BMI2"* | *" -DSNAPPY_HAVE_X86_CRC32"* | *" -DSNAPPY_HAVE_NEON"*)
          echo "== canary FAILED: build claims an ISA feature the shim passes no flag for" >&2
          echo "   command: $cmd" >&2
          exit 1
          ;;
        *) echo "ok   snappy.cc claims no ISA extension beyond the baseline" ;;
      esac
      ;;
  esac
}

canary_standard_run \
  "$here" "$URL" "$SHA256" "snappy-1.2.1" "snappy" "snappy-canary" "4" \
  "${1:-${TMPDIR:-/tmp}/harbour-canary-snappy.$$}"
