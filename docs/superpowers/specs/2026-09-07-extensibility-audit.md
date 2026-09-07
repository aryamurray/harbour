# Audit: is Harbour extensible? Three new packages, measured

**Date:** 2026-09-07
**Status:** Audit — verdict is *yes, the marginal cost has converged*
**Scope:** shim three real C libraries that have never been shimmed
(cJSON 1.7.18, libuv 1.51.0, zstd 1.5.7), verify each by running a consumer
program, and count what new Harbour work each one forced.

---

## Verdict up front

**No new Harbour feature was needed. Not one, for any of the three packages.**

zlib, libpng, openssl and curl each forced new features to be built. The worry
was that this would not converge — that package N+1 always needs new Harbour
work, which would make Harbour a bespoke build-script generator rather than a
package manager. On this evidence it has converged. Every mechanism these three
packages needed already existed and already worked:

| mechanism | needed by | existed |
|---|---|---|
| `[[targets.X.when]]` with `sources` + `defines` | libuv, zstd | yes |
| `.S` assembly sources in a mixed target | zstd | yes |
| `[[targets.X.surface.when]]` → `link.public` | libuv | yes |
| `public` / `private` surface shorthand, `system_libs` | all three | yes |
| `public_headers`, `include_dirs` | all three | yes |
| `requires` / `supports` | all three | yes |
| path dependencies | all three consumers | yes |

Nothing was reached for and found missing. Not `prebuild`, not `exclude`, not
`freestanding`, not recipes, not vcpkg. **Every one of the nine
(package, platform) cells built and passed a behavioural test on the first
attempt**, with zero build-error iterations: no compile failure, no link
failure, no missing header, no wrong-flag round trip. The manifests are 17, 74
and 62 lines of actual configuration.

Two things did cost effort, and neither is a Harbour feature gap:

1. **A genuine Harbour bug, found by trying to build in a container** — a
   relocated `Harbour.lock` silently builds the *original* directory's sources.
   §5. Fixed here, with a test that fails without the fix.
2. **A generalisation of `tools/harvest`** — its `when`-block layering was
   hardcoded to the architecture axis, because openssl's cross-cutting axis is
   architecture. libuv's is the *OS*. §3. Fixed here, with tests covering both
   shapes.

The honest caveat: none of these three packages has a `configure`-generated
`config.h`. That is a real property of the packages, not an evasion — but it
means the 793-line-per-platform `curl_config.h` problem is **untouched, not
solved**. §4 says what that implies and what would actually retire it.

---

## 1. Why these packages

The question is empirical, so the packages were chosen to spread the difficulty
and to attack the specific mechanisms that were expensive last time.

**cJSON — the control.** Two source files, headers at the package root. If the
*easy* case is not easy now, nothing else matters. It also tests whether
`tools/harvest` works on a package that is not openssl.

**libuv — the platform-selection stress test.** Sources are split
`src/unix/` vs `src/win/`, and inside `src/unix/` a per-OS subset is chosen from
54 files: `darwin.c`, `kqueue.c`, `fsevents.c` on macOS; `linux.c`,
`procfs-exepath.c`, `random-getrandom.c` on Linux; `aix.c`, `sunos.c`,
`haiku.c`, `qnx.c`, `os390.c` and friends for platforms nobody here builds. A
default `src/**/*.c` glob is catastrophically wrong. This is the sharpest
available test of `[[targets.X.when]] sources`, and the axis is the OS — which
turned out to matter (§3).

**zstd — assembly plus multi-directory.** Its own Makefile, a cmake build, five
source directories, compile-time option defines that change which directories
participate (`ZSTD_LEGACY_SUPPORT`, `ZSTD_MULTITHREAD`), and one hand-written
`.S` file that exists on x86_64 only. Assembly is where openssl was expensive,
so this re-tests that path on a package with a completely different layout.

sqlite was considered and dropped: its amalgamation is one file, so it tests
option defines but nothing about source selection, and cJSON already covers
"trivial source list". libuv and zstd between them cover strictly more.

---

## 2. What each one cost

### 2.1 Result matrix — all verified by running, not by exit status

Every cell below is a consumer program that links the static archive and
asserts real behaviour. `docker --platform linux/{amd64,arm64}` with the
`rust:1-bookworm` image, Harbour rebuilt inside the container from the mounted
worktree (`CARGO_TARGET_DIR=/lt`, so no macOS `target/` leaks in).

| package | macos/aarch64 | linux/x86_64 | linux/aarch64 | assertions |
|---|---|---|---|---|
| cJSON | pass (3 TUs) | pass (3 TUs) | pass (3 TUs) | 11 |
| libuv | pass (36 TUs) | pass (36 TUs) | pass (36 TUs) | 24 |
| zstd | pass (38 TUs) | pass (**39** TUs) | pass (38 TUs) | 16 |

The 39th zstd translation unit on x86_64 is the assembly, and its object file
was checked for by name rather than inferred:

```
/w/zstdtest/.harbour/target/debug/deps/zstd-1.5.7/obj/zstd/lib/decompress/huf_decompress_amd64.o
```

present on linux/x86_64, absent on both aarch64 platforms. That is the
`[[targets.zstd.when]] arch = "x86_64"` block demonstrably selecting a `.S`
source, not a claim that it should have.

### 2.2 What the consumers actually assert

A build that succeeds proves little, so each consumer exercises the parts the
manifest makes decisions about:

* **cJSON** — parse a document; check a string field, an integer field, a
  float field, an array length, an array element; resolve a JSON Pointer
  (`cJSONUtils_GetPointer`, which lives in the *second* translation unit, so it
  fails if only `cJSON.c` were listed); print, reparse and `cJSON_Compare`;
  assert the printed text contains `"ratio":0.5`; assert a truncated document
  is *rejected*.
* **libuv** — a TCP echo server and a client in one event loop: bind to port 0,
  read the kernel-assigned port back, connect, write a payload, echo it, and
  `strcmp` the received bytes against what was sent. Plus a timer, a
  threadpool job, and then one call each into the functions that come from
  *platform-specific* sources — `uv_exepath` (`darwin.c` /
  `procfs-exepath.c`), `uv_random` (`random-getentropy.c` /
  `random-getrandom.c`), `uv_interface_addresses` (`bsd-ifaddrs.c` /
  `linux.c`), `uv_resident_set_memory`, `uv_fs_stat`. If a `when` block
  selected the wrong OS's files these link, and only these.
* **zstd** — compress 256 KiB and decompress it and `memcmp` the result;
  check the frame header's recorded content size; **corrupt a byte and assert
  the frame no longer decodes to the original**; run the streaming API in
  4 KiB chunks comparing every chunk in place; set `ZSTD_c_nbWorkers` (which
  is rejected unless `ZSTD_MULTITHREAD` was actually compiled in) and
  round-trip through the multithreaded compressor; train a dictionary with
  `ZDICT_trainFromBuffer` from `lib/dictBuilder`.

Compression ratio is reported (`262144 bytes -> 110 bytes`) rather than
asserted loosely, and the round trip is byte-exact, so a misbuilt entropy coder
cannot pass.

### 2.3 Iteration count

This is the number that answers the question, and it is the one worth
distrusting most, so: **zero failed build attempts across all three packages.**
The first `harbour build` of each consumer compiled and linked, and the first
run of each binary passed every assertion. The only rebuild-and-retry in the
whole exercise was deliberate — deleting zstd's private `include_dirs` to check
whether they were needed (they are not; §2.4).

For contrast, the two failures that *did* happen were both in Harbour or its
tooling, not in the manifests: the lockfile bug (§5) and harvest's layering
axis (§3).

### 2.4 Things checked rather than assumed

* zstd's private `include_dirs` were removed and the build re-run. It still
  compiles all 37 C files and passes, because zstd's sources include each other
  by relative path. The manifest is smaller for it, and the comment says so.
* The `pthread` on zstd's *public* surface is load-bearing: `ZSTD_MULTITHREAD`
  is compiled in, and a static archive does not record its own dependencies.
* libuv's `dl` and `rt` come from libuv's own cmake link line, read rather than
  guessed, and are declared `link.public` under `os = "linux"`.
* cJSON's public `system_libs = ["m"]` is exercised by the float assertions.

---

## 3. How much was mechanical: `tools/harvest`

**It worked, on all three, with no package-specific changes.** This is the
second-strongest result in the audit after "no missing features".

```
cJSON  : 1 source,  4 defines,  0 generated   (cmake compile_commands.json)
libuv  : 37/35/35 sources per platform,       (cmake, 3 platforms)
zstd   : 37/38/37 sources per platform        (cmake, 3 platforms)
```

Two things worth recording about *how* it worked.

**Harvesting Linux from macOS needed no change.** The container's
`compile_commands.json` records `/w/libuv-1.51.0/...`; passing
`--source /w/libuv-1.51.0` from the host works because `extract-cc` relativises
lexically. That is a small thing that could easily have been a rewrite.

**Its layering axis was hardcoded, and that was a real defect.** `merge` emits
`when` blocks in layers general-to-specific, and grouped only by
*architecture* — because openssl's cross-cutting deltas are its assembly, which
is arch-specific and OS-agnostic. libuv is the same problem rotated: its four
Linux-only sources are identical on every Linux architecture. The old merge
produced

```toml
[[targets.uv.when]]
os = "linux"
arch = "aarch64"
sources = ["src/unix/linux.c", "src/unix/procfs-exepath.c", ...]

[[targets.uv.when]]
os = "linux"
arch = "x86_64"
sources = ["src/unix/linux.c", "src/unix/procfs-exepath.c", ...]   # identical
```

which is exactly the failure `tools/harvest/README.md` warns about, one axis
over: a manifest that looks granular while covering only what happened to be
harvested. linux/riscv64 matches neither block, compiles the intersection, and
fails to link on `uv__platform_loop_init` — on a package libuv supports.

The fix is a **generalisation, not a hack**: `os` becomes a layering axis on
the same terms as `arch`, and each source/define is attached to the coarsest
condition whose harvested platforms *all* want it (a greedy exact tiling, so no
item is ever emitted twice). The same code now produces:

| package | layer it chooses | why |
|---|---|---|
| openssl-shaped | `arch = "aarch64"` | assembly, same on macOS and Linux |
| libuv | `os = "linux"` | same four files on every Linux arch |
| zstd | `arch = "aarch64"` | `ZSTD_DISABLE_ASM`, same on macOS and Linux |

`tools/harvest/test_layering.py` covers both shapes plus the intersection,
single-platform and generated-sources cases. Run against the pre-change
`harvest.py`, exactly one test fails — the libuv-shaped one — and the
openssl-shaped one still passes. That is the evidence that this generalises
rather than trades one package for another.

### 3.1 What harvest still leaves to the author

Recorded in the README too, because all three showed it:

* **CMake's own defines come through.** cJSON's harvest contributed
  `cjson_EXPORTS` and `CJSON_EXPORT_SYMBOLS`, artefacts of cmake's *shared*
  library target and wrong for the static archive a shim declares.
* **A define recording what the harvested build *disabled* is usually the
  wrong thing to copy.** cmake sets `ZSTD_DISABLE_ASM` on every non-x86_64
  platform and merge duly layers it under `arch = "aarch64"`. But zstd's own
  guard already tests `defined(__x86_64__)`, so the correct manifest omits the
  define entirely and only *adds* the assembly under `arch = "x86_64"`.
  Enumerating the architectures that must disable something means the next
  architecture is silently missing from the list. This is the sharpest
  recurring judgement call in shim-writing and it follows from one property of
  the model: **`when` blocks are additive with no "else"**, so the
  unconditional case has to be the one that is right everywhere. openssl needed
  a whole extra "portable baseline" harvest for the same reason.
* **The public surface cannot be inferred** — already documented, still true.

None of these is automatable from a build system's output, because the build
system does not record the distinction. They are a handful of lines of
judgement per package, and the README now names them so the next author does
not rediscover them.

---

## 4. Manifest length, and the platform-enumeration question

| package | total lines | config lines | inside `when` blocks | vendored config files |
|---|---|---|---|---|
| cJSON | 25 | 17 | 0 | 0 |
| libuv | 94 | 74 | 24 (32%) | 0 |
| zstd | 87 | 62 | 3 (5%) | 0 |
| *curl (existing)* | *60* | — | — | *2 × 793-line `curl_config.h`* |

libuv's 74 lines are dominated not by platform enumeration but by the
unconditional source list: 31 file names, one per line, that every unix
platform compiles. The platform-specific part is 24 lines, and 10 of those are
also just file names. zstd's conditional part is **three lines** — one block,
one condition, one assembly file — for a library with five source directories
and an architecture-specific decoder.

**So: none of the three has curl's problem.** Not a worse one, not the same
one — none. And the reason is a property of the packages, which is the honest
way to state it: libuv and zstd do not have a `configure`-generated
`config.h` at all. libuv branches on `#ifdef __linux__` and friends inside its
sources and takes a handful of feature macros (`_GNU_SOURCE`,
`_DARWIN_USE_64_BIT_INODE`) on the command line; zstd's portability lives in
`lib/common/portability_macros.h`, evaluated by the compiler. Both express
their platform knowledge in C, where it belongs, so there is nothing to vendor.

This is worth saying plainly because it cuts both ways:

* **It is evidence that the vendoring in curl's shim is curl-specific**, not a
  structural tax Harbour imposes on every package. Three more real libraries,
  including a famously platform-sensitive one, needed zero vendored config
  headers. `[[targets.X.when]] include_dirs` exists for the packages that do
  need it and was not needed here.
* **It is not progress on curl.** The 793-line-per-(os, arch) `curl_config.h`
  is untouched by this audit. Retiring it needs something none of these
  packages exercise — either running the package's own `configure` as a
  `prebuild` step (possible today in principle; nobody has done it, and it
  would need the generated header declared as a `prebuild` output), or a
  Harbour-native probe facility that answers `HAVE_*` questions by compiling
  test programs. Nothing here argues for or against that. It stays an open
  question, and the next package to hit it will be an autotools package, not a
  cmake one.

---

## 5. The bug: a relocated `Harbour.lock` silently builds the wrong sources

Found by trying to build in a container, which is where the dominant failure
mode of this codebase — code that looks wired and is not — reliably surfaces.

`Harbour.lock` records path sources as **absolute** URLs, including the root
package's own:

```toml
[[package]]
name = "cjtest"
version = "0.1.0"
source = "path+file:///private/tmp/.../audit/cjtest"
```

The freshness check (`workspace_lockfile_needs_update`) hashes manifest
*content*. A copied project has identical manifests, so the hash matches, so
the lockfile is reused — and every path in it points at the original directory.

### Reproduction (the silent form, on one machine, no container)

```sh
cp -r cjtest repro/cjtest        # includes Harbour.lock
cp -r cJSON-1.7.18 repro/
printf '#include <stdio.h>\nint main(void){puts("I AM THE COPY");return 0;}\n' \
    > repro/cjtest/src/main.c
cd repro/cjtest && harbour build && ./.harbour/target/debug/bin/cjtest
```

Before the fix:

```
INFO Using existing lockfile (workspace unchanged)
INFO All 3 file(s) up to date
     Finished debug [native] .../repro/cjtest/.harbour/target/debug/bin/cjtest
ok   parse succeeded
ok   name == "harbour"
...
```

Exit 0, no warning, and the binary is the *original* program — the edit to the
copy was never compiled. `compile_commands.json` in the copy names the
original's sources, including `audit/cjtest/src/main.c` rather than
`audit/repro/cjtest/src/main.c`. This is the repo's signature failure: a
successful build with wrong output.

The louder form is what surfaced first: in a container the old path does not
exist, so the same lockfile produced

```
INFO Using existing lockfile (workspace unchanged)
error: no manifest found in `/private/tmp/.../audit/cjtest`
help: create `Harbour.toml`
```

for a directory whose `Harbour.toml` is right there at `/w/cjtest`. A committed
`Harbour.lock` breaks the build on every machine but the one that wrote it —
CI, containers, a second clone, a colleague.

### Fix

A lockfile written somewhere else is stale regardless of what it hashes to.
`workspace_lockfile_needs_update` now also checks that the lockfile's recorded
path source for each workspace *member* still points at that member's actual
directory (canonicalised, so `/tmp` vs `/private/tmp` is not a false positive),
and forces re-resolution if not. Re-resolution re-derives every path from the
manifests being built, so the dependencies are corrected too.

After the fix, the reproduction prints `I AM THE COPY` and rewrites the
lockfile with the copy's paths; the container run reports
`Workspace changed, re-resolving dependencies` and builds correctly on
linux/amd64 and linux/arm64.

`test_relocated_workspace_lockfile_is_stale` covers it. It fails on the
unpatched check (verified by disabling the new condition and re-running, not
assumed).

### What this fix deliberately does *not* do

It makes the lockfile *safe* when relocated; it does not make it *portable*. A
committed `Harbour.lock` will still be rewritten on the next machine that
builds it, because the paths it stores are absolute. Cargo avoids this by
omitting `source` for path dependencies entirely and reconstructing them from
the manifests. Doing the same here is a lockfile-format change — encode/decode
plus `SourceId` — and a design decision worth taking on its own terms rather
than inside an audit. Recorded as the follow-up. The bug that silently builds
the wrong tree is fixed now because it is the one that lies to you.

### A note on the process

The fix was verified, then the test was disabled-and-re-enabled to prove it
fails without the fix, and then `target/debug/harbour` was **left stale** by
that experiment. The next shim build promptly failed with the old binary's
behaviour and cost ten minutes of confusion. Rebuild after reverting an
experiment; the harness will not tell you.

---

## 6. What was assumed rather than verified

Stated explicitly because the value of the audit depends on the line being
honest.

* **The unharvested-platform failure mode for libuv is reasoned, not
  observed.** A platform matching no `when` block (FreeBSD, linux/riscv64)
  gets the 31-file intersection and should fail to link on
  `uv__platform_loop_init` — libuv has no generic event loop, so there is
  nothing for it to silently do wrong. That is the desired failure and it
  follows from the source list, but no FreeBSD or riscv64 host was available to
  watch it happen. openssl's equivalent case *was* verified previously (its
  portable baseline), and it is the opposite shape: openssl has a generic C
  fallback, so it needs one, and libuv must not have one.
* **zstd's legacy decoders (`lib/legacy`, 7 files) are verified to build and
  link, not to work.** Exercising them needs a v0.5-era encoder to produce an
  old frame. The consumer's comment says so.
* **No Windows.** All three manifests claim `supports = ["*-apple-darwin",
  "*-*-linux-gnu", "*-*-linux-musl"]` and nothing more. libuv's `src/win/`
  tree was not harvested; zstd's assembly is explicitly rejected under MSVC by
  Harbour's own rules. musl is claimed on the strength of the sources rather
  than a build, which is the weakest claim in the three manifests —
  `supports` only warns, which is the right severity for exactly this.
* **The three platforms are two OSes and two architectures**, not a broad
  matrix. The `os`-axis and `arch`-axis layering claims are load-bearing and
  are covered by tests over synthetic harvests as well as by the real builds.

---

## 7. Verdict

**The marginal cost of adding a package is converging, and on this sample it
has essentially bottomed out at "write down the source list and the defines".**

The three data points, in the order they were attempted:

| | new Harbour features | new harvest work | failed build attempts | config lines |
|---|---|---|---|---|
| cJSON | 0 | 0 | 0 | 17 |
| libuv | 0 | one generalisation (§3) | 0 | 74 |
| zstd | 0 | 0 | 0 | 62 |

The single strongest piece of evidence is the *ordering*: libuv, chosen
specifically because platform-conditional source selection is the hardest thing
in the manifest model, needed no new manifest feature — and then zstd, chosen
for assembly plus multi-directory layout, needed no new tooling work either,
after libuv had already paid for the one generalisation. That is what
convergence looks like: the second hard package was cheaper than the first.

The residual per-package cost is judgement, not features, and it concentrates in
one place: **`when` blocks are additive with no "else"**, so the author must
make the unconditional case the one that is correct on unharvested platforms.
zstd's `ZSTD_DISABLE_ASM` is the miniature of it; openssl's portable baseline
was the expensive version. This is a deliberate design property, it is
documented in two places, and it is now named in the harvest README with a
worked example — but it is the thing that will keep costing a careful half-hour
per package, and it is where a future misbuild will come from.

The unflattering parts, stated without hedging:

1. **Harbour had a live bug that silently built the wrong source tree**, and it
   took only a container to find it. The audit's headline result is about
   manifests; this finding is about the build core, and it is more serious than
   anything the manifests exposed.
2. **`Harbour.lock` is still not portable.** Fixed the lying, not the format.
3. **curl's 793-line vendored config is not addressed** and nothing here
   suggests a path to retiring it. Three packages in a row not needing it is
   evidence about those packages, not a solution.
4. **`tools/harvest` was over-fitted to openssl** in a way that produced a
   correct-looking manifest with a hole in it. It took a second package with a
   different cross-cutting axis to notice. There may be a third axis (`env`,
   for glibc vs musl) waiting with the same shape.

---

## Appendix A: the manifests

### A.1 cJSON 1.7.18 — 25 lines

```toml
[package]
name = "cjson"
version = "1.7.18"
description = "Ultralightweight JSON parser in ANSI C"
license = "MIT"
homepage = "https://github.com/DaveGamble/cJSON"
requires = "hosted"
supports = ["*-apple-darwin", "*-*-linux-gnu", "*-*-linux-musl"]

[targets.cjson]
kind = "staticlib"
sources = ["cJSON.c", "cJSON_Utils.c"]
public_headers = ["cJSON.h", "cJSON_Utils.h"]

# Headers sit at the package root, not under include/, so the public include
# dir is the root itself. libm is public because cJSON's number printer calls
# floor/fabs and a static archive does not record that dependency.
[targets.cjson.public]
include_dirs = ["."]
system_libs = ["m"]

# ENABLE_LOCALES is cJSON's own build-time switch (use the locale decimal
# point when printing). It affects only cJSON's translation units.
[targets.cjson.private]
defines = ["ENABLE_LOCALES=1"]
```

Note what is absent: no `when` blocks, no conditional anything, no vendored
header. The easy case is genuinely easy.

### A.2 libuv 1.51.0 — 94 lines

Sources from `tools/harvest`, three cmake configures. The unconditional list is
the intersection; each OS layer adds its own event loop. Trimmed here to the
parts that are not file names.

```toml
[package]
name = "libuv"
version = "1.51.0"
description = "Cross-platform asynchronous I/O"
license = "MIT"
homepage = "https://libuv.org"
requires = "hosted"
supports = ["*-apple-darwin", "*-*-linux-gnu", "*-*-linux-musl"]

# Sources and defines below came from `tools/harvest`, reading a real cmake
# configure on each of macos/aarch64, linux/x86_64 and linux/aarch64. The
# unconditional list is the intersection: the files every unix platform
# compiles. Unlike openssl there is no portable fallback to harvest -- libuv
# has no generic implementation of its event loop, so a platform with no
# `when` block below fails to link on uv__platform_loop_init rather than
# building something subtly wrong. That is the right failure.
[targets.uv]
kind = "staticlib"
sources = [
  "src/fs-poll.c", "src/idna.c", "src/inet.c", "src/random.c",
  "src/strscpy.c", "src/strtok.c", "src/thread-common.c",
  "src/threadpool.c", "src/timer.c",
  "src/unix/async.c", "src/unix/core.c", "src/unix/dl.c", "src/unix/fs.c",
  "src/unix/getaddrinfo.c", "src/unix/getnameinfo.c",
  "src/unix/loop-watcher.c", "src/unix/loop.c", "src/unix/pipe.c",
  "src/unix/poll.c", "src/unix/process.c", "src/unix/proctitle.c",
  "src/unix/random-devurandom.c", "src/unix/signal.c", "src/unix/stream.c",
  "src/unix/tcp.c", "src/unix/thread.c", "src/unix/tty.c", "src/unix/udp.c",
  "src/uv-common.c", "src/uv-data-getter-setters.c", "src/version.c",
]   # one per line in the real file
public_headers = ["include/uv.h", "include/uv/*.h"]

[targets.uv.public]
include_dirs = ["include"]
system_libs = ["pthread"]

[targets.uv.private]
include_dirs = ["include", "src"]
defines = ["_FILE_OFFSET_BITS=64", "_LARGEFILE_SOURCE"]

# Platform source selection. libuv's split is by OS, not by architecture: the
# same four files build on every Linux arch, so this is keyed on `os` alone.
# (Keying it on os+arch, which is what a per-harvested-platform scheme
# produces, would leave linux/riscv64 matching no block and failing to link
# despite libuv supporting it.)
[[targets.uv.when]]
os = "linux"
sources = [
  "src/unix/linux.c",
  "src/unix/procfs-exepath.c",
  "src/unix/random-getrandom.c",
  "src/unix/random-sysctl-linux.c",
]
defines = ["_GNU_SOURCE", "_POSIX_C_SOURCE=200112"]

[[targets.uv.when]]
os = "macos"
sources = [
  "src/unix/bsd-ifaddrs.c",
  "src/unix/darwin-proctitle.c",
  "src/unix/darwin.c",
  "src/unix/fsevents.c",
  "src/unix/kqueue.c",
  "src/unix/random-getentropy.c",
]
defines = ["_DARWIN_UNLIMITED_SELECT=1", "_DARWIN_USE_64_BIT_INODE=1"]

# From libuv's own cmake link line, and needed by consumers of the archive:
# uv_dlopen wraps dlopen, and the older clock_gettime lives in librt.
[[targets.uv.surface.when]]
os = "linux"
[targets.uv.surface.when."link.public"]
libs = ["dl", "rt"]
```

`NDEBUG` appeared in all three harvests and was dropped: it is the cmake
`Release` build type leaking in, and profiles own that.

### A.3 zstd 1.5.7 — 87 lines, 3 of them conditional

Source list elided (37 files across `lib/common`, `lib/compress`,
`lib/decompress`, `lib/dictBuilder`, `lib/legacy`); the rest verbatim.

```toml
[package]
name = "zstd"
version = "1.5.7"
description = "Zstandard - fast real-time compression"
license = "BSD-3-Clause"
homepage = "https://facebook.github.io/zstd/"
requires = "hosted"
supports = ["*-apple-darwin", "*-*-linux-gnu", "*-*-linux-musl"]

# Sources came from `tools/harvest` reading a real cmake configure on
# macos/aarch64, linux/x86_64 and linux/aarch64. All three compile the same 37
# C files -- zstd's own sources use relative includes, so there is no per-
# platform include path and no configure-generated header at all.
[targets.zstd]
kind = "staticlib"
sources = [ ... 37 files ... ]
public_headers = ["lib/zstd.h", "lib/zstd_errors.h", "lib/zdict.h"]

# ZSTD_MULTITHREAD is compiled in, so a consumer of the archive needs pthread.
[targets.zstd.public]
include_dirs = ["lib"]
system_libs = ["pthread"]

# No private include dirs: zstd's sources include each other by relative path
# ("../common/zstd_internal.h"), so nothing needs to be on the search path.
# Verified by removing them and rebuilding, not assumed.
[targets.zstd.private]
defines = [
  "XXH_NAMESPACE=ZSTD_",
  "ZSTD_LEGACY_SUPPORT=5",
  "ZSTD_MULTITHREAD",
]

# The only architecture-specific file zstd has: a BMI2 Huffman decoder in
# assembly. Keyed on `arch` alone rather than on the (os, arch) pair the
# harvest emitted, because nothing in it is OS-specific -- it is `.S`, so the
# preprocessor sees the same guards on every x86_64 target.
#
# Note what is deliberately *not* here: cmake defines ZSTD_DISABLE_ASM on
# every non-x86_64 platform, and copying that into an `arch = "aarch64"` block
# is both redundant and a trap. zstd's own guard already requires
# `defined(__x86_64__)` (lib/common/portability_macros.h), so an architecture
# nobody harvested is correct without any block -- whereas enumerating the
# architectures that must disable assembly means the next one silently misses
# the list. `when` blocks are additive with no "else", so the base case has to
# be the one that is right everywhere.
[[targets.zstd.when]]
arch = "x86_64"
sources = ["lib/decompress/huf_decompress_amd64.S"]
```

## Appendix B: reproducing the verification

```sh
# harvest, per platform (cmake configure first; --source may be a container path)
cmake -S <pkg> -B build -DCMAKE_EXPORT_COMPILE_COMMANDS=ON <opts>
tools/harvest/harvest.py extract-cc --file build/compile_commands.json \
    --source <pkg> --os linux --arch x86_64 -o linux-x64.json
tools/harvest/harvest.py merge mac-arm.json linux-x64.json linux-arm.json \
    --package libuv --version 1.51.0 --flatten-into uv \
    --public-include-dir include --public-headers 'include/**/*.h' -o Harbour.toml

# harvest's own tests
python3 tools/harvest/test_layering.py

# a consumer, on Linux, with Harbour built inside the container
docker run --rm --platform linux/amd64 \
  -v <worktree>:/src:ro -v <audit-dir>:/w -w /w rust:1-bookworm \
  bash /w/in-container.sh     # CARGO_TARGET_DIR=/lt; bash -c, never bash -lc
```

Two traps worth keeping written down: do not mount or copy `target/` into the
container (cargo finds the macOS binary, considers it current, and you get
`Exec format error`), and use `bash -c` rather than `bash -lc` (a login shell
re-sources the profile and drops the image's PATH, so `cargo: command not
found` — which a narrow output grep will hide, making a failed build look like
a passing one).
