#!/usr/bin/env bash
# openssl 3.5.4 -- the generated-sources canary.
#
# Every other canary has a source list. openssl does not: its crypto
# primitives are emitted by perlasm at configure time, and 31 of its public
# headers are templates. Nothing is vendored here -- the manifest runs
# openssl's own generators (see `Harbour.toml`), so this canary is also the
# only one that exercises `[[targets.X.prebuild]]` and per-platform
# generators.
#
# It does not use `canary_standard_run`, because a translation-unit count is
# not enough for this package. If an arch `when` block stops matching, the
# assembly is absent, the baseline C is compiled instead, and *the count can
# even stay plausible* while the library silently loses the accelerated
# implementations -- and still computes correct digests. So this asserts, in
# addition to the count:
#
#   * the per-architecture object files exist **by name** on disk;
#   * `sha256_block_data_order` is *undefined* in `sha256.o` on an assembly
#     platform, i.e. the C fallback really was compiled out rather than
#     merely outvoted at link time;
#   * on x86_64, `aes_core.o` does **not** exist -- that platform's `exclude`
#     removed it because `aes-x86_64.s` defines `AES_encrypt` itself.
#
# and the consumer then references arch-only symbols strongly, so a missing
# assembly layer fails the *link*.
#
# Usage: ci/canary/openssl/run.sh [work-dir]
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
. "$here/../lib.sh"

URL="https://github.com/openssl/openssl/releases/download/openssl-3.5.4/openssl-3.5.4.tar.gz"
SHA256=967311f84955316969bdb1d8d4b983718ef42338639c621ec4c34fddef355e99

work="${1:-${TMPDIR:-/tmp}/harbour-canary-openssl}"
repo="$(canary_repo_root "$here")"
harbour="$(canary_harbour "$repo")"

# perl is openssl's own build dependency, and this shim's: it generates the
# assembly and the headers. Say so here rather than failing inside a prebuild
# step with `No such file or directory`.
#
# A hard failure, not a skip. A canary that skips itself when a tool is
# missing is a canary that stops testing and reports success, which is the
# failure mode this whole directory exists to catch. Every runner this
# project uses has perl; openssl's own build has required it since 1998.
if ! command -v perl >/dev/null 2>&1; then
  echo "== canary FAILED: openssl's generators need perl, which is not on PATH" >&2
  echo "   (this shim has no vendored assembly to fall back on -- that is the" >&2
  echo "   deliberate trade; see Harbour.toml)" >&2
  exit 1
fi

rm -rf "$work"
mkdir -p "$work"
cd "$work" || exit 1

canary_fetch "$URL" "$SHA256" "openssl-3.5.4"
cp "$here/Harbour.toml" upstream/Harbour.toml
cp -r "$here/consumer" consumer

# A pristine tarball must not already contain what the manifest claims to
# generate. If openssl ever started shipping these, every generator below
# would be a no-op and this canary would silently stop testing anything.
for f in include/openssl/crypto.h include/openssl/configuration.h configdata.pm; do
  if [ -e "upstream/$f" ]; then
    echo "== canary FAILED: upstream already ships \`$f\`, so the prebuild step" >&2
    echo "   that claims to generate it is no longer under test" >&2
    exit 1
  fi
done
echo "== confirmed: crypto.h, configuration.h and configdata.pm are absent from the tarball"

canary_build_library "$harbour" "openssl"

# 9 portable C files, plus this architecture's assembly layer:
#   aarch64: +5 generated .S +1 armcap.c                             = 15
#   x86_64:  +5 generated .s +cpuid.c +ctype.c -aes_core.c (excluded) = 15
#   anything else: the portable baseline alone                        = 9
canary_expect_objects "15|9"

echo "== objects under $(canary_objdir)"

# The C baseline, on every platform. `canary_require_object` and the symbol
# helpers below come from `lib.sh` and are shared with the zstd canary --
# deliberately, because two hand-maintained copies of "which object defines
# the fast path" would drift, and the copy that drifted would be the one that
# stopped checking.
for o in sha1dgst.o sha256.o sha512.o aes_cbc.o aes_ecb.o \
         aes_misc.o cbc128.o mem_clr.o; do
  canary_require_object "$o"
done

arch="$(uname -m)"
case "$arch" in
  arm64 | aarch64)
    canary_require_object sha1-armv8.o
    canary_require_object sha256-armv8.o
    canary_require_object sha512-armv8.o
    canary_require_object aesv8-armx.o
    canary_require_object arm64cpuid.o
    canary_require_object armcap.o
    # aarch64 keeps the C AES; only x86_64 replaces it.
    canary_require_object aes_core.o
    asm_object=sha256-armv8.o
    ;;
  x86_64 | amd64)
    canary_require_object sha1-x86_64.o
    canary_require_object sha256-x86_64.o
    canary_require_object sha512-x86_64.o
    canary_require_object aes-x86_64.o
    canary_require_object x86_64cpuid.o
    canary_require_object cpuid.o
    # `aes-x86_64.s` defines AES_encrypt itself, so the `exclude` in that
    # `when` block has to have removed the C.
    canary_refuse_object aes_core.o
    asm_object=sha256-x86_64.o

    # The generated *bytes*, not just the object they became.
    #
    # openssl's x86_64 perlasm runs `$ENV{CC}` to ask the assembler which
    # encodings it accepts. With `CC` unset it emits a short file with no
    # AVX2 and no SHA-extension code path -- and that file assembles, links,
    # and computes every digest below correctly, only slower. Measured on
    # 3.5.4, on this project's own macOS host, for both flavours the
    # manifest uses (`wc -c`, then `grep -c 'shaext\|avx2'`):
    #
    #                            CC unset            CC set
    #   elf    sha1-x86_64.s     47,282 /  8     102,156 / 22
    #   elf    sha256-x86_64.s   49,912 /  8      97,936 / 26
    #   elf    sha512-x86_64.s   26,500 /  0      96,962 / 18
    #   macosx sha1-x86_64.s     46,081 /  6     100,232 / 18
    #   macosx sha256-x86_64.s   48,528 /  6      95,158 / 22
    #   macosx sha512-x86_64.s   25,737 /  0      94,321 / 16
    #
    # Note which file is which: `sha512-x86_64.pl` decides whether it emits
    # SHA-256 or SHA-512 code from the *output filename*, so the 49,912 vs
    # 97,936 pair quoted in issue #136 is the sha256 output of that script,
    # not the sha512 one. The sha512 output is the starker case -- 0
    # references either way is not a useful check, but 26,500 vs 96,962
    # bytes is.
    #
    # This is the only assertion in the whole canary that can tell the
    # difference between a generator that was told about its toolchain and
    # one that was not: no object name changes, no translation-unit count
    # changes, and all 17 known-answer checks below still pass. The manifest
    # deliberately sets no `CC` -- it comes from Harbour's generator
    # environment, and if that regresses these numbers halve.
    #
    # The thresholds straddle both flavours with a wide margin on each side
    # (every long form is >= 94,321 bytes, every short form <= 49,912).
    for gen in sha1-x86_64.s sha256-x86_64.s sha512-x86_64.s; do
      path="upstream/crypto/sha/$gen"
      if [ ! -f "$path" ]; then
        echo "== canary FAILED: expected generated assembly at $path" >&2
        exit 1
      fi
      bytes=$(wc -c < "$path" | tr -d ' ')
      isa=$(grep -c 'shaext\|avx2' "$path" || true)
      echo "ok   $gen: $bytes bytes, $isa shaext/avx2 references"
      if [ "$bytes" -lt 90000 ]; then
        echo "== canary FAILED: $gen is the short form ($bytes bytes)." >&2
        echo "   perlasm saw no usable \`CC\`, so it emitted no AVX2 and no" >&2
        echo "   SHA-extension path. That library still passes every digest" >&2
        echo "   check in this canary -- it is just slower, with no other" >&2
        echo "   witness anywhere." >&2
        exit 1
      fi
    done
    # The instruction sets themselves, on the one file that references both.
    isa=$(grep -c 'shaext\|avx2' upstream/crypto/sha/sha256-x86_64.s || true)
    if [ "$isa" -lt 20 ]; then
      echo "== canary FAILED: sha256-x86_64.s has only $isa shaext/avx2" >&2
      echo "   references (expected 22 on macOS, 26 on ELF); the AVX2 and" >&2
      echo "   SHA-extension code paths are missing." >&2
      exit 1
    fi
    ;;
  *)
    echo "== $arch matches no \`when\` block; expecting the portable C baseline"
    canary_require_object aes_core.o
    canary_refuse_object sha256-armv8.o
    canary_refuse_object sha256-x86_64.o
    asm_object=
    ;;
esac

# The sharpest check available, and the one a digest cannot make.
#
# On an assembly platform `SHA256_ASM` is defined, so `crypto/sha/sha256.c`
# must compile *without* its own `sha256_block_data_order` and reference the
# assembly's instead. Drop that one define and the count stays 15, every
# object is still present, every digest is still correct, and the archive
# resolves to whichever definition it saw first. Verified by making exactly
# that change and watching the count pass and this fail.
#
# On a platform matching no block it is the mirror image: nothing else
# implements the block function, so `sha256.c` must.
if [ -n "$asm_object" ]; then
  canary_defines_symbol "$asm_object" sha256_block_data_order
  canary_references_symbol sha256.o sha256_block_data_order
else
  canary_defines_symbol sha256.o sha256_block_data_order
fi

canary_run_consumer "$harbour" "openssl-canary"
echo "== canary passed (openssl)"
