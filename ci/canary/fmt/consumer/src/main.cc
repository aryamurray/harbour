// Consumer for the {fmt} C++ canary. Prints OK only after every check ran.
#include <fmt/format.h>
#include <fmt/os.h>
#include <fmt/ranges.h>

#include <cstdio>
#include <stdexcept>
#include <string>
#include <system_error>
#include <vector>

#include "legacy.h"

static int failures = 0;

#define CHECK(cond)                                       \
  do {                                                    \
    if (!(cond)) {                                        \
      std::printf("FAIL line %d: %s\n", __LINE__, #cond); \
      failures++;                                         \
    }                                                     \
  } while (0)

// A function template, instantiated in this translation unit and calling into
// the formatting engine that comes from the fmt archive.
template <typename T>
std::string show(const T& v) {
  return fmt::format("<{}>", v);
}

int main() {
  // Output computed by the library, compared with the exact expected bytes.
  CHECK(fmt::format("{:>8.3f}", 3.14159) == "   3.142");
  CHECK(fmt::format("{:+05d}|{:#x}|{:b}", 42, 255, 5) == "+0042|0xff|101");
  CHECK(fmt::format("{1} {0}", "world", "hello") == "hello world");
  CHECK(show(std::string("abc")) == "<abc>");
  CHECK(show(7) == "<7>");

  // format_to_n writes into a caller buffer and reports the full size.
  char buf[8];
  auto r = fmt::format_to_n(buf, sizeof buf, "{}", "0123456789");
  CHECK(r.size == 10);

  // An exception thrown inside the library (a runtime format string with a
  // bad spec) must unwind through its frames into a catch here. This needs
  // the C++ runtime and matching exception settings on both sides.
  bool caught = false;
  try {
    (void)fmt::format(fmt::runtime("{:d}"), "not a number");
  } catch (const fmt::format_error& e) {
    caught = std::string(e.what()).find("invalid") != std::string::npos;
  }
  CHECK(caught);

  // The standard library's own exception types, built with a fmt message.
  bool std_caught = false;
  try {
    throw std::runtime_error(fmt::format("code {}", 7));
  } catch (const std::exception& e) {
    std_caught = std::string(e.what()) == "code 7";
  }
  CHECK(std_caught);

  // os.cc, the second translation unit: a POSIX error raised by the library.
  bool sys_caught = false;
  try {
    fmt::file f("/nonexistent/dir/file", fmt::file::RDONLY);
  } catch (const std::system_error& e) {
    sys_caught = e.code().value() != 0;
  }
  CHECK(sys_caught);

  // The C translation unit, called through extern "C".
  CHECK(legacy_triple(14) == 42);

  // C++20 is in effect for this program (the graph's standard).
  CHECK(__cplusplus >= 202002L);

  std::vector<int> v{1, 2, 3};
  CHECK(fmt::format("{}", v) == "[1, 2, 3]");

  if (failures) return 1;
  std::printf("OK fmt %d: %s\n", FMT_VERSION, fmt::format("{:>8.3f}", 3.14159).c_str());
  return 0;
}
