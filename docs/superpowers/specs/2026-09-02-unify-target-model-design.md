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

### 4. `--target` is not plumbed end to end

The clap arg exists (`cli.rs:152`, `cli.rs:238`) and `build.rs:73` converts it,
but it does not reach toolchain selection.

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

**Parsing recognizes, it does not count.** Following `llvm::Triple`: split on
`-`, take component 0 as the arch unconditionally, then classify each remaining
component against the vendor / os / env tables. Ambiguous values (`none`, `elf`
— both appear in multiple categories) are resolved by position and by which
slots are still unfilled. Unrecognized components are retained and
`fully_recognized` is set false.

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

Layer 4 is what satisfies "assume any C/C++ target": an unknown triple still
builds if its toolchain follows the universal prefix convention. Layers 1–2 mean
it builds even when it does not. Layer 3 exists specifically for the families
where **prefix ≠ triple** (`avr-gcc`, `xtensa-esp32-elf-gcc`), which is where the
naive convention breaks.

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
- **Canonicalization changes cache keys.** Existing `.harbour/target` caches will
  miss once after upgrade. Acceptable — it is a rebuild, not a corruption.
- **One breaking internal signature**: `detect_toolchain`.

## Relationship to other work

| | Scope | Depends on |
|---|---|---|
| **A** (this doc) | unify the target model | — |
| **B** | target-conditional manifest, libc/float-ABI axes, `capabilities`, `PlatformSupport` redesign | A |
| **C** | registry generation pipeline (LLM-assisted shim authoring) | B |

C depends on B because generating shims against a schema that is about to change
for embedded means regenerating all of them. Natural `gh stack`: A → B → C.
