// Exercise snappy at runtime, through all four translation units.
//
//   snappy.cc                 Compress / Uncompress / validation
//   snappy-sinksource.cc      ByteArraySource and UncheckedByteArraySink,
//                             the interface Compress(Source*, Sink*) is built on
//   snappy-stubs-internal.cc  reached through snappy.cc's string helpers, so
//                             a shim that dropped it fails to link
//   snappy-c.cc               the C API, which only this file defines
//
// A static archive missing a member still archives successfully, so each
// translation unit gets an assertion of its own. Whether the *fast paths* were
// compiled in is not visible from here -- the output is identical without
// them -- and is asserted by run.sh from the compile commands instead.
#include <snappy-c.h>
#include <snappy-sinksource.h>
#include <snappy.h>

#include <cstdio>
#include <cstring>
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

int main() {
    std::string input;
    for (int i = 0; i < 4096; i++) {
        input += "redpanda-snappy-canary ";
        input += static_cast<char>('a' + i % 26);
    }

    // C++ string API: snappy.cc, with snappy-stubs-internal.cc behind it.
    std::string packed, back;
    size_t n = snappy::Compress(input.data(), input.size(), &packed);
    CHECK(n == packed.size());
    CHECK(packed.size() < input.size() / 5);
    CHECK(snappy::IsValidCompressedBuffer(packed.data(), packed.size()));
    CHECK(snappy::Uncompress(packed.data(), packed.size(), &back));
    CHECK(back == input);

    // Source/Sink interface: snappy-sinksource.cc.
    snappy::ByteArraySource source(input.data(), input.size());
    std::vector<char> out(snappy::MaxCompressedLength(input.size()));
    snappy::UncheckedByteArraySink sink(out.data());
    size_t via_sink = snappy::Compress(&source, &sink);
    CHECK(via_sink == packed.size());
    CHECK(std::memcmp(out.data(), packed.data(), via_sink) == 0);

    // Corruption is rejected rather than decoded into garbage.
    std::string bad = packed;
    bad[bad.size() / 2] ^= 0x7f;
    std::string junk;
    bool decoded = snappy::Uncompress(bad.data(), bad.size(), &junk);
    CHECK(!decoded || junk != input);

    // C API: snappy-c.cc, the only file that defines these symbols.
    size_t clen = snappy_max_compressed_length(input.size());
    std::vector<char> cbuf(clen);
    CHECK(snappy_compress(input.data(), input.size(), cbuf.data(), &clen) ==
          SNAPPY_OK);
    size_t ulen = 0;
    CHECK(snappy_uncompressed_length(cbuf.data(), clen, &ulen) == SNAPPY_OK);
    CHECK(ulen == input.size());
    std::vector<char> ubuf(ulen);
    CHECK(snappy_uncompress(cbuf.data(), clen, ubuf.data(), &ulen) ==
          SNAPPY_OK);
    CHECK(std::memcmp(ubuf.data(), input.data(), input.size()) == 0);
    CHECK(snappy_validate_compressed_buffer(cbuf.data(), clen) == SNAPPY_OK);

    if (failures != 0) {
        std::printf("%d check(s) failed\n", failures);
        return 1;
    }
    std::printf("OK snappy %d.%d.%d: %zu -> %zu bytes\n", SNAPPY_MAJOR,
                SNAPPY_MINOR, SNAPPY_PATCHLEVEL, input.size(), packed.size());
    return 0;
}
