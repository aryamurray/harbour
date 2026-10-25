/* Exercise lz4 at runtime, through all four translation units.
 *
 * - lz4.c       block compression and decompression
 * - lz4hc.c     the high-compression encoder, a separate object
 * - lz4frame.c  the frame format Seastar's RPC and Kafka batches use
 * - xxhash.c    reached only through lz4frame.c's content checksum, so a
 *               shim that dropped it links the library and fails the
 *               frame round trip with an undefined XXH32 symbol
 *
 * A static archive missing a member still archives successfully, which is
 * why each translation unit gets an assertion of its own. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "lz4.h"
#include "lz4frame.h"
#include "lz4hc.h"

static int failures = 0;

#define CHECK(cond)                                                            \
    do {                                                                       \
        if (!(cond)) {                                                         \
            printf("FAIL line %d: %s\n", __LINE__, #cond);                     \
            failures++;                                                        \
        }                                                                      \
    } while (0)

enum { N = 64 * 1024 };

static void fill(char *buf) {
    /* Compressible but not trivially so: a repeating phrase with a counter
     * woven through it. */
    for (int i = 0; i < N; i++) {
        buf[i] = (char)("redpanda-lz4-canary "[i % 19] + (i / 4096));
    }
}

int main(void) {
    char *src = malloc(N);
    char *dst = malloc(LZ4_compressBound(N));
    char *back = malloc(N);
    fill(src);

    CHECK(LZ4_versionNumber() == 10904);

    /* Block API, lz4.c. */
    int clen = LZ4_compress_default(src, dst, N, LZ4_compressBound(N));
    CHECK(clen > 0 && clen < N / 4);
    CHECK(LZ4_decompress_safe(dst, back, clen, N) == N);
    CHECK(memcmp(src, back, N) == 0);

    /* High-compression encoder, lz4hc.c: must do at least as well. */
    char *hc = malloc(LZ4_compressBound(N));
    int hclen = LZ4_compress_HC(src, hc, N, LZ4_compressBound(N), 9);
    CHECK(hclen > 0 && hclen <= clen);
    memset(back, 0, N);
    CHECK(LZ4_decompress_safe(hc, back, hclen, N) == N);
    CHECK(memcmp(src, back, N) == 0);

    /* Frame API with a content checksum, lz4frame.c -> xxhash.c. */
    LZ4F_preferences_t prefs;
    memset(&prefs, 0, sizeof prefs);
    prefs.frameInfo.contentChecksumFlag = LZ4F_contentChecksumEnabled;
    size_t fcap = LZ4F_compressFrameBound(N, &prefs);
    char *frame = malloc(fcap);
    size_t flen = LZ4F_compressFrame(frame, fcap, src, N, &prefs);
    CHECK(!LZ4F_isError(flen));

    LZ4F_dctx *dctx = NULL;
    CHECK(!LZ4F_isError(LZ4F_createDecompressionContext(&dctx, LZ4F_VERSION)));
    memset(back, 0, N);
    size_t dsize = N, ssize = flen;
    size_t r = LZ4F_decompress(dctx, back, &dsize, frame, &ssize, NULL);
    CHECK(!LZ4F_isError(r));
    CHECK(dsize == N && memcmp(src, back, N) == 0);

    /* Corrupt one payload byte: the content checksum must reject it. If
     * xxhash.c were compiled out of the library, this is the assertion that
     * would no longer be reachable. */
    frame[flen / 2] ^= 0x40;
    LZ4F_resetDecompressionContext(dctx);
    dsize = N;
    ssize = flen;
    r = LZ4F_decompress(dctx, back, &dsize, frame, &ssize, NULL);
    CHECK(LZ4F_isError(r));
    LZ4F_freeDecompressionContext(dctx);

    free(src);
    free(dst);
    free(back);
    free(hc);
    free(frame);

    if (failures != 0) {
        printf("%d check(s) failed\n", failures);
        return 1;
    }
    printf("OK lz4 %s: block %d bytes, hc %d bytes, frame %zu bytes\n",
           LZ4_versionString(), clen, hclen, flen);
    return 0;
}
