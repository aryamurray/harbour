/* CPython's sources include pyconfig.h by that name and nothing else; this
   stands in for them. The header existing and compiling is the first
   assertion -- `run.sh` then compares every answer in it against CPython's own
   configure. */
#include "pyconfig.h"

/* Answers CPython cannot build without, asserted here so a header that
   generated but came out empty fails the build rather than the diff. */
#ifndef SIZEOF_LONG
#error "SIZEOF_LONG missing from the generated config"
#endif
#ifndef SIZEOF_VOID_P
#error "SIZEOF_VOID_P missing from the generated config"
#endif
#ifndef HAVE_UNISTD_H
#error "HAVE_UNISTD_H missing from the generated config"
#endif

/* The header must agree with the compiler about the machine. This is the check a
   textual comparison with configure cannot make: configure's own header has
   `WORDS_BIGENDIAN` inside `#if defined AC_APPLE_UNIVERSAL_BUILD`, a name-by-name
   reading calls that "defined", and a header built from it claims a big-endian
   machine on little-endian arm64 and x86_64 (this canary asserted exactly that). */
#if defined(__BYTE_ORDER__) && defined(__ORDER_BIG_ENDIAN__)
#  if __BYTE_ORDER__ == __ORDER_BIG_ENDIAN__
#    ifndef WORDS_BIGENDIAN
#      error "this is a big-endian target but the generated config does not define WORDS_BIGENDIAN"
#    endif
#  else
#    ifdef WORDS_BIGENDIAN
#      error "this is a little-endian target but the generated config defines WORDS_BIGENDIAN"
#    endif
#  endif
#endif

#ifdef SIZEOF_INT
_Static_assert(sizeof(int) == SIZEOF_INT, "SIZEOF_INT disagrees with the compiler");
#endif
#ifdef SIZEOF_LONG
_Static_assert(sizeof(long) == SIZEOF_LONG, "SIZEOF_LONG disagrees with the compiler");
#endif
#ifdef SIZEOF_LONG_LONG
_Static_assert(sizeof(long long) == SIZEOF_LONG_LONG, "SIZEOF_LONG_LONG disagrees with the compiler");
#endif
#ifdef SIZEOF_VOID_P
_Static_assert(sizeof(void *) == SIZEOF_VOID_P, "SIZEOF_VOID_P disagrees with the compiler");
#endif
#ifdef SIZEOF_SIZE_T
_Static_assert(sizeof(__SIZE_TYPE__) == SIZEOF_SIZE_T, "SIZEOF_SIZE_T disagrees with the compiler");
#endif
#ifdef SIZEOF_SHORT
_Static_assert(sizeof(short) == SIZEOF_SHORT, "SIZEOF_SHORT disagrees with the compiler");
#endif

int main(void) {
    return (SIZEOF_LONG > 0 && SIZEOF_VOID_P > 0) ? 0 : 1;
}
