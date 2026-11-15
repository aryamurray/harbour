// Exercise crc32c at runtime against fixed answers and an independent
// bit-at-a-time reference.
//
// The vectors are RFC 3720 appendix B.4 (iSCSI), the standard CRC-32C test
// set, so a wrong answer here is wrong against the specification, not just
// against another build of this library. The long-buffer sweep matters
// because the hardware paths process 8 or 24 bytes at a time with a scalar
// head and tail: a bug in the alignment or tail handling shows up only at
// lengths that are not a multiple of the stride.
//
// Which implementation answered is not visible from here -- the portable and
// accelerated paths agree by construction -- so run.sh asserts that from the
// object files instead.
#include <crc32c/crc32c.h>

#include <algorithm>
#include <cstdint>
#include <cstdio>
#include <string>
#include <vector>

static int failures = 0;

#define CHECK(cond)                                                            \
    do {                                                                       \
        if (!(cond)) {                                                         \
            std::printf("FAIL line %d: %s\n", __LINE__, #cond);                \
            failures++;                                                        \
        }                                                                      \
    } while (0)

// Reflected CRC-32C, one bit at a time: slow, and shares no code with the
// library.
static uint32_t reference(const uint8_t *p, size_t n) {
    uint32_t crc = 0xFFFFFFFFu;
    for (size_t i = 0; i < n; i++) {
        crc ^= p[i];
        for (int b = 0; b < 8; b++) {
            crc = (crc >> 1) ^ (0x82F63B78u & (0u - (crc & 1u)));
        }
    }
    return ~crc;
}

int main() {
    // "123456789" is the classic check value for CRC-32C.
    CHECK(crc32c::Crc32c(std::string("123456789")) == 0xE3069283u);

    // RFC 3720 B.4.
    std::vector<uint8_t> buf(32, 0x00);
    CHECK(crc32c_value(buf.data(), buf.size()) == 0x8A9136AAu);
    std::fill(buf.begin(), buf.end(), 0xFF);
    CHECK(crc32c_value(buf.data(), buf.size()) == 0x62A8AB43u);
    for (int i = 0; i < 32; i++) buf[i] = static_cast<uint8_t>(i);
    CHECK(crc32c_value(buf.data(), buf.size()) == 0x46DD794Eu);
    for (int i = 0; i < 32; i++) buf[i] = static_cast<uint8_t>(31 - i);
    CHECK(crc32c_value(buf.data(), buf.size()) == 0x113FDB5Cu);

    // Every length and misalignment around the accelerated strides.
    std::vector<uint8_t> big(4096 + 64);
    uint32_t x = 12345;
    for (auto &b : big) {
        x = x * 1664525u + 1013904223u;
        b = static_cast<uint8_t>(x >> 24);
    }
    for (size_t off = 0; off < 8; off++) {
        for (size_t len : {0, 1, 7, 8, 9, 15, 16, 23, 24, 25, 47, 48, 63, 64,
                           255, 256, 1000, 4096}) {
            const uint8_t *p = big.data() + off;
            CHECK(crc32c_value(p, len) == reference(p, len));
        }
    }

    // Extend is associative over a split: crc(a||b) == extend(crc(a), b).
    uint32_t whole = crc32c_value(big.data(), 1000);
    uint32_t head = crc32c_value(big.data(), 313);
    CHECK(crc32c_extend(head, big.data() + 313, 687) == whole);

    if (failures != 0) {
        std::printf("%d check(s) failed\n", failures);
        return 1;
    }
    std::printf("OK crc32c: check value 0x%08X, %zu lengths x 8 offsets\n",
                crc32c::Crc32c(std::string("123456789")), size_t{18});
    return 0;
}
