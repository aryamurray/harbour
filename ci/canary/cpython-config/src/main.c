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

int main(void) {
    return (SIZEOF_LONG > 0 && SIZEOF_VOID_P > 0) ? 0 : 1;
}
