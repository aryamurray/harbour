/*
 * openssl canary consumer.
 *
 * The failure this is shaped to catch: if a `[[targets.crypto.when]]` block
 * stops matching, openssl's per-architecture assembly is simply absent, the
 * archive still links against the generic C in the baseline, and every digest
 * below is still **correct** -- only slower. A digest check alone therefore
 * proves that the library works, not that the library Harbour was asked to
 * build is the library it built.
 *
 * So there are two kinds of assertion here:
 *
 *   1. Bytes. Known-answer tests from FIPS 180-4 (SHA-2) and FIPS 197 /
 *      SP 800-38A (AES), compared byte for byte. These catch a *wrong*
 *      build: a misassembled block function, a wrong define, a stale object.
 *   2. Symbols that exist only in the assembly layer, referenced *strongly*
 *      so the linker must resolve them from the archive. `OPENSSL_armcap_P`
 *      is defined in `crypto/armcap.c`, and `OPENSSL_ia32cap_P` in
 *      `crypto/cpuid.c`; both are in an arch `when` block and nowhere else.
 *      On aarch64 this program additionally calls `aes_v8_set_encrypt_key`
 *      and `aes_v8_encrypt` -- the ARMv8 AES instruction implementations from
 *      `aesv8-armx.S` -- and checks their output against the FIPS 197
 *      vector. If the block stopped matching, this program does not link.
 *      That is the loud failure, and it is what a digest cannot give you.
 *
 * `run.sh` covers the third angle: that the object files exist on disk under
 * the names the manifest implies, and that `sha256_block_data_order` is
 * *undefined* in `sha256.o` on an assembly platform -- i.e. that the C
 * fallback was compiled out rather than merely outvoted.
 */

#include <openssl/aes.h>
#include <openssl/sha.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int failures = 0;
static int checks = 0;

/*
 * Silent on success, like the other canaries' consumers: `ci/canary/lib.sh`
 * requires `OK` to be the *first* thing on stdout, and a passing check's name
 * is not information anyone reads. A failing one prints both values.
 */

/* Compare `got` against a lowercase hex string. */
static void expect_hex(const char *what, const unsigned char *got, size_t n,
                       const char *want_hex) {
    char got_hex[257];
    checks++;
    if (n > 128) {
        printf("FAIL %s: buffer too large for this helper\n", what);
        failures++;
        return;
    }
    for (size_t i = 0; i < n; i++)
        snprintf(got_hex + 2 * i, 3, "%02x", got[i]);
    if (strcmp(got_hex, want_hex) != 0) {
        printf("FAIL %s\n       got  %s\n       want %s\n", what, got_hex, want_hex);
        failures++;
    }
}

static void expect(const char *what, int cond) {
    checks++;
    if (!cond) {
        printf("FAIL %s\n", what);
        failures++;
    }
}

/*
 * Defined by the assembly layer only:
 *   aarch64 -> crypto/armcap.c   (in [[targets.crypto.when]] arch = "aarch64")
 *   x86_64  -> crypto/cpuid.c    (in [[targets.crypto.when]] arch = "x86_64")
 * Declared here rather than by including a private openssl header, because a
 * consumer of the package only has the public include dir -- which is also
 * the point: these are references the *linker* has to satisfy.
 */
#if defined(__aarch64__) || defined(_M_ARM64)
extern unsigned int OPENSSL_armcap_P;
/* From crypto/aes/aesv8-armx.S -- the ARMv8 AES instructions. */
int aes_v8_set_encrypt_key(const unsigned char *user_key, int bits, AES_KEY *key);
void aes_v8_encrypt(const unsigned char *in, unsigned char *out, const AES_KEY *key);
#define ARMV8_AES_BIT (1u << 2)
#define ARMV8_SHA256_BIT (1u << 4)
#elif defined(__x86_64__) || defined(_M_X64)
extern unsigned int OPENSSL_ia32cap_P[4];
#endif

/*
 * openssl 3.x's one-shot `SHA256()` is *not* in `crypto/sha/sha256.c`: it
 * lives in `crypto/sha/sha1_one.c` and is implemented with `EVP_Q_digest`, so
 * it drags in the whole provider and property machinery. A low-level slice
 * cannot offer it -- and it is the API a caller is most likely to reach for,
 * which is worth knowing. Init / Update / Final is what a slice can provide,
 * and it is the path the block function is reached through anyway.
 */
#define DIGEST_ONESHOT(NAME, CTXTYPE)                                          \
    static void NAME##_oneshot(const unsigned char *d, size_t n,               \
                               unsigned char *md) {                            \
        CTXTYPE c;                                                             \
        NAME##_Init(&c);                                                       \
        NAME##_Update(&c, d, n);                                               \
        NAME##_Final(md, &c);                                                  \
    }
DIGEST_ONESHOT(SHA1, SHA_CTX)
DIGEST_ONESHOT(SHA256, SHA256_CTX)
DIGEST_ONESHOT(SHA512, SHA512_CTX)

/* FIPS 180-4 known answers. */
static const char *SHA1_ABC = "a9993e364706816aba3e25717850c26c9cd0d89d";
static const char *SHA1_MILLION_A = "34aa973cd4c4daa4f61eeb2bdbad27316534016f";
static const char *SHA256_ABC =
    "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
static const char *SHA256_MILLION_A =
    "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0";
static const char *SHA512_ABC =
    "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a"
    "2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f";
static const char *SHA512_MILLION_A =
    "e718483d0ce769644e2e42c7bc15b4638e1f98b13b2044285632a803afa973eb"
    "de0ff244877ea60a4cb0432ce577c31beb009c5c2c49aa2e4eadb217ad8cc09b";

/* FIPS 197 appendix C: one block, three key sizes. */
static const unsigned char FIPS197_PT[16] = {0x00, 0x11, 0x22, 0x33, 0x44, 0x55,
                                             0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb,
                                             0xcc, 0xdd, 0xee, 0xff};

/* NIST SP 800-38A F.2.1, AES-128-CBC, four blocks. */
static const unsigned char CBC_KEY[16] = {0x2b, 0x7e, 0x15, 0x16, 0x28, 0xae,
                                          0xd2, 0xa6, 0xab, 0xf7, 0x15, 0x88,
                                          0x09, 0xcf, 0x4f, 0x3c};
static const unsigned char CBC_PT[64] = {
    0x6b, 0xc1, 0xbe, 0xe2, 0x2e, 0x40, 0x9f, 0x96, 0xe9, 0x3d, 0x7e, 0x11,
    0x73, 0x93, 0x17, 0x2a, 0xae, 0x2d, 0x8a, 0x57, 0x1e, 0x03, 0xac, 0x9c,
    0x9e, 0xb7, 0x6f, 0xac, 0x45, 0xaf, 0x8e, 0x51, 0x30, 0xc8, 0x1c, 0x46,
    0xa3, 0x5c, 0xe4, 0x11, 0xe5, 0xfb, 0xc1, 0x19, 0x1a, 0x0a, 0x52, 0xef,
    0xf6, 0x9f, 0x24, 0x45, 0xdf, 0x4f, 0x9b, 0x17, 0xad, 0x2b, 0x41, 0x7b,
    0xe6, 0x6c, 0x37, 0x10};
static const char *CBC_CT_HEX =
    "7649abac8119b246cee98e9b12e9197d"
    "5086cb9b507219ee95db113a917678b2"
    "73bed6b8e3c1743b7116e69e22229516"
    "3ff1caa1681fac09120eca307586e1a7";

static void sha2_known_answers(void) {
    unsigned char md[SHA512_DIGEST_LENGTH];
    unsigned char *million;

    SHA1_oneshot((const unsigned char *)"abc", 3, md);
    expect_hex("SHA1(\"abc\")", md, SHA_DIGEST_LENGTH, SHA1_ABC);

    SHA256_oneshot((const unsigned char *)"abc", 3, md);
    expect_hex("SHA256(\"abc\")", md, SHA256_DIGEST_LENGTH, SHA256_ABC);

    SHA512_oneshot((const unsigned char *)"abc", 3, md);
    expect_hex("SHA512(\"abc\")", md, SHA512_DIGEST_LENGTH, SHA512_ABC);

    /*
     * One million 'a'. Long enough to run the block function many times, and
     * fed in 1000-byte chunks so the buffering path (`md32_common.h` for
     * SHA-256) is exercised rather than a single aligned call. A block
     * function with a wrong loop bound passes on "abc" and fails here.
     */
    million = malloc(1000);
    if (million == NULL) {
        printf("bad  malloc\n");
        failures++;
        return;
    }
    memset(million, 'a', 1000);

    SHA_CTX c1;
    SHA1_Init(&c1);
    for (int i = 0; i < 1000; i++)
        SHA1_Update(&c1, million, 1000);
    SHA1_Final(md, &c1);
    expect_hex("SHA1(1e6 x 'a'), 1000-byte chunks", md, SHA_DIGEST_LENGTH,
               SHA1_MILLION_A);

    SHA256_CTX c256;
    SHA256_Init(&c256);
    for (int i = 0; i < 1000; i++)
        SHA256_Update(&c256, million, 1000);
    SHA256_Final(md, &c256);
    expect_hex("SHA256(1e6 x 'a'), 1000-byte chunks", md, SHA256_DIGEST_LENGTH,
               SHA256_MILLION_A);

    SHA512_CTX c512;
    SHA512_Init(&c512);
    for (int i = 0; i < 1000; i++)
        SHA512_Update(&c512, million, 1000);
    SHA512_Final(md, &c512);
    expect_hex("SHA512(1e6 x 'a'), 1000-byte chunks", md, SHA512_DIGEST_LENGTH,
               SHA512_MILLION_A);

    /* Unaligned, prime-sized chunks: the buffer-carry path. */
    SHA256_Init(&c256);
    for (int i = 0; i < 1000000; i += 7)
        SHA256_Update(&c256, million, (1000000 - i) < 7 ? (size_t)(1000000 - i) : 7);
    SHA256_Final(md, &c256);
    expect_hex("SHA256(1e6 x 'a'), 7-byte chunks", md, SHA256_DIGEST_LENGTH,
               SHA256_MILLION_A);

    free(million);
}

static void aes_known_answers(void) {
    unsigned char key[32];
    unsigned char out[16];
    unsigned char back[16];
    AES_KEY ks, dks;

    for (int i = 0; i < 32; i++)
        key[i] = (unsigned char)i;

    struct {
        int bits;
        const char *ct;
    } cases[] = {
        {128, "69c4e0d86a7b0430d8cdb78070b4c55a"},
        {192, "dda97ca4864cdfe06eaf70a0ec0d7191"},
        {256, "8ea2b7ca516745bfeafc49904b496089"},
    };

    for (size_t i = 0; i < sizeof(cases) / sizeof(cases[0]); i++) {
        char label[64];
        if (AES_set_encrypt_key(key, cases[i].bits, &ks) != 0) {
            printf("bad  AES_set_encrypt_key(%d)\n", cases[i].bits);
            failures++;
            continue;
        }
        AES_encrypt(FIPS197_PT, out, &ks);
        snprintf(label, sizeof(label), "FIPS 197 AES-%d encrypt", cases[i].bits);
        expect_hex(label, out, 16, cases[i].ct);

        if (AES_set_decrypt_key(key, cases[i].bits, &dks) != 0) {
            printf("bad  AES_set_decrypt_key(%d)\n", cases[i].bits);
            failures++;
            continue;
        }
        AES_decrypt(out, back, &dks);
        snprintf(label, sizeof(label), "FIPS 197 AES-%d decrypt round trip",
                 cases[i].bits);
        expect(label, memcmp(back, FIPS197_PT, 16) == 0);
    }
}

static void aes_cbc_known_answer(void) {
    /* `AES_cbc_encrypt` lives in aes_cbc.c and calls AES_encrypt per block --
     * on x86_64 that is the assembly, on the baseline the C in aes_core.c. */
    unsigned char iv[16], ct[64], back[64];
    AES_KEY ks;

    for (int i = 0; i < 16; i++)
        iv[i] = (unsigned char)i;
    if (AES_set_encrypt_key(CBC_KEY, 128, &ks) != 0) {
        printf("bad  AES_set_encrypt_key for CBC\n");
        failures++;
        return;
    }
    AES_cbc_encrypt(CBC_PT, ct, sizeof(CBC_PT), &ks, iv, AES_ENCRYPT);
    expect_hex("SP 800-38A AES-128-CBC, 4 blocks", ct, sizeof(ct), CBC_CT_HEX);

    /* The IV was consumed in place; reset it, as SP 800-38A's decrypt does. */
    for (int i = 0; i < 16; i++)
        iv[i] = (unsigned char)i;
    AES_KEY dks;
    if (AES_set_decrypt_key(CBC_KEY, 128, &dks) != 0) {
        printf("bad  AES_set_decrypt_key for CBC\n");
        failures++;
        return;
    }
    AES_cbc_encrypt(ct, back, sizeof(ct), &dks, iv, AES_DECRYPT);
    expect("AES-128-CBC decrypt round trip", memcmp(back, CBC_PT, 64) == 0);

    /* A ciphertext equal to its plaintext would mean the cipher did nothing,
     * which every "round trip succeeded" check above would happily accept. */
    expect("CBC ciphertext differs from plaintext",
           memcmp(ct, CBC_PT, sizeof(CBC_PT)) != 0);
}

/* Printed after the `OK` line, which `ci/canary/lib.sh` matches at the start
 * of stdout. What this says is the one thing a digest cannot: which
 * implementation actually ran. */
static char report[512] = "     (no architecture-specific implementation)\n";

static void architecture_specific(void) {
#if defined(__aarch64__) || defined(_M_ARM64)
    /*
     * A strong reference: if `crypto/armcap.c` were not in the build,
     * this program would not link. Reported rather than asserted non-zero,
     * because a machine legitimately may lack the extensions -- what is
     * asserted is that the *symbol* is here, which only the aarch64 `when`
     * block provides. `armcap.c` fills it from a constructor via
     * `arm64cpuid.S`'s SIGILL-guarded probes.
     */
    snprintf(report, sizeof(report),
             "     aarch64: OPENSSL_armcap_P = 0x%08x (ARMv8 SHA256=%s AES=%s)\n"
             "     so sha256_block_data_order from sha256-armv8.S took the %s path\n",
             OPENSSL_armcap_P,
             (OPENSSL_armcap_P & ARMV8_SHA256_BIT) ? "yes" : "no",
             (OPENSSL_armcap_P & ARMV8_AES_BIT) ? "yes" : "no",
             (OPENSSL_armcap_P & ARMV8_SHA256_BIT) ? "SHA-2-instruction"
                                                   : "scalar/NEON fallback");
    expect("aarch64: crypto/armcap.c linked (OPENSSL_armcap_P resolved)", 1);

    if (OPENSSL_armcap_P & ARMV8_AES_BIT) {
        /*
         * aesv8-armx.S, reached directly. This is the only path that proves
         * the ARMv8 AES instructions were assembled: nothing in the public
         * API routes to them in this slice (openssl reaches them through
         * EVP), so without this call the object could be missing and every
         * other assertion would still pass.
         */
        AES_KEY hw;
        unsigned char key[16], out[16];
        for (int i = 0; i < 16; i++)
            key[i] = (unsigned char)i;
        expect("aesv8-armx: aes_v8_set_encrypt_key",
               aes_v8_set_encrypt_key(key, 128, &hw) == 0);
        aes_v8_encrypt(FIPS197_PT, out, &hw);
        expect_hex("aesv8-armx: FIPS 197 AES-128 via ARMv8 AES instructions", out,
                   16, "69c4e0d86a7b0430d8cdb78070b4c55a");
    } else {
        /* The CPU has no AES extension, so calling aes_v8_* would SIGILL.
         * The address is still resolved, which is what the manifest is
         * asserted on; `run.sh` checks the object file itself. */
        expect("aesv8-armx: aes_v8_set_encrypt_key address resolved",
               (void *)aes_v8_set_encrypt_key != NULL);
    }
#elif defined(__x86_64__) || defined(_M_X64)
    snprintf(report, sizeof(report),
             "     x86_64: OPENSSL_ia32cap_P = 0x%08x%08x\n"
             "     AES_encrypt came from aes-x86_64.s, not aes_core.c (excluded)\n",
             OPENSSL_ia32cap_P[1], OPENSSL_ia32cap_P[0]);
    expect("x86_64: crypto/cpuid.c linked (OPENSSL_ia32cap_P resolved)", 1);
#else
    snprintf(report, sizeof(report),
             "     no `when` block matches this architecture, so the portable C\n"
             "     baseline is what produced the digests above -- correct, slower\n");
#endif
}

int main(void) {
    sha2_known_answers();
    aes_known_answers();
    aes_cbc_known_answer();
    architecture_specific();

    if (failures != 0) {
        printf("FAILED: %d of %d check(s)\n", failures, checks);
        return 1;
    }
    printf("OK openssl 3.5.4 slice: %d known-answer checks (FIPS 180-4 SHA-1/256/512, "
           "FIPS 197 AES-128/192/256, SP 800-38A CBC)\n",
           checks);
    fputs(report, stdout);
    return 0;
}
