# Canaries: real third-party packages, built and run

Each subdirectory builds a real upstream library with Harbour and then **runs
something against it**. That last part is the point. Every bug these exist to
catch produced a *successful build*:

- `ar r` matches archive members by name, so a renamed source left its stale
  object behind and won symbol resolution. The library reported its
  pre-change behaviour with no error anywhere.
- A `[[targets.X.when]]` block that stops matching drops a platform's event
  loop or an assembly fast path. zstd falls back to a C implementation, so
  the archive still links and the only witness is the decompressed bytes.
- A source list that silently shrinks still archives successfully.

So a canary asserts on the program's output and on the translation-unit
count, never on exit status alone — and where a package has an assembly fast
path, on **which object defines the symbol**, because the count is blind to
the two cases that matter most:

- a `.S` compiled with its body `#if`'d out is an *empty object*: the count
  is unchanged, the name is present, the output is correct;
- a `|`-separated count set (`"38|37"`) legitimises both values on every
  platform, so it cannot notice one platform getting the other's answer.

`canary_require_object`, `canary_defines_symbol` and friends in `lib.sh` are
what catch those. All four failures were reproduced by breaking real
manifests, not reasoned about; the table is in
`docs/superpowers/specs/2026-09-12-openssl-generated-sources.md`.

## Running them

```sh
cargo build
ci/canary/run-all.sh              # all of them
ci/canary/zstd/run.sh             # one
ci/canary/zstd/run.sh /tmp/mydir  # one, in a chosen work dir
CANARIES="cjson zlib" ci/canary/run-all.sh   # a subset, in the order given
```

`CANARIES` replaces the default list. A name that is not a canary, or an empty
selection, is an error (exit 2) and not a quiet success; Windows uses this to run
the two canaries that build there (see below).

`run-all.sh` runs every canary even after one fails, because *which* ones are
red is the diagnostic. cjson and zlib red together means something broad;
zstd alone points at the architecture-conditional assembly block; libuv alone
points at per-OS source selection; `curl-config` alone points at the probe
subsystem, and its output names the individual question that disagreed.

## What each one covers

| canary | sources | what only it exercises |
|---|---|---|
| `cjson` | 2 | the control. No conditionals, no vendored config. Red here means something broad. Its float assertions are what make `system_libs = ["m"]` on a *public* surface load-bearing. |
| `zlib` | 15 | the original canary: a public header consumers must find, platform-conditional defines. |
| `crc32c` | 4, one empty per arch | **per-source compile flags**, which Harbour does not have: upstream keeps `crc32c_sse42.cc` and `crc32c_arm64.cc` apart because each needs an ISA flag the portable code must not get, so the shim makes each its own `staticlib` target joined with `local = true`. Asserts which object *defines* the accelerated entry point and which is an empty unit, and that the measured `HAVE_STRONG_GETAUXVAL` answer differs between Linux and macOS. |
| `snappy` | 4 | a C++ library whose `config.h` is made of `#cmakedefine01` macros tested with `#if`, so a shim that answers none of them still round-trips every byte on the portable fallback. Asserts the answers on the recorded compile command; four compiler-feature answers are literals under `compiler =` because no probe kind compiles a snippet. |
| `curl-config` | 1 | **89 of curl 8.22.0's own configure questions**, answered by Harbour probes and compared against what curl's cmake concluded on the same platform. 11 of the 89 answers differ between macOS and Linux, in both directions — those are the rows that would catch the probe subsystem returning constants. Downloads nothing: curl's *questions* are what is under test. See `curl-config/regenerate.md`. |
| `cpython-config` | 1 | **475 of CPython 3.14.9's own configure questions**, answered by Harbour probes and compared with what `./configure` concluded on the same platform: headers, functions, sizes, types, struct members, declarations. 167 more are *asserted* (choices, package metadata, run-time behaviour checks) and printed as a separate group, so an oracle satisfied by literals cannot pass for a subsystem that measured nothing. The generator and the seven things Harbour could not express are in `cpython-config/regenerate.md`. |
| `libuv` | 31 + per-OS | `[[targets.X.when]]` keyed on `os` with **no portable fallback** — a stale block fails to link on `uv__platform_loop_init` rather than building something subtly wrong. The consumer drives a real TCP echo round trip through the selected event loop. |
| `zstd` | 37 + 1 `.S` on x86_64 | mixed C and assembly across five source directories; `ZSTD_MULTITHREAD` making `pthread` load-bearing on the public link surface; the dictionary builder, which is the directory a source list is most likely to drop. Also the **symbol-level** check on that `.S`: `ZSTD_DISABLE_ASM` compiles it to an empty object, which keeps the count at 38 and the round trip byte-exact. |
| `luajit` | 69 + 1 generated `.S` | **host targets: programs built for the build machine and run during the build.** `minilua` and `buildvm` are `host = true` targets, and `prebuild` steps list them under `tools`: `minilua` writes `buildvm_arch.h`, `buildvm` writes the VM's assembly and seven tables. `buildvm` runs on the build machine but is *configured for the target* -- its DynASM flags and `-DLUAJIT_TARGET` name the architecture the VM is generated for, so they key on `target_arch`, and on Linux x86_64 also `SHADOW_STACK`, which must agree between the generated assembly and the C that includes it. On macOS the canary also cross-builds for the other Apple architecture and checks the VM object is the target's while `buildvm` stays a host binary. The consumer runs a hot loop long enough for the tracing JIT to compile it and asks the VM whether it did; the `.S`'s object must define the interpreter entry point. The count of 76 is the library's 70 plus the host tools' 6, which a native build keeps in the package's own object directory. |
| `openssl` | 9 + 5 generated per arch | **sources that do not exist in the tarball.** The only canary that runs `[[targets.X.prebuild]]` generators, and the only one whose *headers* are generated: 31 `.h.in` templates plus per-architecture perlasm, all produced on the machine doing the build, nothing vendored. It also covers per-(os, arch) generator selection — the perlasm flavour (`ios64`/`linux64`/`macosx`/`elf`) is what a cross-build gets wrong — and `exclude`, since on x86_64 `aes-x86_64.s` replaces `aes_core.c` rather than adding to it. |
| `cpython` | 190 + generated `config.c` and 24 frozen headers | **the deepest generated-source chain, built from host targets, and a feature that changes a generated file.** `core` is a static library shared by three programs: `freeze_module` and `bootstrap_python` (`host = true`) and the interpreter. CPython freezes its own startup modules by running CPython, so each stage is a `prebuild` step that lists the tool it needs under `tools`, producing 24 headers asserted **byte-identical** to CPython's own build (a count would pass with the wrong source frozen). The tools want the *host's* `pyconfig.h`, which is what a plain `os`/`arch` condition means inside a host tool. The built-in module table is generated from the active features, so enabling `zlib` changes it, and the canary checks that `import zlib` round-trips with it on and fails with it off. `inputs` and the tool binaries' content make a build with nothing changed run no generator and recompile nothing (asserted as behaviour, not as a time). `pyconfig.h` is vendored per platform from CPython's own `./configure` -- see `cpython/README.md`. Needs host targets (#172), `target_*` conditions (#176), and the compile-cache key fix (#179). |
| `redis` | 145 across 9 targets | **one package that is several programs and the libraries they share.** Redis's own `deps/` (hiredis, linenoise, Lua with cjson/cmsgpack/struct/bit, hdr_histogram, fpconv) are siblings built as `staticlib` targets, and a `common` library holds the 13 translation units the server, the CLI and the benchmark all compile, built once; the three programs link them with `local = true`. The canary runs what it built: `redis-server --version` against the pin, PING, SET/GET, INCR and Lua over TCP on a kernel-chosen port, the Redis-only Lua modules, `redis-benchmark` (hiredis and hdr_histogram), and SHUTDOWN exiting 0. No `when` selects a source (the event loop is chosen by a macro in `ae.c`), so a Linux and a macOS build compile the same 145 files; the platform-specific part is the link line (`-lpthread -lrt -rdynamic` on Linux). Pins **7.2.16**, the newest BSD-3-Clause release. Built with libc malloc, not the bundled jemalloc upstream defaults to on Linux (`malloc=libc` in the version line). |

## Windows (MSVC)

CI runs `cjson` and `zlib` on `windows-latest` in their own job, `Canaries
(Windows)`, with `CANARIES="cjson zlib" ci/canary/run-all.sh` under Git Bash. The
others are not run on Windows yet. Each is blocked by something specific, found by
a one-off probe (draft PR #213, never merged) that built them with MSVC 14.51:

| canary | Windows status | what it needs before it joins the job |
|---|---|---|
| `cjson`, `zlib` | **run in CI** | nothing; cJSON needed Harbour to skip the Unix-only `m` library under MSVC (#210) |
| `zstd` | builds with a shim change | an `os = "windows"` block that excludes the `.S` file and sets `ZSTD_DISABLE_ASM`; Harbour rejects `.S` under `cl` with a clear error. Its tarball holds symlinks, so it needs `MSYS=winsymlinks:lnk`, which the job already sets |
| `libuv` | needs a Windows shim | a Windows source set (about 37 sources) and system libraries; the committed shim is the Unix one and fails on `sys/socket.h`. The consumer's `MSG` macro also collides with the `MSG` struct in `<windows.h>` and must be renamed |
| `luajit` | library builds and links | `buildvm` writes `lj_vm.obj` directly under MSVC (no assembly), so Harbour must accept a prebuilt `.o`/`.obj` in `sources` (#209) and the missing-source check must honour `exclude` (#212). The consumer's `ffi.C.abs` does not resolve there and needs a CRT-independent call |
| `cpython` | not built | feasible: Windows ships `PC/pyconfig.h` and `PC/config.c`, so nothing is generated, but the shim needs about 51 more sources |
| `redis` | not applicable | POSIX only: upstream has no Windows port (`fork(2)`, epoll/kqueue), and the shim's `supports` lists no MSVC triple |
| `openssl` | not attempted | needs a Windows source set and an MSVC assembler path (`ml64` or `nasm`) that Harbour does not have |
| `curl-config`, `cpython-config` | not run | native probes under MSVC (the 2026-09-12 spike) have not been exercised end to end |

**What the two Windows canaries assert, against Linux and macOS.** The same
things, because the scripts are the same and nothing is skipped:

- the tarball's sha256, the build, and a second build that reports every object
  up to date and compiles nothing;
- the translation-unit count, taken from `.obj` files (2 for cJSON, 15 for zlib);
- the consumer is built, found (`.exe`), run, and must print `OK` after its own
  assertions.

A mutation run on Windows confirmed each of those is live and not decorative:
changing zlib's expected count to 16 failed with "compiled 15 translation units,
expected 16", and changing a cJSON consumer assertion failed with the consumer's
own `FAIL line 44` and "consumer did not report OK".

**What they do not assert on Windows.** zlib's per-OS `Z_HAVE_UNISTD_H` defines
live in `os = "linux"` and `os = "macos"` blocks, so the platform-conditional
defines are not exercised under MSVC. cJSON's `libm` check is not either: `m` is
skipped under MSVC (the CRT provides the math), so "`system_libs` on a public link
surface is load-bearing" is only tested on Unix. Neither canary defines symbol
checks, so there is no `nm` step to skip. The runner does have an `nm`
(`C:\mingw64\bin\nm`, MinGW's); whether it reads MSVC `.obj` files has not been
tested, and `canary_nm` fails loudly when `nm` is missing rather than skipping.

**Things the job depends on.**

- `shell: bash` is Git Bash. Its `link` is **GNU coreutils' `link`**, not MSVC's,
  and sits first on `PATH`; `cl` is not on `PATH` at all. Harbour finds MSVC through
  `vswhere` and never resolves it from `PATH`, which the green run shows. Do not add
  a `vcvarsall` or `msvc-dev-cmd` step, and do not call a bare `link` from a canary
  script.
- `MSYS=winsymlinks:lnk` is set for the job. It is inert for these two tarballs
  and is there so the next one (zstd's has symlinks) does not rediscover it.
- Windows bills at 2x. Cost, measured on real runs: about 2:50 cold (169 s: building Harbour 119 s, the two canaries 15 s) and
  1:43 warm (103 s: 47 s and 20 s), so roughly 3 and 2 billed minutes per run at the 2x
  rate. Most of it is `cargo build`; the canaries are seconds. Each new canary here
  adds its own download and compile time on top.

## How they are laid out

```
ci/canary/
  lib.sh            the shared machinery: fetch, verify, build, assert
  run-all.sh
  <pkg>/run.sh      ~10 lines: a pinned URL, a sha256, expected TU count
  <pkg>/Harbour.toml          the committed shim
  <pkg>/consumer/             a program that uses the library and prints OK
  <pkg>/overlay/              (optional) files laid over the extracted tree: generator scripts
```

`lib.sh` exists rather than a copy of the same 100 lines per package. Four
copies of "assert the second build reused everything" would drift, and the
copy that drifted would be the one that stopped checking — which is the
finding the 2026-09-07 schema audit reached ten times over about this
codebase.

## Two deliberate choices

Two of them do not follow the fetch-and-build shape. `openssl` has its own
`run.sh` body rather than `canary_standard_run`, because it has work to do
before the build (asserting the tarball does *not* already contain the headers
its manifest claims to generate) and a longer list of objects to check
afterwards. A canary that only needs the latter uses the
`canary_extra_assertions` hook instead, as zstd does.

And `curl-config` has
no upstream tarball, because what it tests is curl's list of *questions*
rather than its sources. It holds curl's answers as a golden file
(`expected.json`, produced by curl's own cmake — see `regenerate.md`) and
compares Harbour's generated `curl_config.h` against them question by
question.

**Manifests are committed; upstream sources are not.** A change to a shim
then shows up in review as a diff, while a vendored tarball would be a fork
nobody remembers taking. Each `run.sh` fetches a pinned tarball into a work
directory and copies the committed `Harbour.toml` into it.

These manifests were previously rebuilt from scratch three separate times
because they only ever lived in a temp directory, and were recoverable at all
only because `docs/superpowers/specs/2026-09-07-extensibility-audit.md`
happens to embed them — and zstd's source list is elided even there. They are
version-controlled now for that reason.

**LuaJIT is pinned by commit, not by tarball.** It publishes no releases and
no tags, so `CANARY_GIT_COMMIT` makes `canary_standard_run` fetch that commit
with git instead. Git checks every object against the hash, which is stronger
than the sha256 of a GitHub-generated archive whose bytes GitHub does not
promise to keep stable.

**Tarballs are pinned by sha256, and by a release asset where one exists.** A
canary that follows whatever upstream publishes turns a supply-chain change
into a Harbour bug report, and an interrupted download into a mystery compile
error. zlib, zstd and curl publish immutable release assets; libuv publishes
`dist.libuv.org` tarballs. cJSON has only GitHub's auto-generated archive,
which GitHub has changed the generation of once before — pinning it means
that shows up as a checksum mismatch naming the file.

## Adding one

Four files. The expected translation-unit count may be a `|`-separated set
when it legitimately differs per platform (`"38|37"` for zstd) — but know what
that costs: a set legitimises every value on every platform, so it cannot
catch one platform building another's source list. If the difference is an
assembly fast path, define `canary_extra_assertions` and name the object and
its symbol as well.

```sh
mkdir -p ci/canary/foo/consumer/src
# ci/canary/foo/Harbour.toml         the shim
# ci/canary/foo/consumer/Harbour.toml  path dep on "../upstream"
# ci/canary/foo/consumer/src/main.c    prints "OK ..." only after asserting
# ci/canary/foo/run.sh                 URL, SHA256, topdir, name, exe, count
```

Then add it to `CANARIES` in `run-all.sh`. Cheapest and broadest first, so a
systemic breakage is reported in seconds rather than after two minutes of
downloads.
