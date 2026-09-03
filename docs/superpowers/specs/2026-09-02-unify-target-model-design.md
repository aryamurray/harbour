# Design: Unify the Target Model

**Date:** 2026-09-02
**Status:** Draft — awaiting review
**Project:** A of A→B→C (see "Relationship to other work")

---

## Problem

Harbour cannot correctly build for a target other than the host, and the
reason is structural rather than a missing feature.

### 1. Two competing target types

| Type | Shape | Role |
|---|---|---|
| `core::abi::TargetTriple` (`src/core/abi.rs:13`) | `{arch, vendor, os, env}` | feeds the ABI fingerprint / cache key |
| `builder::shim::intent::TargetTriple` (`src/builder/shim/intent.rs:233`) | `{triple: String}` | carried through the build path |

`src/bin/harbour/commands/build.rs:9-11` imports both, aliasing one as
`AbiTargetTriple`. The build path carries an opaque string while the cache key
carries parsed components, so there is no single source of truth for "what are
we building for."

There is in fact a **third** notion of the host triple:
`ops/verify/harness.rs:194-229` hand-rolls `get_host_triple()` as a 7-way
`#[cfg(all(target_os, target_arch))]` table returning literal triple strings,
independent of both types above. All three must collapse into one.

`intent::TargetTriple` repeats the positional bug: `os()` (`intent.rs:266`)
returns `split('-')` index 2 and `arch()` (`intent.rs:272`) returns index 0.
Its `is_host()` (`intent.rs:247`) is the worst offender in the codebase — it
gates on the *host* `cfg!` at compile time of the Harbour binary itself and
then substring-matches the target string (`contains("linux")`,
`contains("msvc")`), never comparing architecture at all.

### 2. Positional parsing is wrong for bare metal

`TargetTriple::parse` (`src/core/abi.rs:50`) splits on `-` and assigns
`parts[0..3]` to arch/vendor/os positionally:

| Input | Parsed as | Correct? |
|---|---|---|
| `x86_64-unknown-linux-gnu` | arch=x86_64, vendor=unknown, os=linux, env=gnu | yes |
| `arm-none-eabi` | arch=arm, vendor=none, **os=eabi** | no — ABI in the OS slot |
| `thumbv7em-none-eabihf` | arch=thumbv7em, vendor=none, **os=eabihf** | no |
| `avr` | `None` (rejected) | no |

The type cannot express "no operating system", so every OS-conditional decision
silently misfires on embedded targets.

### 3. Toolchain detection is host-only

`detect_toolchain()` (`src/builder/toolchain/detect.rs:38`) takes no arguments.
There is no path to select `arm-none-eabi-gcc` for a requested target.

### 4. `--target-triple` is parsed and then explicitly discarded

The chain is: clap arg (`cli.rs:238`) → `build.rs:73` converts it →
`BuildOptions.target_triple` (`build.rs:94`) → folded into `BuildIntent`
(`harbour_build.rs:215`) → **dropped at `harbour_build.rs:306` by
`let _ = intent;`**, commented "Store intent for potential later use."

Downstream of that line, `plan.rs`, `native.rs`, and `executor.rs` contain
**zero** occurrences of `target_triple` or `TargetTriple`. `BuildContext::new`
(`context.rs:73`) has no target parameter and unconditionally calls
`detect_toolchain()` and `TargetTriple::host()` (`context.rs:84`).

Before it is dropped, the requested triple is used for exactly two things: a
capability gate that rejects backends lacking cross support
(`validation.rs:207` → `harbour_build.rs:259-265`), and skipping harness
*execution* in verify (`harness.rs:160-169`). Neither influences a single
compiler flag.

### 5. `toolchain.target` is a config field that does nothing

`harbour toolchain override --target <triple>` writes
`ToolchainSettings.target` (`commands/toolchain.rs:109`) and it is printed back
at `:160` and `:249` — but `try_detect_from_config` (`detect.rs:72`) reads
`cc`/`cxx`/`ar` and **never reads `target`**. The CLI presents a working
cross-target setting that silently has no effect. This is a user-visible lie
and A should either honour it or remove it.

---

## Goals

1. One canonical target type, used by the build path and the ABI key alike.
2. Parse **any** C/C++ target triple. Never reject; always round-trip losslessly.
3. Represent freestanding/bare-metal targets correctly (no OS).
4. Target-aware toolchain selection, with an open-ended fallback for targets
   Harbour has never heard of.
5. `--target` flows CLI → build plan → toolchain → ABI fingerprint → output path.

## Non-goals (deliberately deferred)

- Target-conditional manifest sections (`[target.<triple>.dependencies]`) — Project B.
- Redesigning `PlatformSupport` / the registry shim schema — Project B.
- Shipping built-in specs for every MCU. The convention fallback covers the tail.
- Running cross-compiled tests (QEMU). Cross verification is compile+link only.

---

## Design

### 1. `core::target::triple::TargetTriple`

```rust
pub struct TargetTriple {
    /// Exactly as the user wrote it. Always preserved.
    raw: String,
    arch: String,
    vendor: Option<String>,
    /// None => freestanding / bare metal.
    os: Option<String>,
    env: Option<String>,
    /// False if any component was unrecognized; drives a diagnostic, not an error.
    fully_recognized: bool,
}
```

**Components are open strings, not enums.** "Any C/C++ target" means an
unrecognized architecture must be representable. An enum would force an
`Other(String)` variant that every match site has to handle, which is strictly
worse than an open newtype with recognition helpers.

**Parsing recognizes, it does not count.** Following `llvm::Triple`:

1. Split on `-`; drop empty trailing tokens. Component 0 is **always** the
   arch, recognized or not — an unknown arch is preserved verbatim, never an
   error.
2. For each remaining component left-to-right, assign it to the first
   *not-yet-filled* slot among `vendor → os → env` whose known-value set
   contains it. **Override: `elf` is only ever assigned to `env`, never to
   `os`, even when the `os` slot is open.** In every real triple, `elf` denotes
   object format, not an operating system. This single rule fixes the three
   most dangerous false negatives (`msp430-elf`, `xtensa-esp32-elf`,
   `riscv32-esp-elf`).
3. A component matching no known set still fills the next open slot, preserving
   the literal string. Leftovers after `env` go to an `extra` bucket rather
   than being dropped or erroring.

`none` is genuinely ambiguous — vendor in `arm-none-eabi`, OS in
`riscv32imac-unknown-none-elf` — and slot-filling resolves it without string
special-casing. Both readings agree on `is_bare_metal`, which is the saving
grace.

**Two separate predicates, not one.**
`is_bare_metal := os is absent OR os == "none"`, evaluated on the *parsed*
field. Note that `"unknown"` is **not** `"none"`: `wasm32-unknown-unknown` is
not bare metal, and conflating the two is an easy bug. Separately,
`is_embedded_rtos := os ∈ {zephyr, rtems, nuttx, vxworks}` — those targets have
a real OS and are not freestanding, but still need a cross toolchain and cannot
use the host libc. Overloading one flag for both loses that distinction.

`parse` is **infallible** — it returns a `TargetTriple`, never an error. An
unparseable-looking triple still yields something usable, because the toolchain
convention fallback may well work anyway.

Two string forms, and the distinction matters:

- `as_str()` returns `raw` — lossless round-trip, used for display and for
  invoking the toolchain.
- `canonical()` returns the normalized 4-component form — used for the **ABI
  fingerprint**, so that `arm-none-eabi` and `arm-unknown-none-eabi` do not
  produce two cache entries for one target.

Structured predicates replace substring matching: `is_bare_metal()`,
`is_windows()`, `is_darwin()`, `env_is("musl")`.

### 2. Delete `builder::shim::intent::TargetTriple`

Replace all uses with the canonical type. Any site currently inspecting the
inner string by substring gets a predicate instead.

### 3. Target specs: four-layer resolution, never fails

A triple resolves to a `TargetSpec { toolchain_prefix, cflags, libc, linkage_defaults }`:

| Layer | Source | Priority |
|---|---|---|
| 1 | `[target.<triple>]` in `.harbour/config.toml` | highest — user override always wins |
| 2 | custom spec file: `targets/<triple>.toml` in the project, or `--target-spec <path>` | project-local exotic targets |
| 3 | built-in table for common triples | curated defaults |
| 4 | convention: `<triple>-gcc`, then `clang --target=<triple>` | the tail |

**Layer 4 is not a single convention — it is an ordered candidate list.**
Research showed `<triple>-gcc` is wrong for *most* targets, not a few:

| Triple | Actual binary | What breaks |
|---|---|---|
| `thumbv7em-none-eabihf` | `arm-none-eabi-gcc` | one binary serves every `thumbv*`; the core comes from `-mcpu`, not the name |
| `riscv32imac-unknown-none-elf` | `riscv32-unknown-elf-gcc` | arch extension suffix dropped; `none` collapses to `elf` |
| `aarch64-unknown-linux-gnu` | `aarch64-linux-gnu-gcc` | Debian/Ubuntu drop the vendor |
| `x86_64-unknown-linux-musl` | `x86_64-linux-musl-gcc` | vendor dropped |
| `x86_64-pc-windows-gnu` | `x86_64-w64-mingw32-gcc` | shares nothing with the triple |
| `armv7-linux-androideabi` | `armv7a-linux-androideabi21-clang` | clang, API level spliced in, arch respelled |
| `x86_64-apple-darwin` | none — `xcrun clang -target ...` | no prefixed binary exists |
| `x86_64-pc-windows-msvc` | none — `cl.exe` via `vswhere` | not a gcc-family lookup at all |
| `xtensa-esp32s3-elf` | `xtensa-esp32s3-elf-gcc` | exact match — one of the few |

So layer 4 generates an **ordered list of plausible binary names** and probes
each, confirming the winner with `-dumpmachine`:

1. exact `<raw>-gcc` / `<raw>-clang`
2. vendor dropped
3. arch extension suffix normalized (`riscv32imac`→`riscv32`, `thumbv*`→`arm`)
4. `os=none` collapsed to `-elf`
5. family special cases: mingw, Android NDK, Apple `xcrun`, MSVC `vswhere`

Layer 3's built-in table exists to short-circuit this for known families, and
layers 1–2 let a user pin an exotic toolchain directly. Nothing rejects an
unknown triple — worst case every candidate misses and the error names what was
probed.

Two flag-derivation facts that constrain the `TargetSpec` shape: Xtensa has no
`-mcpu` equivalent (chip selection *is* the compiler binary), and AVR/MSP430
triples carry no chip granularity at all (`-mmcu=atmega328p` cannot be derived
from `avr`). So `TargetSpec` must allow flags that come from outside the triple,
and A must not assume every family exposes a core-selection flag.

### 4. `detect_toolchain(target: Option<&TargetTriple>)`

- `None` → host detection, current behaviour.
- `Some(t)` → resolve the `TargetSpec`, locate the compiler, and verify it by
  invoking `-dumpmachine`.

**Hard rule: never fall back to the host compiler for a cross target.** Doing so
produces host binaries labelled as target binaries — a silent, badly-corrupting
failure. A missing cross toolchain is an actionable error naming the expected
binary and how to install it.

### 5. Plumbing and output layout

`--target` reaches toolchain selection, the ABI fingerprint, and the output
directory, which becomes `.harbour/target/<triple>/<profile>/`, mirroring Cargo.
Host builds keep the current path so nothing existing moves.

### 6. Host-vs-target cfg hygiene

Every `cfg!(target_os = ...)` / `std::env::consts::{ARCH,OS}` site is classified
as either *about the machine we run on* (process spawning, path separators,
executable extension — correct as-is) or *about the machine we compile for*
(compiler flags, library naming, linkage — a bug). The second category is
rewritten to query the target triple. This is the substantive bug class the
refactor exists to fix; the type unification is what makes it possible.

### 7. The concrete host-for-target bugs A must fix

The inventory classified every `cfg!(target_os)` / `env::consts` site as either
*about the machine we run on* (correct) or *about the machine we compile for*
(a bug). The second category:

| Site | Bug |
|---|---|
| `builder/toolchain/gcc.rs:277` | `shared_lib_extension()` picks `.dylib` vs `.so` from `cfg!(target_os="macos")` — the **host**. Cross-building for macOS from Linux emits `.so`. |
| `builder/toolchain/detect.rs:224-235` | Host `env::consts::ARCH` is passed to `vcvarsall.bat <arch>`, so an x86_64 host can never configure an arm64-MSVC environment even though `vcvarsall` accepts cross arguments. |
| `core/surface.rs:393-394` | `TargetPlatform::host()` from `env::consts` is what manifest surface conditions are evaluated against (`context.rs:87`) — so which defines and flags apply is decided by the host, not the build target. |
| `ops/ffi_bundle.rs:460-477`, `:485-651` | RPATH-rewrite strategy and runtime-dep collection (`patchelf`/`otool`/`dumpbin`) dispatch on host `#[cfg(target_os)]`, but describe the **artifact's** target. |
| `ops/ffi_bundle.rs:339` | `get_platform_string()` labels the bundle's platform from host `env::consts`. |
| `ops/verify/harness.rs:137-152` | Harness link flags (`-lpthread -ldl -lm`, `-framework CoreFoundation`) are chosen by host `#[cfg]`, and the harness is compiled with the host toolchain even when the library under test was cross-built. |
| `builder/shim/intent.rs:247-266` | `is_host()` — host `cfg!` plus substring matching, described above. |
| `util/vcpkg.rs:286-295` | `resolve_triplet`/`infer_triplet` map to a vcpkg triplet correctly, but every caller passes `TargetTriple::host()`, so `--target-triple` never reaches vcpkg. |

Legitimately host-scoped and left alone: vcpkg/`cmake`/`meson` binary lookup
and install hints, `doctor`'s environment report, executable extension for the
harness binary Harbour itself runs, and the MSVC probe paths that only exist on
a Windows host.

---

## Current-state findings that shaped this design

Two things turned up in the code inventory that change the plan and are worth
recording, because both are cases where an existing type implies a working
feature that is not actually connected:

1. **The ABI fingerprint module is dead code** (see Risks). Wiring it up is
   real work with real value — incremental-rebuild correctness — but it is
   *separate* from unifying the target model, and A deliberately does not do
   it. A gets cross-build correctness from output-path separation instead.

2. **`PlatformSupport` and `min_platforms` have no consumers.**
   `Shim::platforms()` (`shim.rs:552`) and `min_platforms` (`shim.rs:197`) are
   read only by their own unit tests. Nothing in dependency resolution or
   registry handling enforces `[curated].min_platform_count` or
   `requires_ci_pass`. The curation gate described in the registry plan
   (Project C) has to be *written*, not merely configured — I had previously
   read the presence of those config types as evidence the gate existed.

---

## Testing

Table-driven, built from the researched corpus before the parser is written:

1. **Parse corpus** — one row per real triple: `raw → (arch, vendor, os, env, is_bare_metal)`.
   Covers glibc/musl, macOS/iOS, Windows msvc/mingw, Cortex-M (thumbv6m/7m/7em/8m,
   hf and soft-float), RISC-V bare metal and Linux, ESP32 (xtensa and riscv),
   AVR, MSP430, wasm32/wasi, Android, plus malformed inputs.
2. **Round-trip property** — `parse(s).as_str() == s` for every row.
3. **Canonicalization** — equivalent spellings compare equal under `canonical()`.
4. **ABI fingerprint** — distinct targets differ; equivalent spellings match.
5. **Toolchain** — an unknown target with no installed compiler yields an
   actionable error and *never* the host compiler.

---

## Risks

- **Blast radius.** The type unification touches the whole build path. Mitigated
  by landing it as one reviewable PR with the parse corpus as a safety net, and
  by the fact that the existing 754-test suite must stay green throughout.
- **No cache invalidation risk, because there is no live cache key.** I had
  assumed canonicalization would invalidate existing fingerprints. It cannot:
  `LinkFingerprint::for_link` (`fingerprint.rs:225`) and
  `ToolchainFingerprint::new`/`hash` (`fingerprint.rs:69,104`) have **zero call
  sites outside their own module**, and `AbiIdentity::fingerprint`
  (`abi.rs:169`) is only reached through them. The ABI/fingerprint machinery is
  designed but not wired into `plan.rs`/`native.rs`/`executor.rs`. Correctness
  for cross builds therefore comes from **path separation**
  (`.harbour/target/<triple>/<profile>/`), which is sufficient to stop host and
  target artifacts contaminating each other, and does not require the
  fingerprint module to be live.
- **One breaking internal signature**: `detect_toolchain`.

## Relationship to other work

| | Scope | Depends on |
|---|---|---|
| **A** (this doc) | unify the target model | — |
| **B** | target-conditional manifest, libc/float-ABI axes, `capabilities`, `PlatformSupport` redesign | A |
| **C** | registry generation pipeline (LLM-assisted shim authoring) | B |

C depends on B because generating shims against a schema that is about to change
for embedded means regenerating all of them. Natural `gh stack`: A → B → C.
