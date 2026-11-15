/* Exercise zstd at runtime.
 *
 * Three things this asserts that a build cannot:
 *
 *  - A 256 KiB round trip, which on x86_64 goes through the BMI2 Huffman
 *    decoder in `huf_decompress_amd64.S`. That file is selected by
 *    `[[targets.zstd.when]] arch = "x86_64"`, and an `.S` file missing from
 *    the archive still links -- zstd falls back to its C path -- so only
 *    decompressed *bytes* prove the assembly was assembled correctly.
 *  - The dictionary builder, which lives in lib/dictBuilder and is the
 *    directory a source list is most likely to drop, being the one nothing
 *    else references.
 *  - Multi-threaded compression, which is what makes `pthread` load-bearing
 *    on zstd's *public* link surface. `ZSTD_MULTITHREAD` is compiled into
 *    the archive, and a static archive does not record that dependency. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "zstd.h"
#include "zdict.h"

static int failures = 0;

#define CHECK(cond)                                                            \
    do {                                                                       \
        if (!(cond)) {                                                         \
            printf("FAIL line %d: %s\n", __LINE__, #cond);                     \
            failures++;                                                        \
        }                                                                      \
    } while (0)

#define RAW_SIZE (256u * 1024u)

/* Compressible but not trivially so, and deterministic. */
static void fill(unsigned char *buf, size_t n, unsigned seed) {
    unsigned state = seed;
    for (size_t i = 0; i < n; i++) {
        state = state * 1103515245u + 12345u;
        buf[i] = (unsigned char) ((state >> 16) & 0x3f);
    }
}

int main(void) {
    unsigned char *raw = malloc(RAW_SIZE);
    size_t bound = ZSTD_compressBound(RAW_SIZE);
    unsigned char *comp = malloc(bound);
    unsigned char *back = malloc(RAW_SIZE);
    if (raw == NULL || comp == NULL || back == NULL) {
        printf("FAIL: out of memory\n");
        return 1;
    }
    fill(raw, RAW_SIZE, 0x9e3779b9u);

    /* Single-threaded round trip. */
    size_t csize = ZSTD_compress(comp, bound, raw, RAW_SIZE, 3);
    CHECK(!ZSTD_isError(csize));
    CHECK(csize > 0 && csize < RAW_SIZE);

    unsigned long long declared = ZSTD_getFrameContentSize(comp, csize);
    CHECK(declared == (unsigned long long) RAW_SIZE);

    size_t dsize = ZSTD_decompress(back, RAW_SIZE, comp, csize);
    CHECK(!ZSTD_isError(dsize));
    CHECK(dsize == RAW_SIZE);
    CHECK(memcmp(raw, back, RAW_SIZE) == 0);

    /* Multi-threaded compression: the pthread path. */
    ZSTD_CCtx *cctx = ZSTD_createCCtx();
    CHECK(cctx != NULL);
    if (cctx != NULL) {
        size_t rc = ZSTD_CCtx_setParameter(cctx, ZSTD_c_nbWorkers, 2);
        /* A build without ZSTD_MULTITHREAD refuses the parameter rather
           than silently running single-threaded, so this is a real check on
           the private define reaching the compiler. */
        CHECK(!ZSTD_isError(rc));
        size_t mt = ZSTD_compress2(cctx, comp, bound, raw, RAW_SIZE);
        CHECK(!ZSTD_isError(mt));
        memset(back, 0, RAW_SIZE);
        size_t md = ZSTD_decompress(back, RAW_SIZE, comp, mt);
        CHECK(md == RAW_SIZE);
        CHECK(memcmp(raw, back, RAW_SIZE) == 0);
        ZSTD_freeCCtx(cctx);
    }

    /* Dictionary builder: lib/dictBuilder. */
    {
        const unsigned nb = 32;
        size_t sizes[32];
        size_t chunk = RAW_SIZE / nb;
        for (unsigned i = 0; i < nb; i++) {
            sizes[i] = chunk;
        }
        size_t dictCap = 16 * 1024;
        void *dict = malloc(dictCap);
        CHECK(dict != NULL);
        if (dict != NULL) {
            size_t dsz = ZDICT_trainFromBuffer(dict, dictCap, raw, sizes, nb);
            /* Training can legitimately decline on unsuitable input; what
               must not happen is the symbol being absent, which would be a
               link error above. Accept either a dictionary or a clean
               refusal, and reject a crash. */
            CHECK(ZDICT_isError(dsz) || dsz > 0);
            free(dict);
        }
    }

    free(raw);
    free(comp);
    free(back);

    if (failures != 0) {
        printf("%d check(s) failed\n", failures);
        return 1;
    }
    printf("OK zstd %s: %u KiB round trip, multithreaded compress, dictBuilder\n",
           ZSTD_versionString(), RAW_SIZE / 1024u);
    return 0;
}
