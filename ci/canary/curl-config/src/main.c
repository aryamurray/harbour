/* curl's own sources include curl_config.h by this name and nothing else;
   this stands in for them. The file existing and compiling is the first
   assertion -- `run.sh` then compares every answer in it against curl's. */
#include "curl_config.h"

/* A handful of answers curl's code cannot build without, asserted here so a
   header that generated but came out empty fails the build rather than the
   diff. */
#ifndef SIZEOF_LONG
#error "SIZEOF_LONG missing from the generated config"
#endif
#ifndef SIZEOF_SIZE_T
#error "SIZEOF_SIZE_T missing from the generated config"
#endif

int main(void) {
    /* Reference them so an empty header cannot pass by being unused. */
    return (SIZEOF_LONG > 0 && SIZEOF_SIZE_T > 0) ? 0 : 1;
}
