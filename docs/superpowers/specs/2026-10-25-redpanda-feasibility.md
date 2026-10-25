# Can Harbour build Redpanda?

Status: findings and a recommended order of work. Nothing here changes Harbour;
the three packages marked **built** are real canaries added in the PR that adds
this note, and two issues were filed (#226, #227).

## Verdict

**Not today, and not in weeks. But nothing found is a wall.** The expensive,
uncertain part of Redpanda is the C++23 coroutine reactor everything sits on,
and Harbour compiles and links it: Seastar's 96 translation units build with
Harbour on Linux under both GCC 14 and clang 19 with libc++, and a program on
that reactor runs a coroutine to completion. What is missing is volume, not a
capability: about 35 third-party packages that need entries (five are in hand, and OpenSSL needs a version bump), four
code generators, 1,041 of Redpanda's own translation units to wire up, and a
Linux-only test loop on a Mac. Two Harbour limits will bite on the way:
per-source compile flags (#227) and planning time that is cubic in the number
of targets (#226), which rules out a one-manifest-target-per-Bazel-target
translation (Redpanda has 985 library targets) until it is fixed.

A defensible estimate for a first Redpanda binary that starts and passes a
smoke test is **two to four engineer-months**, dominated by dependency shims
(abseil, protobuf with `protoc` as a host tool, Boost, krb5, the FIPS OpenSSL
module, hwloc) and by discovering what Redpanda's own sources need that its
Bazel build supplies implicitly. That is judgement, not measurement; the
sections below say what each number rests on. If "build Redpanda" means
"reproduce its release binary, PGO and all, with its pinned LLVM 22.1.0
toolchain", add the toolchain and profile work on top.

The three things that matter most, in order: fix #226 (a day, and it removes a
wall); decide the target mapping (flatten, see below); and extend `harvest` to
read the compilation database Redpanda's own Bazel tree can emit, so the
source lists are measured instead of copied by hand. Per-source flags (#227)
are next, but only two dependencies need them and each has a workaround.

## What Redpanda is

Redpanda v26.2.2, released 2026-08-22 (commit `fd30e45`, `git clone --depth 1`).

| fact | value | where |
|---|---|---|
| build system | Bazel 9.1.0, bzlmod | `.bazelversion`, `MODULE.bazel` (522 lines) |
| size | 1,975 `.cc` and 1,822 `.h` under `src/v`; **1,041 non-test `.cc`**, about 597,000 lines of non-test `.cc` and `.h`; 835 test `.cc` | counted |
| targets | 252 `BUILD` files, **985** `redpanda_cc_library` targets | `bazel/build.bzl` |
| language standard | C++23 | `MODULE.bazel` `CXX_STANDARD`, `.bazelrc` |
| toolchain | clang **22.1.0** from `redpanda-data/llvm-project`, libc++ (`-stdlib=libc++`), lld, a Ubuntu 22.04 sysroot; `--config=system-clang` builds with the host clang instead | `MODULE.bazel`, `.bazelrc`, `bazel/toolchain/README.md` |
| ISA flags | `-march=westmere` on x86_64, `-march=armv8-a+crc+crypto` on aarch64 | `MODULE.bazel` `COMPILE_FLAGS` |
| link | libc++, `-Wl,-z,max-page-size=2097152`, `-Wl,-z,separate-loadable-segments`; `--config=lto` is thin LTO, `pgo-instrument` and `pgo-optimize` configs exist | `src/v/redpanda/BUILD`, `.bazelrc` |
| platform | **the broker is Linux only** (aarch64 and x86_64). The README tells macOS users to run it in Docker | `README.md` |
| license | **BSL 1.1** for the core, a separate Redpanda Community License for enterprise features (2,471 files under `src/v` carry the BSL header) | `licenses/` |
| not in scope | `rpk` (Go, `//:rpk`), the Go and Java test harnesses, the Rust transform SDK | |

Upstream cold build time is not published and was not measured here (see
"What could not be verified").

### The broker links Rust

`//src/v/redpanda:application` depends on `//src/v/wasm:impl`, which depends on
`@crates//:wasmtime_c` (`src/v/wasm/BUILD` line 224). The crate set is wasmtime
42.0.2 with a 1,120-line `Cargo.lock` (`bazel/thirdparty/Cargo.toml`). The data
transforms feature cannot be compiled out without patching Redpanda.

## Dependencies and build steps

"Harbour today" is what the manifest reference and the canaries support; the
last column is a judgement for one engineer who knows shims, using `harvest`
where the package has a cmake or Makefile to read. S is under a day, M a few
days, L one to two weeks, XL more.

| dependency | version (Redpanda) | built upstream by | Harbour today | effort |
|---|---|---|---|---|
| zlib | 1.3.1 | BCR | **built** (canary, registry entry) | done |
| zstd | 1.5.7 | BCR | **built** (canary, registry entry) | done |
| lz4 | 1.9.4 | BCR | **built** here (`ci/canary/lz4`) | done |
| snappy | 1.2.1 | BCR | **built** here (`ci/canary/snappy`) | done |
| crc32c | 1.1.0 | BCR | **built** here (`ci/canary/crc32c`), three targets because of #227 | done |
| OpenSSL | 3.5.7 | `rules_foreign_cc` `configure_make` | shim exists for 3.5.4 (canary, registry entry); bump | S |
| fmt | 12.1.0 | BCR | two sources, no config; compiled in the spike below, not committed. Redpanda needs 12: Debian's 10.1.1 fails on `fmt::range_format_kind` | S |
| yaml-cpp | 0.8.0 | BCR | about 40 C++ files | S to M |
| bzip2 | 1.0.8 | BCR | 8 C files | S |
| xxhash | pinned commit | `cc_library` | one source plus header | S |
| utf8proc | 2.11.0 | BCR | one source plus data | S |
| hdr_histogram | 0.11.8 | `cc_library` | about a dozen C files, needs zlib | S |
| ada | 3.2.4 | single-header archive | one `ada.cpp` | S |
| unordered_dense, rapidjson (fork), jsoncons, lexy | pinned commits, 2025.05.0 | `cc_library` | header-only: `kind = "header-only"` | S each |
| CRoaring (fork) | pinned commit | `cc_library` | amalgamated sources with runtime ISA dispatch | M |
| base64 | 0.5.0 | cmake | a SIMD codec per ISA, each needing its own flag: **#227**. Needs the same split crc32c needed | M |
| c-ares | 1.34.6 | cmake | about 100 sources and a config header of dozens of probe questions | M |
| libxml2 | 2.15.3 | BCR | config header, about 50 sources | M |
| liburing | 2.14 (patched) | BCR | about a dozen sources, `config-host.h` and `compat.h` from configure-time checks; Linux only | M |
| lksctp-tools | 1.0.19 | `cc_library` + `expand_template` | small; headers from templates, which Harbour has no step for (overlay file) | S to M |
| libpciaccess | pinned commit | `cc_library` | small, Linux only | S |
| hwloc | 2.11.2 | `configure_make`, autoreconf | the largest configure in the set; backends per OS | L |
| re2 | 2024-07-02 | BCR | needs abseil | M |
| Boost | rules_boost pin | `rules_boost` | headers plus compiled `filesystem`, `program_options`, `thread` and a few more; list from `@boost//:` uses in `BUILD` files (asio, beast, outcome, algorithm, range, lexical_cast, uuid, intrusive, multi_index, msm ...) | L |
| abseil | 20250814.1 | BCR | 162 non-test sources, many small units | L |
| protobuf | 33.5 | BCR | libprotobuf plus `protoc`: 221 non-test sources, needs abseil. `protoc` as a **host tool** is the mechanism LuaJIT and CPython use; a C++ host program that links a library the final binary also links works (see the spike) | L |
| avro (fork) | pinned commit | `cc_library`, 3 header-copy genrules | about 30 C++ sources, Boost, snappy, zlib | M |
| Seastar (fork) | `5bf36c5` | Bazel translation of the cmake build | **built** by the spike with system dependencies; needs ragel and protoc at build time | M once its dependencies exist |
| krb5 | 1.22.2 | `configure_make`, bison | autotools with generated error tables and mechanism config | XL |
| OpenSSL FIPS module | 3.1.2 | `configure_make` | a second OpenSSL source tree built with `enable-fips`; a different tarball | L |
| wasmtime C API | 42.0.2 (Rust) | `rules_rust` crates | a `custom` recipe that runs `cargo build` and copies a staticlib works (spike below); crates are fetched at build time, the recipe is not fingerprinted | L, mostly hermeticity |
| googleapis, protovalidate | pinned | `.proto` files only | files the protoc steps read | S |

Build steps that are not libraries:

| step | what it is | Harbour today |
|---|---|---|
| Kafka message codegen | 47 messages x request and response: `generator.py` turns 94 JSON schemas into `.h`/`.cc` | `prebuild` with `program = "python3"`. The script needs `jinja2` and `jsonschema`; `[package] build_tools` (#194) can say `python3` is needed but not which modules. M |
| RPC service codegen | `rpc_compiler.py` (jinja2), one header per service JSON | same as above. S once that works |
| protobuf codegen | 138 `.proto` files, 45 `proto_library` uses; a custom Go protoc plugin (`bazel/pbgen`) | `protoc` as a host tool, then a Go program as a second tool: needs `go` in `build_tools`. M to L |
| Seastar generated inputs | three `ragel` parsers, `metrics2.proto`, `seastar-json2code.py` (only for swagger) | done by hand in the spike; as prebuild steps with `ragel` and `protoc` as host tools. S |
| wasm | `cargo` | custom recipe, see above |
| link | two `-Wl,-z` flags, libc++, optional thin LTO and PGO | `ldflags` for the flags; `lto = true` is full LTO only (#103); PGO has no spelling |

## What was built

Three packages, chosen because Redpanda needs each and because each exercises a
different part of the manifest. All three pass on macOS (arm64, Apple clang 21)
and on Linux (aarch64, Debian 13, GCC 14.2, GNU ar 2.44, in Docker). None has
run on x86_64 yet; CI's Ubuntu runner will, and the x86_64 branches of the
`crc32c` and `snappy` assertions have not executed.

| canary | sources | what it proves | breakage it was shown to catch |
|---|---|---|---|
| `lz4` 1.9.4 | 4 | the leaf: nothing in the way. The consumer touches all four units, including `xxhash.c`, which only the frame format's content checksum reaches | dropping `xxhash.c`: the object count check fails (3, expected 4) |
| `snappy` 1.2.1 | 4 | a C++ library whose `config.h` is `#cmakedefine01` macros the sources test with `#if`. The four compiler-feature answers are literals under `compiler = "clang"` and `"gcc"` (no probe kind compiles a snippet, by design); the six header and function answers are measured | removing one answer: **the consumer still prints OK with identical output** and only the compile-command assertion fails, which is the whole reason it asserts there. Dropping `snappy-stubs-internal.cc`: count check fails |
| `crc32c` 1.1.0 | 4 across 3 targets | **per-source compile flags.** `crc32c_sse42.cc` needs `-msse4.2`, `crc32c_arm64.cc` needs `-march=armv8-a+crc+crypto`, and the portable code must have neither. Each ISA group is its own `staticlib` joined with `local = true`. Asserts which object defines the accelerated entry point and which is an empty unit, and that the *measured* `HAVE_STRONG_GETAUXVAL` answer differs between Linux and macOS | an `arch` block that matches nothing (renaming `aarch64` to `aarch65`): the build succeeds and **the consumer prints OK**; only the symbol assertion fails. Losing the define from the ISA target alone fails louder, at link |

Sources are pinned by sha256 in each `run.sh`. The snappy and crc32c shims carry
a small overlay (`snappy-stubs-public.h`, `crc32c_config.h`) because Harbour has
no template-expansion step; each says exactly what it stands in for.

Harbour decisions these exposed, all consistent with the manifest reference:

- **Duplicate basenames in one library are fine.** Redpanda has many (`types.cc`
  in a dozen directories). Harbour keeps the source's relative path in the
  object name; two `types.o` members in one archive link and rebuild correctly
  with BSD `ar` and GNU `ar` (checked with a two-file test on both).
- **The private-define lint caught a real upstream fragility.** The Seastar
  spike's first build warned that `SEASTAR_HAVE_HWLOC`, a private define in
  Redpanda's `seastar.BUILD` (`local_defines`), is referenced by the public
  header `include/seastar/core/resource.hh`. Consumers compile that header
  without it. The spike makes it public.
- **Toolchain selection works for this.** `CC`/`CXX` environment variables are
  honoured (`src/builder/toolchain/detect.rs`), `[build] cpp_runtime = "libc++"`
  and `cpp_std = "23"` are accepted, and `-fuse-ld=lld` goes in `ldflags`.

## The Seastar spike

`docs/superpowers/specs/redpanda-seastar-spike/` holds a manifest for Seastar
at the commit Redpanda pins, a one-coroutine reactor program, and
`reproduce.sh`, which does the whole thing in a fresh Debian 13 container. It
uses the **distribution's** Boost, fmt, lz4, yaml-cpp, c-ares, OpenSSL, hwloc,
liburing, protobuf and abseil so the question is Harbour's, not the packaging of
38 dependencies. Ragel parsers and `metrics2.pb.{h,cc}` are generated beside the
sources first, as `seastar.BUILD` does.

| | result |
|---|---|
| GCC 14.2 + libstdc++, `-std=c++23`, Debian 13 aarch64 | 96 units compile; the library links; `hello` runs `co_await sleep(10ms)` on the reactor and prints OK. Reproduced from a fresh container using only the committed files |
| clang 19.1.7 + libc++ (`cpp_runtime = "libc++"`), same sources | 96 units compile in 41 s at `-j5`; 105,243 libc++ symbol references in the archive. **Not linked** (the system libraries are built against libstdc++) |
| cost at `-j5` in a 7.7 GiB container | `-O3`: 63 s wall, 4 min 36 s of CPU for 96 units (about 2.9 s a unit); debug: about the same. `-j14` was killed by the kernel (`cc1plus` out of memory) |
| what the spike needed beyond the BUILD file | `xfs/linux.h` (`xfslibs-dev`; Redpanda's sysroot supplies it, `MODULE.bazel` does not list it) |

### Redpanda's own sources

Redpanda's first layer (`base`, `bytes`, `hashing`, `strings`, `random`,
`utils`: 27 translation units, base64 excluded) was built through Harbour on top
of that Seastar, with fmt 12.1.0 built by a throwaway Harbour shim. Findings:

- **GCC with libstdc++ does not get far.** `directory_walker.cc` uses
  `std::filesystem::path` without including `<filesystem>`; libc++ pulls it in
  transitively. Redpanda's code assumes libc++.
- **clang 19 with libc++ 19 passes 25 of 27** under `-fsyntax-only`. The two
  failures are the missing `hdr_histogram` header (Debian has no package) and an
  abseil version mismatch (`absl::debian7::string_view` against
  `std::string_view`: Debian's abseil is not built the way Redpanda's pinned one is), both packaging, not Harbour.
- **fmt 10 is not enough.** Redpanda specialises `fmt::range_format_kind`, which
  needs fmt 11 or later.

**A survey of the rest.** Every sixth of Redpanda's 1,042 non-test translation
units (177, in path order) was checked with `clang++ 19 -std=c++23 -stdlib=libc++
-fsyntax-only`, using the command Harbour recorded for that first layer plus the
pinned header sources of the header-heavy dependencies (abseil 20250814.1,
protobuf 33.5, rapidjson, CRoaring, jsoncons, ada, avro, lexy, hdr_histogram,
base64, crc32c) and Debian's packages for the rest. **123 of 177 (69%) pass
with no generated code and no further work.** The 54 that do not, by cause:

| cause | units | what it is |
|---|---|---|
| generated headers not produced | 37 | Kafka message headers (16), RPC service headers (13), `.proto` outputs (6), one Avro and one swagger header |
| `XXH64_state_t` is an incomplete type | 14 | Redpanda pins a newer xxhash commit than Debian's 0.8.3; a dependency version, not a language problem |
| `fmt::format` with a forwarded format string | 2 | fmt 12's compile-time format checking; same family |
| unclassified (no diagnostic captured) | 1 | `cluster/controller_utils.cc` |

None of the failures is a language or library incompatibility that Harbour
introduced or could fix; they are the dependency and generator work in the
tables above. The pass rate is the useful number: it says the two thirds of
Redpanda that needs no generated code compiles as is with a plain clang and
libc++, so the flattened-target plan has a real chance of working on the first
pass once the dependencies exist. A sample is not a census, and a syntax check is
not a compile: it says nothing about link errors or about code generation time.


## Harbour limits found

| limit | evidence | consequence | filed |
|---|---|---|---|
| planning is cubic in the number of local targets | a no-change build takes 1.5 s for 1,001 sources in one target, 0.5 s for 1,000 unconnected targets, 6 s for 1,000 targets in a chain, 14 s for 1,000 targets each depending on three others (release build; the debug build is 44 s). `local_dep_closure` re-sorts every sibling for every target | a 1:1 translation of Redpanda's 985 targets would pay this on every build | #226 |
| no per-source compile flags | crc32c (`-msse4.2`, `-march=...+crc`), base64 (a flag per SIMD codec) | each ISA group becomes a separate `staticlib` | #227 |
| no probe for "does the compiler implement this builtin" | snappy's four compiler-feature answers are literals under `compiler =` | a human assertion, visible in the manifest. **Deliberate** (`2026-09-11-native-probes-design.md` rejects snippet probes); recorded, not filed | |
| `custom` recipes are not fingerprinted | the Rust spike reruns `cargo` every build | acceptable for one crate; the warning is already printed | |
| generators cannot declare the Python modules they import | Kafka and RPC codegen need `jinja2` and `jsonschema` | `build_tools` checks a program, not a module; vendor the wheels or document the requirement | |
| `lto = true` is full LTO only | Redpanda's `--config=lto` is thin | `cflags`/`ldflags` can carry `-flto=thin` | #103 (open) |
| no template-expansion step | snappy, crc32c, lksctp, wasmtime each carry a pre-expanded header | an overlay file per package | |

What was checked and is **not** a limit: C++23 and coroutines (`cpp_std = "23"`);
libc++ and lld; a C++ host program that links a library shared with the final
binary and writes a header the target then compiles (protoc's shape; a scratch
project with a `host = true` C++ executable, a sibling library and a prebuild
step printed the generated text, and compiled the shared library once); a Rust
staticlib through a `custom` recipe, linked into a C++ executable (printed 42);
duplicate basenames; and 1,001 translation units in one target (cold 64 s in a debug build, no-change 1.5 s in a release build).

## Mapping Redpanda's graph

Redpanda's own layering is Bazel's, not a property of the code. Headers are
exposed under a virtual prefix (`include_prefix = package_name.removeprefix
("src/v/")` in `bazel/build.bzl`), so `#include "cluster/types.h"` is
`-I src/v` for a flat build. Recommended mapping, until #226 is fixed and then
by choice: **flatten**. A handful of targets (base, protocol and generated code,
storage and raft, cluster, kafka and the application) with `sources` globs and a
single `src/v` include directory, not 985. The source lists should come from a
compilation database, not by hand: `tools/harvest` already reads
`compile_commands.json` for cmake projects, and Redpanda's tree has a
compilation-database generator (`bazel/compilation_database_generator`) that
was not run here.

## Order of work

1. **#226** (planning cost). Small and unblocks everything with many targets.
2. **Leaves, in dependency order**, each with a canary that asserts what the
   output cannot show: fmt, xxhash, utf8proc, bzip2, hdr_histogram, yaml-cpp,
   ada, the header-only set, c-ares, libxml2, liburing, lksctp, libpciaccess;
   OpenSSL 3.5.7.
3. **abseil, then protobuf with `protoc` as a host tool, then re2 and avro.**
   This is the first place a real generator chain (`.proto` to C++) is proven
   end to end, and everything below needs it.
4. **Boost** (headers plus the compiled libraries Redpanda lists) and **hwloc**.
5. **Seastar proper**, replacing the spike's system dependencies, as a Linux-only
   CI job (the spike's `reproduce.sh` is the starting point).
6. **Code generators**: Kafka messages, RPC services, `pbgen`.
7. **Redpanda's core**, flattened, bottom-up by directory, using the
   `-fsyntax-only` survey above as the work list. Use clang and libc++.
8. **The hard three**, in parallel with 7 because they only meet at link time:
   krb5, the FIPS module, wasmtime.
9. **Smoke test**: `redpanda --version`, then a single-node start with
   `--smp 1`. Everything after (the test suites, release flags, PGO) is
   reproducing upstream, not building it.

#227 (per-source flags) slots in before base64 and CRoaring.

## Licensing

Redpanda's core is BSL 1.1 with an Additional Use Grant: any use except offering
it as a "Streaming or Queueing Service" to third parties, converting to Apache
2.0 four years after each release (`licenses/bsl.md`). Enterprise features are
under a separate community license.

A Harbour registry entry holds a shim and patches that point at an upstream
tarball; it does not hold Redpanda's source. The license field #214 requires is
an SPDX identifier, and `BUSL-1.1` is one (it names the generic license, not
Redpanda's grant, so the grant belongs in the entry's description). For a
**private** registry used for your own builds the grant does not restrict
anything: building and running it for yourself is not a streaming service. For a
public registry, the shim text is yours to license; whether to publish an entry
for BSL software is a policy question. This is not legal advice.

## What could not be verified

- **Upstream cold build time.** Redpanda's Bazel build downloads a hermetic
  toolchain and needs far more than the 7.7 GiB Docker had; it was not run. The
  only compile-cost numbers here are Seastar's (2.9 s of CPU a unit at `-O3`, 96
  units) and the syntax-only survey. Redpanda's own units are heavier; a guess of
  5 to 10 CPU-seconds a unit puts a full compile at roughly 90 to 170 CPU-minutes.
  That is arithmetic, not a measurement.
- **x86_64.** Every Linux run was an aarch64 VM. Redpanda's primary target is
  x86_64 with `-march=westmere`.
- **Seastar beyond a coroutine.** No disk I/O, networking or multi-shard run.
- **clang plus libc++ linked.** Only compiled, because the spike's system
  libraries are libstdc++ builds.
- **Any of Redpanda's own units compiled to an object**, other than the 27
  layer-zero units under `-fsyntax-only` with Debian's packages standing in for
  Redpanda's pins.
- **Windows and macOS.** The broker is Linux only; none of this applies.
