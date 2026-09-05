/* Compress a buffer, decompress it again, and verify the round trip.
 *
 * The point of the canary is behaviour, not exit status: every bug that
 * motivated it produced a build that succeeded and a library that computed
 * the wrong thing. So this calls into zlib, checks its return codes,
 * compares the recovered bytes with the originals, and prints a line the CI
 * job greps for. A stale object left in the archive, a source silently
 * skipped, or a link input dropped between the surface and the linker all
 * show up here as a wrong answer or a missing symbol.
 */
#include <stdio.h>
#include <string.h>
#include <zlib.h>

int main(void) {
    /* Deliberately repetitive, so compression has to actually do something:
       a no-op "compressor" would leave the size unchanged. */
    unsigned char original[4096];
    for (unsigned i = 0; i < sizeof(original); i++) {
        original[i] = (unsigned char)('a' + (i % 7));
    }

    unsigned char packed[8192];
    uLongf packed_len = sizeof(packed);
    int rc = compress2(packed, &packed_len, original, sizeof(original), 9);
    if (rc != Z_OK) {
        printf("FAIL compress2 rc=%d\n", rc);
        return 1;
    }
    if (packed_len >= sizeof(original)) {
        printf("FAIL compressed %lu bytes is no smaller than %lu\n",
               (unsigned long)packed_len, (unsigned long)sizeof(original));
        return 1;
    }

    unsigned char restored[4096];
    uLongf restored_len = sizeof(restored);
    rc = uncompress(restored, &restored_len, packed, packed_len);
    if (rc != Z_OK) {
        printf("FAIL uncompress rc=%d\n", rc);
        return 1;
    }
    if (restored_len != sizeof(original) ||
        memcmp(restored, original, sizeof(original)) != 0) {
        printf("FAIL round trip differs (%lu bytes back)\n",
               (unsigned long)restored_len);
        return 1;
    }

    /* crc32 and adler32 live in separate translation units from the
       deflate/inflate ones, so checking them too means the archive has to
       carry more than one working member. Both values are fixed by the
       algorithm, not by the platform. */
    unsigned long crc = crc32(crc32(0L, Z_NULL, 0), (const Bytef *)"harbour", 7);
    unsigned long adler =
        adler32(adler32(0L, Z_NULL, 0), (const Bytef *)"harbour", 7);
    if (crc != 0x5b689c9aUL) {
        printf("FAIL crc32 = 0x%08lx\n", crc);
        return 1;
    }
    if (adler != 0x0b9002f4UL) {
        printf("FAIL adler32 = 0x%08lx\n", adler);
        return 1;
    }

    printf("OK zlib %s round trip %lu -> %lu -> %lu bytes\n", zlibVersion(),
           (unsigned long)sizeof(original), (unsigned long)packed_len,
           (unsigned long)restored_len);
    return 0;
}
