# openssl: generate, don't vendor — and the assembly was not the hard part

**Date:** 2026-09-12
**Status:** Decision + canary. `ci/canary/openssl/` builds and passes on five
(os, arch) cells — including a 32-bit platform that matches no `when` block
and therefore builds the portable C baseline — and runs green alongside the
other five canaries. The `tools/harvest` half is proved by running every
generator it emits.
**Scope:** shim openssl 3.5.4 as a Harbour package, choosing between
vendoring its generated assembly, generating it at build time, and building
`no-asm`.

---

## The decision, and the premise it corrected

**Chosen: run openssl's own generators as `prebuild` steps. Vendor nothing.**

The brief framed this as a choice about *assembly*: vendor ~5.6 MB of
generated `.s` per architecture, run perlasm at build time, or give up and
build `no-asm`. The first thing this exercise found is that **the framing is
wrong in a way that changes the answer**:

> openssl 3.x's *public headers* are generated too. A pristine
> `openssl-3.5.4.tar.gz` contains no `include/openssl/crypto.h`, no
> `safestack.h`, no `configuration.h` — 31 `.h.in` templates in total. They
> are filled in by openssl's own template engine (`util/dofile.pl`), which
> `die`s without a `configdata.pm`, which is written by `./Configure`.
>
> `crypto/sha/sha256.c` does not compile without them. Neither does anything
> else. So **the `no-asm` option is not a baseline that avoids the problem**
> — it hits exactly the same wall, one step earlier. There was never a
> strategy that did not have to answer "who generates openssl's sources".

Verified, not read: `for f in include/openssl/crypto.h
include/openssl/configuration.h configdata.pm; do [ -e upstream/$f ]; done`
is asserted *inside* `ci/canary/openssl/run.sh`, so if openssl ever starts
shipping them the canary fails rather than silently stopping testing.

Once that is true, the assembly question mostly answers itself: a strategy
that already has to run `util/dofile.pl` to get a header can run
`sha512-armv8.pl` to get a block function. The marginal cost of the assembly
is five more `prebuild` blocks per (os, arch), not a new mechanism.

### What was rejected, and why

**Vendoring the generated assembly (option 1).** Rejected, and the reasons
are not only about megabytes.

* It does not solve the problem it is proposed for. The generated *headers*
  would still have to come from somewhere, and vendoring 31 headers means
  vendoring the API of a security library, where a stale copy is a CVE with
  extra steps.
* It multiplies by architecture and by *ABI flavour*, not just by
  architecture. The same aarch64 assembly is generated differently for
  `ios64` and `linux64` — symbol decoration and section directives — so the
  axis is (arch × object format).
* And the *content* depends on the assembler the generator finds: the same
  script emits 49,912 or 97,936 bytes depending on whether `CC` is set (see
  the `prebuild` env finding below). A vendored `.s` freezes whichever
  answer the vendoring machine gave.
* And there are a lot of architectures to multiply by:
  `crypto/aes/build.info` alone names asm for x86, x86_64, ia64, sparcv9,
  mips32/64, s390x, armv4, aarch64, parisc, ppc32/64, c64xplus, riscv32/64
  and loongarch64.
* It is a fork nobody remembers taking, which is the argument
  `ci/canary/README.md` already makes about tarballs. A vendored `.s` is
  worse than a vendored tarball: it is a *transformation* of upstream that no
  upstream commit will ever update.
* Measured, for the record: generating openssl's full aarch64 assembly from
  the recipes `tools/harvest` extracts produces **2.3 MB across 22 files**
  (not 5.6 MB — that figure appears to be the whole `crypto/**/*.s` set for
  x86_64 including the `-mb-`/AVX512 variants). Either way it is per-arch and
  it is a mirror of upstream's generators.

**Building `no-asm` only (option 3).** Rejected as a *destination*, kept as
the *base layer* — which turns out to be the same arrangement the rest of
this repo already reached for other reasons. The unconditional source list in
the manifest is the portable C; the architecture `when` blocks add assembly
and, on x86_64, `exclude` the C it replaces. So `no-asm` is not an
alternative to the assembly strategy, it is the thing that makes an
unharvested architecture work, and this is verified on 32-bit ARM rather than
reasoned about: it matches neither arch block, builds those nine files and
nothing else, and computes the same digests. What was rejected is
*stopping* there, because for a
crypto library the accelerated path is most of the point, and because the
assembly is what makes the package interesting as a Harbour test.

**Shelling out to `./Configure` (rejected by the repo owner before this
started).** Worth restating what was actually avoided, because the line is
finer than it looks. Harbour does not run `Configure`, does not read a
generated `Makefile`, does not read openssl's `configdata.pm`, and does not
produce or consult `build.info`. What it *does* run is two of openssl's
scripts — `util/dofile.pl` (a `Text::Template` front end) and the perlasm
generators (each of which takes `flavour` and an output path and nothing
else). The configuration those scripts need is **18 lines of perl written by
the manifest**, listing 14 variables found by grepping the templates:

```
target builddir sourcedir perl_platform version full_version major minor
patch prerelease release_date shlib_version build_metadata b64l b64 b32
processor rc4_int openssl_{sys,api,feature}_defines   ($config)
dso_scheme perl_platform                             ($target)
```

That is the substitution: **`Harbour.toml` replaces `Configure`, and openssl's
generators stay generators.** A code generator is not a build system. The
test is whether Harbour decides *what* is built — and it does: the source
list, the defines, the per-platform selection and the link surface are all in
the manifest, and nothing reads openssl's opinion about any of them.

---

## What was built

`ci/canary/openssl/` — manifest, `run.sh`, and a consumer that computes real
digests. **One target, `crypto`, covering openssl's SHA-1/SHA-2 and AES
primitives: 9 portable C files plus 5 generated per architecture.** It is a
slice, stated as such in the manifest's first paragraph, and the reason is
scope rather than a limitation of the approach — §"What remains" shows the
other 1,090 sources are mechanical.

### Verified by running

Every cell below is `ci/canary/openssl/run.sh` end to end: fetch the pinned
tarball, generate, build, assert a no-op rebuild, assert translation-unit
count, assert objects by name, `nm` the objects, then build and run the
consumer.

| platform | how | TUs | consumer checks | result |
|---|---|---|---|---|
| macos/aarch64 | native | 15 | 19 | pass |
| macos/x86_64 | `--target-triple`, run under Rosetta | 15 | 17 | pass |
| linux/aarch64 | `docker --platform linux/arm64` | 15 | 19 | pass |
| linux/x86_64 | `docker --platform linux/amd64` | 15 | 17 | pass |
| linux/arm (32-bit) | cross-compiled in the amd64 container, run under `--platform linux/arm/v7` | 9 | 16 | pass — matches no `when` block |

The counts differ because the architecture-specific assertions are
architecture-specific: 16 known-answer checks everywhere, plus one for the
capability symbol on x86_64, plus two more on aarch64 for the direct
`aes_v8_*` calls. The 32-bit row got the 16 and nothing else, which is the
whole point of it: **an architecture the shim knows nothing about builds the
portable C and computes the same digests.** Its consumer printed

```
OK openssl 3.5.4 slice: 16 known-answer checks (...)
     no `when` block matches this architecture, so the portable C
     baseline is what produced the digests above -- correct, slower
```

and the library build was checked the other way round from an assembly
platform: exactly 9 objects, `aes_core.o` **present**, no object matching
`*armv8*` or `*x86_64*` anywhere, and `sha256.o` **defining**
`sha256_block_data_order` rather than importing it. `include/crypto/bn_conf.h`
said `#define THIRTY_TWO_BIT`, so the 32-bit `configdata.pm` rewrite fired.
Harbour also emitted the right advisory, which is worth recording because it
is the first time `supports` has been seen to do its job here:

```
WARN package `openssl` does not list `arm-unknown-linux-gnueabihf` among the
     targets it supports (*-apple-darwin, *-*-linux-gnu, *-*-linux-musl).
     The build will proceed -- the list records what has been built, not what
     can be -- but nothing has verified this combination.
```

**How that row was run, because the method is not the obvious one.** Building
natively under `docker --platform linux/arm/v7` does not work on this host:
Docker runs 32-bit ARM under QEMU (x86_64 gets Rosetta instead), and
`cargo build` of Harbour inside it ran at roughly a fifth of real time and
did not finish in two attempts. Compilation is the expensive part and
*execution* is not, so the two were split: Harbour runs in the x86_64
container and cross-compiles to `arm-unknown-linux-gnueabihf`, and the
resulting 32-bit ELF is then executed in a `linux/arm/v7` container. Anyone
reproducing this should do the same rather than waiting on a native build.

### What the consumer actually asserts

Not "it worked". Byte-exact known answers, and symbols that only exist if the
right sources were selected.

* **SHA-1/256/512 of `"abc"`** against FIPS 180-4:
  `a9993e364706816aba3e25717850c26c9cd0d89d`,
  `ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad`,
  `ddaf35a1...54ca49f`.
* **SHA-1/256/512 of one million `'a'`**:
  `34aa973cd4c4daa4f61eeb2bdbad27316534016f`,
  `cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0`,
  `e718483d...ad8cc09b`. Fed in 1000-byte chunks, and SHA-256 again in
  **7-byte** chunks, so the buffer-carry path in `md32_common.h` runs. A
  block function with a wrong loop bound passes on `"abc"` and fails here.
* **AES-128/192/256, FIPS 197 appendix C**: plaintext
  `00112233445566778899aabbccddeeff` → `69c4e0d86a7b0430d8cdb78070b4c55a`,
  `dda97ca4864cdfe06eaf70a0ec0d7191`, `8ea2b7ca516745bfeafc49904b496089`,
  plus a decrypt round trip for each.
* **AES-128-CBC, NIST SP 800-38A F.2.1**: all four blocks of ciphertext
  compared as one 64-byte string (`7649abac...7586e1a7`), decrypted back, and
  — separately — asserted *different from the plaintext*, because a cipher
  that did nothing passes every round-trip check.
* **Architecture-only symbols, referenced strongly.** `OPENSSL_armcap_P`
  (from `crypto/armcap.c`) on aarch64, `OPENSSL_ia32cap_P` (from
  `crypto/cpuid.c`) on x86_64. Both live in an arch `when` block and nowhere
  else, so **if the block stops matching the consumer does not link.** On
  aarch64 it also calls `aes_v8_set_encrypt_key`/`aes_v8_encrypt` from
  `aesv8-armx.S` and checks the FIPS 197 vector through the ARMv8 AES
  instructions — the only path that reaches that object at all in this slice,
  since openssl normally routes to it through EVP.
* It reports the capability word it measured, which is the one thing a digest
  cannot tell you: `0x0000987d` on macOS/aarch64 (sysctl), `0x000008fd` on
  linux/arm64 (getauxval), `0x7ed8320f4f8b8f15` on linux/x86_64. Different
  values on the same architecture means `armcap.c` and `arm64cpuid.S` are
  really running, not returning a constant.

### What `run.sh` asserts that the consumer cannot

The failure mode this package has and no other canary does: **the assembly
can be absent and everything still passes.** openssl falls back to C, the
archive links, the digests are correct, only slower. So:

* **Objects by name, per architecture.** `sha1-armv8.o`, `sha256-armv8.o`,
  `sha512-armv8.o`, `aesv8-armx.o`, `arm64cpuid.o`, `armcap.o` on aarch64;
  `sha1-x86_64.o`, `sha256-x86_64.o`, `sha512-x86_64.o`, `aes-x86_64.o`,
  `x86_64cpuid.o`, `cpuid.o` on x86_64.
* **An object asserted *absent*.** On x86_64, `aes_core.o` must not exist:
  `aes-x86_64.s` defines `AES_encrypt` itself, so the `exclude` in that
  `when` block is load-bearing. On aarch64 the same file must be *present*,
  because there the assembly adds rather than replaces.
* **`nm`, both directions.** `sha256.o` must **not** define
  `sha256_block_data_order` on an assembly platform — that is what proves
  `SHA256_ASM` took effect and the C fallback was compiled out — and the
  corresponding `.o` from the perlasm must define it.

**All three checks were proved to fail on a broken manifest, not assumed to.**
Two deliberate breakages, each run to completion:

| breakage | TU count | `nm` check | digests | caught by |
|---|---|---|---|---|
| delete `aesv8-armx.S` from the aarch64 block | 14, **fails** | n/a | would pass | count |
| delete `SHA256_ASM` from the aarch64 block | 15, **passes** | **fails** | would pass | `nm` |

The second row is the whole argument for the `nm` check: the count is right,
the objects are all present, the digests are correct, and the library is
running its C block function while carrying an unused assembly object.

---

## Accounting: vendored vs measured vs declared

| | count | where |
|---|---|---|
| vendored files | **0** | — |
| generated at build time | 31 headers + 5 assembly sources per arch | openssl's own `dofile.pl` and perlasm |
| declared in the manifest | 9 source names, 5 per-arch source names, 4 define sets, 18 lines of perl config | `Harbour.toml` |
| measured by a probe | **0** | see below |

**Probes cannot produce `opensslconf.h`, and the reason is structural rather
than a missing probe kind.** This was checked rather than assumed:

* `emit = { header = "..." }` writes **one** header. openssl needs 31, by
  name, including three (`configuration.h`, `bn_conf.h`, `dso_conf.h`) that
  are config and 28 that are API.
* The 28 API headers are not a define list at all. `crypto.h.in` and
  `safestack.h.in` run perl to *generate C* — `generate_stack_macros`,
  `generate_lhash_macros` — so there is no set of probe answers that produces
  them. This is not something a `type` or `flag` probe kind would change.
* Of the config three, exactly **one line** is a genuine toolchain
  measurement: `SIXTY_FOUR_BIT_LONG` vs `THIRTY_TWO_BIT` in `bn_conf.h`,
  which is `sizeof(long)` — a question `check_sizeof` already answers
  perfectly. And it cannot be used, because **a probe answer cannot reach a
  `prebuild` generator** (MANIFEST.md, "Not yet implemented"). The manifest
  therefore hardcodes 64-bit and rewrites `configdata.pm` under
  `arch = "arm"`, `arch = "armv7"` and `arch = "i686"` — enumerating
  architectures, which is
  the trap `tools/harvest/README.md` names. Accepted here only because
  bitness has no "right everywhere" answer: one of the two has to be the
  base.

So the honest summary is that probes and generated sources are **adjacent
solutions to the same problem that cannot currently be composed**, and
openssl is the package that shows why that matters. Curl's 793-line
`curl_config.h` is answerable by probes because it *is* a define list.
openssl's is not.

---

## Harbour features that do not exist, found by needing them

1. **A probe answer cannot be passed to a `prebuild` generator.** Documented
   as not implemented; openssl is the first package to actually want it, for
   the one line above. Without it, the only genuine measurement in openssl's
   generated config is expressed by enumerating architectures.
2. **`prebuild` steps get no `HARBOUR_*` environment.** The brief states that
   the `prebuild`/`CustomStep` machinery provides `HARBOUR_ARTIFACT_DIR` and
   `HARBOUR_PACKAGE_ROOT`. That is true of `CustomStep` (`recipe`), set in
   `src/builder/native.rs:682`; it is **not** true of
   `[[targets.X.prebuild]]`, whose `PrebuildStep::run`
   (`src/builder/plan.rs:187`) passes only the manifest's own `env`. Nor is
   the target triple passed. Consequence for this shim: a generator cannot
   learn anything about the platform it is generating for except through the
   `when` block that selected it — which is workable, and is why the flavour
   is a literal argument in eight separate blocks, but it means a generator
   can never be written once and parameterised.

   This one has teeth. openssl's x86_64 perlasm scripts *run the assembler*
   to decide which encodings it accepts, reading `$ENV{CC}`. Measured on
   3.5.4:

   ```
   perl crypto/sha/asm/sha512-x86_64.pl elf out.s     ->  49,912 bytes
   CC=cc perl crypto/sha/asm/sha512-x86_64.pl elf ... ->  97,936 bytes
   ```

   The short one is missing the AVX2 and SHA-extension paths entirely, and it
   assembles, links and computes correct digests. So a `prebuild` step that
   is not told the compiler produces a *silently slower library* — the exact
   failure shape this canary exists for, arriving through the generator
   rather than through the source list. The manifest sets `env = { CC = "cc" }`
   on eight blocks to avoid it. A generator that could ask Harbour for the
   real compiler would not need to guess.
3. **`emit` writes one header.** Noted above. openssl needs 31, and no
   arrangement of `-D` substitutes because its sources `#include` them by
   name.
4. **`prebuild` has no shell, therefore no output redirection.** This is
   correct design, and it has a concrete cost: openssl's `.c.in`/`.h.in`
   recipes are all `... > $@`, so they are not expressible as written. The
   way out existed and had to be found — `util/dofile.pl -i.in` writes the
   file itself — and it is the only reason this shim is possible without
   `sh -c`. Worth recording as a pattern: when a generator only writes to
   stdout and has no in-place mode, `prebuild` cannot drive it at all.
5. **Harbour's cross-toolchain probing misses Debian's armhf compiler.**
   Asking for `armv7-unknown-linux-gnueabihf` on a Debian container with
   `gcc-arm-linux-gnueabihf` installed fails:

   ```
   error: no toolchain found for target `armv7-unknown-linux-gnueabihf`
   probed: armv7-unknown-linux-gnueabihf-gcc, armv7-unknown-linux-gnueabihf-clang,
           armv7-linux-gnueabihf-gcc, clang -target armv7-unknown-linux-gnueabihf
   ```

   The binary Debian ships is `arm-linux-gnueabihf-gcc`, which none of those
   four spellings matches. `arm-unknown-linux-gnueabihf` works, because its
   `arm-linux-gnueabihf-gcc` guess hits. Not fixed here — it is a change to
   toolchain discovery, outside this task's files — but it is the reason the
   32-bit row above is keyed on `arch = "arm"`.

6. **`arch` is the literal triple component, and that makes a `when`
   condition machine-specific in a way nothing warns about.** The *same*
   Debian cross compiler is `arch = "arm"` through one triple and
   `arch = "armv7"` through another. A manifest keyed on one silently misses
   the other. This was caught before it could bite: the only triple whose
   toolchain Harbour could actually find was `arm-unknown-linux-gnueabihf`,
   whose `arch` is `arm`, while the manifest's 32-bit block said `armv7` —
   so a build would have produced a `bn_conf.h` claiming
   `SIXTY_FOUR_BIT_LONG`. The shim now lists `arm`, `armv7` and `i686`, and is still wrong
   for `armv7r`, `thumbv7`, `mips` and `powerpc`. This is the enumeration
   trap `tools/harvest/README.md` warns about, observed rather than
   predicted — and the argument for feeding a `sizeof` probe to the generator
   instead.

7. **Not a gap, but a sharp edge:** `prebuild` steps re-run on every build
   (documented). For openssl that is 7 perl invocations and ~0.4 s per build
   (2 unconditional, 5 from the matching (os, arch) block),
   and it is harmless *only* because the generators are deterministic:
   fingerprints are taken after regeneration, so byte-identical output leaves
   everything up to date. The canary asserts the second build recompiles
   nothing, which is what makes that claim checkable.

## A real defect in the output, found by reading a link

Not a Harbour bug and not openssl's either, but the kind of thing this
project keeps saying a green build will not tell you:

```
/usr/bin/ld: warning: sha512-x86_64.o: missing .note.GNU-stack section
             implies executable stack
```

openssl 3.5.4 emits no `.note.GNU-stack` from perlasm and passes no assembler
flag for it — `grep -rn noexecstack` over the entire tree finds nothing — so
an ELF link of its assembly produces a binary with a **writable, executable
stack**. Every assertion still passed. The manifest now carries
`cflags = ["-Wa,--noexecstack"]` under `os = "linux"`, and the warning is
gone from the linux/x86_64 run (`grep -c GNU-stack` on the log: 1 before, 0
after). Note the warning named only `sha512-x86_64.o` although five assembly
objects were built — so it is not a property of that one file; ld appears to
report it once per link. That is an inference from one log, not something
checked against ld's source.

---

## What remains, and why it is mechanical

The slice is 15 translation units. Full libcrypto is ~1,105. The claim that
the rest is mechanical is backed by running, not by estimating:

`tools/harvest` gained `merge --emit-prebuild`, which reads the generator
*recipes* out of the build system's own rules and writes
`[[targets.X.when.prebuild]]` blocks. On a real openssl 3.5.4 configured for
`darwin64-arm64`:

* 1,083 sources and **22 generated** resolved;
* all 22 emitted as prebuild blocks with the right flavour (`ios64`), the
  right output path, and `env = { CC = 'cc' }`;
* the resulting TOML parses;
* **all 22 blocks were then executed against a pristine tarball** with the
  program, args, env and cwd Harbour's `PrebuildStep` uses, and all 22
  produced their declared outputs — 2.3 MB of aarch64 assembly from a tree
  where `Configure` had never run.

What `--emit-prebuild` still refuses is the ten `.c.in` template sources,
because their recipes redirect. That refusal is deliberate and tested: a tool
that emitted 22 of 32 generators would produce a manifest that links and
computes correct answers with no witness. The workflow is therefore:
Configure once as an oracle → generate the ten template sources with
`dofile.pl -i.in` → harvest → `merge --emit-prebuild` → hand-write the two
template steps. Four of the five stages are now mechanical.

Four new tests in `tools/harvest/test_layering.py` cover it, including the
asymmetry that matters most: **the generated source layers on `arch`, the
generator that writes it does not.** openssl's 33 arm assembly files are one
`arch = "aarch64"` block and two generator blocks, one per OS. Layering the
generator would send `ios64` to Linux; pinning the source would duplicate 33
lines per OS.

## Windows

**Unsupported, and it fails loudly rather than being undefined.**

`supports` lists only `*-apple-darwin`, `*-*-linux-gnu`, `*-*-linux-musl`.
Beyond that advisory warning, the behaviour on Windows is determined by
Harbour's own rule: a target whose resolved sources include any `.s`/`.S`
fails with a dedicated error under MSVC (`src/builder/plan.rs:767`), naming
the file and pointing at clang or gcc. Because the arch `when` block adds the
assembly *before* that check runs, an x86_64 or aarch64 Windows build hits it
and never reaches the C baseline.

Honest about what was and was not run: **no canary runs on Windows** — the
canary CI job is `ubuntu-latest` only, deliberately, because `windows-latest`
bills at 2x — so the above is read from `src/builder/plan.rs` and from
MANIFEST.md, not observed. The only Windows-adjacent thing actually executed
was `--target-triple x86_64-pc-windows-msvc` from macOS, which stops earlier
still, at toolchain discovery: `no toolchain found ... probed: cl.exe
(requires vswhere; not yet supported)`. What is *not* known: whether the
portable C baseline would build under MSVC at all if the assembly were
removed. openssl's `e_os.h` and `dso_conf.h` would need `OPENSSL_SYS_WINDOWS`
and a `win32` DSO scheme, neither of which this manifest provides, so the
likely answer is no — but that is a prediction, and it is labelled as one.

---

## Proved by running vs inferred by reading

**Proved by running.** Every row of the platform table: five
(os, arch) cells, each fetching the pinned tarball, generating 31 headers and
per-arch assembly from nothing, building, rebuilding with zero recompiles,
asserting objects by name, `nm`-ing two of them, and running a consumer that
compares digests and ciphertexts byte for byte. All six canaries green
together via `run-all.sh`. Both deliberate breakages, run to completion, with
the count catching one and `nm` catching the other. The `--emit-prebuild`
output, parsed and then *executed* — 22 generators, 22 outputs. The 32-bit
`configdata.pm` rewrite and the `THIRTY_TWO_BIT` it produces, plus its
failure when its pattern is absent. `harbour build --plan` running the
generators. The `no-toolchain-found` messages for the MSVC and
`armv7-*-gnueabihf` triples. The absence of
`crypto.h`/`configuration.h`/`configdata.pm` from the tarball. The
`.note.GNU-stack` warning, and its disappearance after `-Wa,--noexecstack`.
The 49,912-vs-97,936-byte difference `CC` makes to generated assembly.

**Inferred by reading.**

* The MSVC assembly rejection, its message and the order of the checks —
  read in `src/builder/plan.rs`, never executed.
* That `PrebuildStep::run` passes no `HARBOUR_*` env — read in
  `src/builder/plan.rs:187`; confirmed by the absence of any `env(` call, not
  by observing a generator fail to find the variable.
* That the full 1,105-source libcrypto would build. What is proved is that
  its *sources and generators* are derivable and that the generators run. The
  library has not been compiled, and there is no basis here for claiming the
  remaining 1,090 compiles and links cleanly.
* That musl works. Claimed in `supports` on the strength of the sources, as
  the other canaries do. Not built.
* FreeBSD and any other OS: would take the portable baseline, which is the
  intent, but openssl's `e_os.h` paths for those platforms were not
  exercised.
* The 5.6 MB per-architecture figure for vendored assembly, from the brief.
  What was measured here is 2.3 MB for aarch64.
* **That the 32-bit binary is *fast enough to be useful*.** It was executed
  under QEMU; only its output was checked. Nothing here measures the cost of
  the C fallback against the assembly on any platform.

## Corrections to the brief

1. **"The zstd canary already does this (`huf_decompress_amd64.o` present on
   x86_64 only); follow it."** It does not. `ci/canary/zstd/run.sh` calls
   `canary_standard_run` with `"38|37"` and asserts a translation-unit
   *count*; the string `huf_decompress_amd64` appears nowhere in the
   repository. The by-name object check is described in the 2026-09-07 audit,
   where it was done by hand, and it was never carried into the canary.

   Fixed here, for zstd as well as openssl, and the shared helpers now live in
   `ci/canary/lib.sh` so the two cannot drift. zstd's version was verified by
   breaking its manifest three ways, and **the count caught none of them**:

   | breakage | TUs | count verdict | caught by |
   |---|---|---|---|
   | `ZSTD_DISABLE_ASM` added on x86_64 | 38 | **passes** | `canary_defines_symbol` |
   | `.S` removed from the x86_64 block | 37 | **passes** — 37 is legitimate on aarch64, so `"38\|37"` accepts it | `canary_require_object` |
   | `arch` condition removed (aarch64 host) | 38 | **passes** — 38 is legitimate on x86_64 | `canary_refuse_object` |

   The first row is the sharpest: the whole body of `huf_decompress_amd64.S`
   is inside `#if ZSTD_ENABLE_ASM_X86_64_BMI2`, so with the define it
   assembles to an **empty object**. 38 translation units, the object present
   by name, every round trip byte-exact, and no accelerated Huffman loop. The
   second and third rows show something worth naming on its own: a
   `|`-separated count set legitimises both values everywhere, so it cannot
   catch a platform getting the *other* platform's answer.
2. **The three strategies are not independent, and `no-asm` is not the
   simple baseline.** All three need openssl's generated headers, so "build
   the no-asm configuration" is not the low rung it appears to be — it is the
   same problem with the interesting part removed.
3. **`HARBOUR_ARTIFACT_DIR` / `HARBOUR_PACKAGE_ROOT` are not available to
   `prebuild`.** They are set for `recipe`/`CustomStep` only.
4. **The CI comment claiming openssl is "explicitly out" of the canary job on
   cost grounds** is now wrong for this shim and has been updated: 15
   translation units and a 55 MB download, not 1,100 sources.
