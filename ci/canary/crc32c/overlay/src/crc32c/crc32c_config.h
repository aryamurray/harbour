// Harbour canary overlay: stands in for the `crc32c_config.h` that upstream's
// cmake generates from `src/crc32c_config.h.in`.
//
// Upstream's template is eight `#cmakedefine01` lines: each macro is defined
// as 0 or 1 and the sources test it with `#if`. Harbour has no template step,
// and a vendored per-platform copy of the *answers* is what its probes exist
// to replace. So this header carries only the part no probe can express:
// "define it as 0 unless the build already said otherwise". The answers
// arrive as -D flags from the manifest (`-DHAVE_SSE42=1` on x86_64, and so
// on), and this header turns every unanswered question into an explicit 0.
//
// It matters that they are explicit rather than left undefined: an undefined
// macro is 0 in `#if` too, but `-Wundef` builds, and a reader grepping for
// what a build decided, both need the question to have an answer.
#ifndef CRC32C_CRC32C_CONFIG_H_
#define CRC32C_CRC32C_CONFIG_H_

#ifndef BYTE_ORDER_BIG_ENDIAN
#define BYTE_ORDER_BIG_ENDIAN 0
#endif

#ifndef HAVE_BUILTIN_PREFETCH
#define HAVE_BUILTIN_PREFETCH 0
#endif

#ifndef HAVE_MM_PREFETCH
#define HAVE_MM_PREFETCH 0
#endif

#ifndef HAVE_SSE42
#define HAVE_SSE42 0
#endif

#ifndef HAVE_ARM64_CRC32C
#define HAVE_ARM64_CRC32C 0
#endif

#ifndef HAVE_STRONG_GETAUXVAL
#define HAVE_STRONG_GETAUXVAL 0
#endif

#ifndef HAVE_WEAK_GETAUXVAL
#define HAVE_WEAK_GETAUXVAL 0
#endif

#ifndef CRC32C_TESTS_BUILT_WITH_GLOG
#define CRC32C_TESTS_BUILT_WITH_GLOG 0
#endif

#endif  // CRC32C_CRC32C_CONFIG_H_
