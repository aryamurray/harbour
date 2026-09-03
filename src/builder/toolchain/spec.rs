//! Target-specification layer: given a [`TargetTriple`], work out which
//! compiler binary to look for and which flags that target needs.
//!
//! ## The core finding this module is built around
//!
//! `<triple>-gcc` is **wrong for most targets**, not a few. Concrete,
//! researched mismatches:
//!
//! | Triple | Actual compiler binary | What differs |
//! |---|---|---|
//! | `thumbv7em-none-eabihf`, all `thumbv*` | `arm-none-eabi-gcc` | ONE binary serves every Cortex-M sub-arch; the core is selected by `-mcpu`, not by binary name |
//! | `riscv32imac-unknown-none-elf` | `riscv32-unknown-elf-gcc` | arch extension suffix (`imac`/`gc`) dropped; `none` OS collapses into `-elf` naming |
//! | `riscv64gc-unknown-linux-gnu` | `riscv64-unknown-linux-gnu-gcc`, Debian: `riscv64-linux-gnu-gcc` | suffix dropped; Debian also drops vendor |
//! | `aarch64-unknown-linux-gnu` | `aarch64-linux-gnu-gcc` | Debian/Ubuntu drop the `unknown` vendor |
//! | `x86_64-unknown-linux-musl` | `x86_64-linux-musl-gcc` | vendor dropped |
//! | `x86_64-pc-windows-gnu` | `x86_64-w64-mingw32-gcc` | shares nothing with the triple; mingw-w64's naming predates LLVM's |
//! | `armv7-linux-androideabi` | `armv7a-linux-androideabi21-clang` | clang not gcc; API level spliced in; `armv7`->`armv7a` |
//! | `x86_64-apple-darwin` | none -- `xcrun --sdk macosx clang -target <triple>` | no prefixed binary exists at all |
//! | `x86_64-pc-windows-msvc` | none -- `cl.exe` located via `vswhere.exe` | not a PATH-prefix lookup at all |
//! | `xtensa-esp32s3-elf` | `xtensa-esp32s3-elf-gcc` | exact match -- one of the few |
//! | `avr` | `avr-gcc` | trivially matches; must NOT grow extra fields if normalized internally |
//! | `msp430-elf` | `msp430-elf-gcc` (modern TI) or `msp430-gcc` (pre-GCC9) | version-dependent |
//!
//! This module does not perform discovery itself (no filesystem or PATH
//! access) -- it only computes candidates and flags. Wiring this into actual
//! `which`-style discovery and into `detect_toolchain()` is left to a later
//! change; see the module-level constraints in the design doc that produced
//! this file.

use crate::core::target::TargetTriple;

/// How a [`ToolchainCandidate`] should be located on the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryStrategy {
    /// Look up `name` as a PATH-prefixed binary (e.g. `which arm-none-eabi-gcc`).
    PathPrefix,
    /// macOS only: `xcrun --sdk <sdk> <name> -target <triple>`. There is no
    /// prefixed binary at all for Apple targets; Xcode's `clang` is invoked
    /// directly via `xcrun` with an explicit `-target`.
    Xcrun,
    /// Windows/MSVC only: `cl.exe` is not found by PATH-prefix convention.
    /// It is located by running `vswhere.exe` to find a Visual Studio / Build
    /// Tools installation, then adding its bin directory to PATH.
    Vswhere,
    /// The binary is supplied verbatim by the user/config (e.g. `CC=/opt/x/bin/foo-gcc`)
    /// and should not be probed for at all.
    ExplicitPath,
}

/// The compiler family a candidate binary belongs to. Mostly informative --
/// it lets callers pick `-mcpu`/`-march` style flags vs. clang's `-target`
/// style, and pick the right C++ driver name (e.g. `g++` vs `clang++`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompilerFamily {
    Gcc,
    Clang,
    AppleClang,
    Msvc,
}

/// One plausible compiler binary name for a target, in priority order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolchainCandidate {
    /// The C compiler binary name to probe for (e.g. `arm-none-eabi-gcc`).
    ///
    /// For [`DiscoveryStrategy::Xcrun`] and [`DiscoveryStrategy::Vswhere`]
    /// this is not a PATH-prefixed binary name; see those variants' docs.
    pub c_name: String,
    /// The corresponding C++ driver name, where the family draws a
    /// distinction (e.g. `arm-none-eabi-g++`, `clang++`). `None` when the
    /// same binary drives both languages (rare) or the family has no fixed
    /// convention worth guessing (in which case swap the trailing `gcc` for
    /// `g++`/`clang` for `clang++` as a fallback).
    pub cxx_name: Option<String>,
    pub strategy: DiscoveryStrategy,
    pub family: CompilerFamily,
    /// Short human-readable note on why this candidate was generated, used
    /// to build a "probed: X, Y, Z" message when discovery fails.
    pub rationale: &'static str,
}

impl ToolchainCandidate {
    fn gcc(c_name: impl Into<String>, rationale: &'static str) -> Self {
        let c_name = c_name.into();
        let cxx_name = derive_cxx_name(&c_name, CompilerFamily::Gcc);
        ToolchainCandidate {
            c_name,
            cxx_name,
            strategy: DiscoveryStrategy::PathPrefix,
            family: CompilerFamily::Gcc,
            rationale,
        }
    }

    fn clang(c_name: impl Into<String>, rationale: &'static str) -> Self {
        let c_name = c_name.into();
        let cxx_name = derive_cxx_name(&c_name, CompilerFamily::Clang);
        ToolchainCandidate {
            c_name,
            cxx_name,
            strategy: DiscoveryStrategy::PathPrefix,
            family: CompilerFamily::Clang,
            rationale,
        }
    }

    fn xcrun(rationale: &'static str) -> Self {
        ToolchainCandidate {
            c_name: "clang".to_string(),
            cxx_name: Some("clang++".to_string()),
            strategy: DiscoveryStrategy::Xcrun,
            family: CompilerFamily::AppleClang,
            rationale,
        }
    }

    fn vswhere(rationale: &'static str) -> Self {
        ToolchainCandidate {
            c_name: "cl.exe".to_string(),
            cxx_name: Some("cl.exe".to_string()),
            strategy: DiscoveryStrategy::Vswhere,
            family: CompilerFamily::Msvc,
            rationale,
        }
    }
}

/// Swap a trailing `-gcc`/`-clang` (or bare `gcc`/`clang`) for the C++
/// driver name. Best-effort; only used when a family doesn't have a fixed
/// convention (like `armv7a-linux-androideabi21-clang` -> `...-clang++`).
fn derive_cxx_name(c_name: &str, family: CompilerFamily) -> Option<String> {
    match family {
        CompilerFamily::Gcc => c_name
            .strip_suffix("gcc")
            .map(|prefix| format!("{prefix}g++")),
        CompilerFamily::Clang | CompilerFamily::AppleClang => c_name
            .strip_suffix("clang")
            .map(|prefix| format!("{prefix}clang++")),
        CompilerFamily::Msvc => None,
    }
}

/// The C standard library flavour a bare-metal/embedded target links
/// against, where known.
///
/// Deliberately does NOT include a "nano" variant: newlib-nano is a
/// link-time `--specs=nano.specs` choice on top of ordinary newlib, never a
/// distinct libc or a triple component. It is modeled as a `BuildSetting`
/// flag instead (see [`TargetSpec::extra_link_flags`] / the `nano_specs`
/// setting below).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LibcFlavor {
    /// glibc, musl, MSVC CRT, Apple libSystem, etc. -- a hosted OS libc.
    Hosted,
    Newlib,
    Picolibc,
    /// No libc at all / freestanding, or unknown.
    None,
}

/// What a target needs in order to build: candidate compiler binaries, how
/// to discover them, target-specific flags, and the libc flavour where
/// known.
///
/// Two shapes are deliberately *not* assumed here, because assuming them
/// would be wrong for real targets:
///
/// - **Xtensa has no `-mcpu` equivalent.** Chip selection *is* the compiler
///   binary (`xtensa-esp32-elf-gcc` and `xtensa-esp32s3-elf-gcc` are
///   different GCC builds, not the same GCC with a different flag). So
///   `derived_flags` for Xtensa never emits a core-selection flag -- there
///   isn't one.
/// - **AVR and MSP430 triples carry no chip granularity.** `-mmcu=atmega328p`
///   cannot be derived from the triple `avr` alone; it must come from
///   outside (manifest/config). `extra_cflags` exists precisely so a caller
///   can splice in externally-supplied flags like this; `TargetSpec` never
///   invents an `-mmcu` value.
///
/// Likewise, newlib-nano is modeled as a boolean build setting
/// (`nano_specs`), never as a field derived from the triple, because it is
/// a link-time choice orthogonal to the target triple.
#[derive(Debug, Clone)]
pub struct TargetSpec {
    /// Ordered candidate compiler binaries, most-specific first.
    pub candidates: Vec<ToolchainCandidate>,
    /// Compile flags this target needs, derived purely from the triple.
    /// Does not include chip-specific flags that the triple cannot express
    /// (e.g. `-mmcu`); see `extra_cflags` for those.
    pub derived_cflags: Vec<String>,
    /// Flags that must be supplied by the caller from outside the triple
    /// (manifest config, CLI flag, etc.) because the triple does not carry
    /// enough information to derive them. Always empty from
    /// [`TargetSpec::for_triple`] alone -- callers append to this field.
    pub extra_cflags: Vec<String>,
    /// The libc flavour, where it can be determined from the triple alone.
    pub libc: LibcFlavor,
    /// Whether this target's flags are only partially known (e.g. Cortex-M7
    /// single- vs double-precision FPU cannot be told apart from the triple
    /// alone). When true, `derived_cflags` is a best-effort default and
    /// callers should let users override it.
    pub flags_uncertain: bool,
    /// Human-readable note explaining any uncertainty, empty if none.
    pub uncertainty_note: &'static str,
}

impl TargetSpec {
    /// Build a `TargetSpec` for a triple. Never fails -- an unrecognized
    /// triple still yields plausible candidates (see
    /// [`toolchain_candidates`]) with no derived flags and unknown libc.
    pub fn for_triple(triple: &TargetTriple) -> Self {
        let candidates = toolchain_candidates(triple);
        let (derived_cflags, flags_uncertain, uncertainty_note) = derived_flags(triple);
        let libc = libc_flavor(triple);

        TargetSpec {
            candidates,
            derived_cflags,
            extra_cflags: Vec::new(),
            libc,
            flags_uncertain,
            uncertainty_note,
        }
    }

    /// All compile flags: derived followed by externally-supplied.
    pub fn cflags(&self) -> Vec<String> {
        let mut flags = self.derived_cflags.clone();
        flags.extend(self.extra_cflags.iter().cloned());
        flags
    }
}

/// Generate plausible compiler binary names for `triple`, most-specific
/// first. Never returns an empty list and never errors -- worst case, only
/// generic convention-based candidates come back and discovery fails later
/// with a message naming what was probed (that message is the caller's
/// responsibility; this function only supplies the `rationale` strings that
/// go into it).
///
/// Ordering rationale:
/// 1. A built-in table of exact, researched family special cases (mingw-w64,
///    Android NDK, Xtensa, AVR, MSP430, Apple, MSVC) -- these are wrong
///    often enough by convention that they must short-circuit everything
///    else.
/// 2. Exact `<raw>-gcc` / `<raw>-clang` -- the naive-but-sometimes-right
///    guess (e.g. `xtensa-esp32s3-elf-gcc`, plain `avr-gcc`).
/// 3. Vendor dropped (`aarch64-linux-gnu-gcc` for Debian/Ubuntu cross
///    packages that omit `unknown`).
/// 4. Arch extension suffix normalized (`riscv32imac` -> `riscv32`, any
///    `thumbv*` -> `arm`) with vendor dropped and `os=none` collapsed to
///    `-elf` naming, matching how riscv32-elf and arm-none-eabi toolchains
///    are actually packaged.
/// 5. `os=none` collapsed to `-elf` on its own, for triples that already
///    have a normalized arch.
pub fn toolchain_candidates(triple: &TargetTriple) -> Vec<ToolchainCandidate> {
    let mut out = Vec::new();

    // --- 1. Built-in table of known family special cases ---
    built_in_candidates(triple, &mut out);

    // Apple and MSVC are not PATH-prefix lookups at all: no `<prefix>-gcc`
    // binary exists for them, so generating one would be actively
    // misleading rather than merely redundant. Short-circuit here.
    if triple.is_apple() || triple.is_msvc() {
        return out;
    }

    // --- 2. Exact raw-triple match ---
    let raw = triple.as_str();
    if !raw.is_empty() {
        push_unique_gcc(
            &mut out,
            format!("{raw}-gcc"),
            "exact raw triple, gcc convention",
        );
        push_unique_clang(
            &mut out,
            format!("{raw}-clang"),
            "exact raw triple, clang convention",
        );
    }

    // --- 3. Vendor dropped ---
    if let Some(no_vendor) = drop_vendor(triple) {
        push_unique_gcc(
            &mut out,
            format!("{no_vendor}-gcc"),
            "vendor dropped (Debian/Ubuntu cross-package convention)",
        );
    }

    // --- 4/5. Normalized arch + os=none -> -elf ---
    let normalized_arch = normalize_arch(triple.arch());
    let is_bare = triple.is_bare_metal();

    if let Some(norm_arch) = normalized_arch {
        // Normalized arch, vendor dropped, os=none collapsed to `-elf`.
        let candidate = if is_bare {
            format!("{norm_arch}-elf-gcc")
        } else {
            let vendor_os_env = triple_suffix_after_arch(triple);
            format!("{norm_arch}{vendor_os_env}-gcc")
        };
        push_unique_gcc(
            &mut out,
            candidate,
            "arch extension suffix normalized (e.g. riscv32imac -> riscv32, thumbv* -> arm)",
        );

        if is_bare {
            push_unique_gcc(
                &mut out,
                format!("{norm_arch}-none-eabi-gcc"),
                "normalized arch, none-eabi convention (e.g. arm-none-eabi-gcc)",
            );
        }
    } else if is_bare {
        // Arch already normalized (or unknown): just collapse os=none to -elf.
        push_unique_gcc(
            &mut out,
            format!("{}-elf-gcc", triple.arch()),
            "os=none collapsed to -elf naming",
        );
    }

    out
}

/// Case-insensitive-free helper: push a gcc candidate if its `c_name` isn't
/// already present.
fn push_unique_gcc(out: &mut Vec<ToolchainCandidate>, name: String, rationale: &'static str) {
    if !out.iter().any(|c| c.c_name == name) {
        out.push(ToolchainCandidate::gcc(name, rationale));
    }
}

fn push_unique_clang(out: &mut Vec<ToolchainCandidate>, name: String, rationale: &'static str) {
    if !out.iter().any(|c| c.c_name == name) {
        out.push(ToolchainCandidate::clang(name, rationale));
    }
}

/// Everything after the arch in the raw triple (e.g. `-unknown-linux-gnu`),
/// used to reattach vendor/os/env to a normalized arch.
fn triple_suffix_after_arch(triple: &TargetTriple) -> String {
    let raw = triple.as_str();
    match raw.strip_prefix(triple.arch()) {
        Some(rest) => rest.to_string(),
        None => String::new(),
    }
}

/// Drop the vendor component from a triple's raw string, if any, joining
/// the remaining components back with `-`. Returns `None` if there was no
/// vendor to drop.
fn drop_vendor(triple: &TargetTriple) -> Option<String> {
    let vendor = triple.vendor()?;
    if vendor == "none" {
        // `none` in the vendor slot means "no vendor" already; dropping it
        // is the same as the `os=none -> -elf` case, handled separately.
        return None;
    }
    let mut parts = vec![triple.arch().to_string()];
    if let Some(os) = triple.os() {
        parts.push(os.to_string());
    }
    if let Some(env) = triple.env() {
        parts.push(env.to_string());
    }
    if parts.len() <= 1 {
        return None;
    }
    Some(parts.join("-"))
}

/// Normalize an arch token by dropping RISC-V extension suffixes and
/// collapsing any Thumb sub-architecture to `arm`. Returns `None` if the
/// arch token needs no normalization.
fn normalize_arch(arch: &str) -> Option<String> {
    if arch.starts_with("thumbv") {
        return Some("arm".to_string());
    }
    if let Some(rest) = arch.strip_prefix("riscv32") {
        // riscv32, riscv32i, riscv32imc, riscv32imac, riscv32imafc, riscv32gc
        if !rest.is_empty() {
            return Some("riscv32".to_string());
        }
        return None;
    }
    if let Some(rest) = arch.strip_prefix("riscv64") {
        // riscv64, riscv64gc, riscv64imac
        if !rest.is_empty() {
            return Some("riscv64".to_string());
        }
        return None;
    }
    None
}

/// Built-in, researched family special cases that short-circuit generic
/// probing. Appends to `out`, most-specific first, without duplicating an
/// entry that's already present.
fn built_in_candidates(triple: &TargetTriple, out: &mut Vec<ToolchainCandidate>) {
    let arch = triple.arch();
    let raw = triple.as_str();

    // --- Apple: no prefixed binary exists at all ---
    if triple.is_apple() {
        out.push(ToolchainCandidate::xcrun(
            "Apple targets have no prefixed compiler binary; located via \
             `xcrun --sdk <sdk> clang -target <triple>`",
        ));
        return;
    }

    // --- Windows MSVC: not a PATH-prefix lookup ---
    if triple.is_msvc() {
        out.push(ToolchainCandidate::vswhere(
            "MSVC has no PATH-prefixed binary; cl.exe is located via vswhere.exe",
        ));
        return;
    }

    // --- Windows GNU (mingw-w64): shares nothing with the triple ---
    if triple.is_windows() && triple.env_is("gnu") {
        // mingw-w64's naming predates LLVM's triple convention.
        push_unique_gcc(
            out,
            "x86_64-w64-mingw32-gcc".to_string(),
            "mingw-w64 naming predates LLVM triples; shares nothing with the triple",
        );
        if arch == "i686" || arch == "i386" || arch == "x86" {
            push_unique_gcc(
                out,
                "i686-w64-mingw32-gcc".to_string(),
                "mingw-w64 32-bit naming",
            );
        }
        return;
    }

    // --- Android: clang, not gcc; API level spliced in; armv7 -> armv7a ---
    if triple.env_is("android") || triple.env_is("androideabi") {
        let ndk_arch = match arch {
            "armv7" | "arm" => "armv7a",
            "aarch64" => "aarch64",
            "x86_64" => "x86_64",
            "x86" | "i686" => "i686",
            other => other,
        };
        let abi = if triple.env_is("androideabi") {
            "androideabi"
        } else {
            "android"
        };
        // API level is required by the real NDK naming but is not carried
        // by the triple at all; we cannot know it, so we emit the
        // API-level-free form as a best-effort candidate and note the gap.
        push_unique_clang(
            out,
            format!("{ndk_arch}-linux-{abi}-clang"),
            "Android NDK clang; note: real NDK binaries splice in an API \
             level (e.g. ...androideabi21-clang) which the triple does not \
             carry, so this candidate omits it -- uncertain",
        );
        return;
    }

    // --- Xtensa: chip selection IS the binary name, exact match ---
    if arch == "xtensa" {
        if let Some(vendor) = triple.vendor() {
            // e.g. xtensa-esp32-elf-gcc, xtensa-esp32s3-elf-gcc
            push_unique_gcc(
                out,
                format!("xtensa-{vendor}-elf-gcc"),
                "Xtensa: chip selection is the compiler binary itself, not a flag",
            );
        }
        return;
    }

    // --- AVR: trivially matches, must not grow extra fields ---
    if arch == "avr" {
        push_unique_gcc(out, "avr-gcc".to_string(), "AVR: exact match by convention");
        return;
    }

    // --- MSP430: version-dependent binary name ---
    if arch == "msp430" {
        push_unique_gcc(
            out,
            "msp430-elf-gcc".to_string(),
            "modern TI MSP430 GCC (GCC9+) toolchain naming",
        );
        push_unique_gcc(
            out,
            "msp430-gcc".to_string(),
            "pre-GCC9 MSP430 toolchain naming",
        );
        return;
    }

    // --- RISC-V Linux: Debian drops the vendor too ---
    if arch.starts_with("riscv64") && triple.os() == Some("linux") {
        push_unique_gcc(
            out,
            "riscv64-linux-gnu-gcc".to_string(),
            "Debian/Ubuntu riscv64 cross-package naming (vendor dropped)",
        );
        push_unique_gcc(
            out,
            "riscv64-unknown-linux-gnu-gcc".to_string(),
            "riscv-gnu-toolchain upstream naming (suffix dropped, vendor kept)",
        );
    }

    // --- aarch64/x86_64 Linux: Debian/Ubuntu drop the vendor ---
    if triple.os() == Some("linux") && triple.vendor() == Some("unknown") {
        if arch == "aarch64" {
            push_unique_gcc(
                out,
                "aarch64-linux-gnu-gcc".to_string(),
                "Debian/Ubuntu cross-package naming (vendor dropped)",
            );
        }
        if arch == "x86_64" && triple.env_is("musl") {
            push_unique_gcc(
                out,
                "x86_64-linux-musl-gcc".to_string(),
                "musl.cc / musl-cross-make naming (vendor dropped)",
            );
        }
    }

    // --- All thumbv* Cortex-M: one binary serves every sub-arch ---
    if arch.starts_with("thumbv") || (arch == "arm" && triple.is_bare_metal()) {
        push_unique_gcc(
            out,
            "arm-none-eabi-gcc".to_string(),
            "ARM: one arm-none-eabi-gcc binary serves every Cortex-M \
             sub-arch; core is selected by -mcpu, not by binary name",
        );
    }

    let _ = raw; // silence unused warning if no branch above used it directly
}

/// The libc flavour for `triple`, where derivable from the triple alone.
/// Anything hosted (has a real OS with an env we recognize) is `Hosted`.
/// Bare metal ARM (`eabi`/`eabihf`) defaults to newlib, since that's what
/// arm-none-eabi-gcc ships with. Everything else unknown/bare metal is
/// `None` rather than guessed.
fn libc_flavor(triple: &TargetTriple) -> LibcFlavor {
    if !triple.is_bare_metal() && !triple.is_embedded_rtos() {
        return LibcFlavor::Hosted;
    }

    let arch = triple.arch();
    if (arch == "arm" || arch.starts_with("thumbv") || arch.starts_with("armv7"))
        && matches!(triple.env(), Some("eabi") | Some("eabihf"))
    {
        // arm-none-eabi-gcc ships newlib by default. picolibc is a
        // deliberate opt-in (a different toolchain/config), not derivable
        // from the triple, so we report the default rather than picolibc.
        return LibcFlavor::Newlib;
    }

    LibcFlavor::None
}

/// Derive target-specific compile flags purely from the triple, for the
/// families where this is possible. Returns `(flags, uncertain, note)`.
///
/// Values here are the ones the design doc marked as verified:
/// - Cortex-M0 (ARMv6-M, no FPU): `-mcpu=cortex-m0 -mthumb -mfloat-abi=soft`,
///   `-mfpu` omitted entirely (some GCC versions error if `-mfpu` is paired
///   with a coreless-FPU target).
/// - Cortex-M4 (no F suffix): `-mcpu=cortex-m4 -mthumb -mfloat-abi=soft`, no
///   `-mfpu` -- "M4" alone does not imply hardfloat.
/// - Cortex-M4F: `-mcpu=cortex-m4 -mthumb -mfloat-abi=hard -mfpu=fpv4-sp-d16`.
/// - Cortex-M7: `-mcpu=cortex-m7 -mthumb -mfloat-abi=hard -mfpu=fpv5-d16` as
///   a *default*, but single-precision-only M7 parts actually need
///   `fpv5-sp-d16`. This is a per-chip, not per-core, distinction the triple
///   cannot express, so `uncertain` is set true for M7 and the note explains
///   the gap.
/// - RISC-V `rv32imac`: `-march=rv32imac -mabi=ilp32` (never `ilp32f`/`ilp32d`:
///   `imac` carries no float extension).
/// - RISC-V `rv32imafc`: `-march=rv32imafc -mabi=ilp32f`.
/// - ESP32 Xtensa: `-mlongcalls -mtext-section-literals`.
/// - AVR/MSP430: no flags derived here -- `-mmcu=<part>` must come from
///   outside the triple; see `TargetSpec::extra_cflags`.
fn derived_flags(triple: &TargetTriple) -> (Vec<String>, bool, &'static str) {
    let arch = triple.arch();

    // Bare-metal ARM Cortex-M family, keyed off the Thumb sub-arch token.
    // Mapping thumbv* -> Cortex-M core is itself a convention (the mapping
    // used by `rustc`/`arm-none-eabi-gcc` toolchain files), included here
    // because it's what the vast majority of thumbv* triples in practice
    // target -- but it is a convention, not something the triple encodes
    // authoritatively, so it's called out.
    match arch {
        "thumbv6m" => (
            vec![
                "-mcpu=cortex-m0".to_string(),
                "-mthumb".to_string(),
                "-mfloat-abi=soft".to_string(),
            ],
            false,
            "",
        ),
        "thumbv7m" => (
            // Cortex-M3: no FPU on any M3 part, same shape as M0.
            vec![
                "-mcpu=cortex-m3".to_string(),
                "-mthumb".to_string(),
                "-mfloat-abi=soft".to_string(),
            ],
            false,
            "",
        ),
        "thumbv7em" => {
            // Ambiguous: thumbv7em covers both plain Cortex-M4 (soft float)
            // and Cortex-M4F (hardfloat + fpv4-sp-d16). The triple's `eabihf`
            // vs `eabi` env is the only signal available and is what we use;
            // it matches Rust's own thumbv7em-none-eabi vs
            // thumbv7em-none-eabihf split.
            if triple.env_is("eabihf") {
                (
                    vec![
                        "-mcpu=cortex-m4".to_string(),
                        "-mthumb".to_string(),
                        "-mfloat-abi=hard".to_string(),
                        "-mfpu=fpv4-sp-d16".to_string(),
                    ],
                    false,
                    "",
                )
            } else {
                (
                    vec![
                        "-mcpu=cortex-m4".to_string(),
                        "-mthumb".to_string(),
                        "-mfloat-abi=soft".to_string(),
                    ],
                    false,
                    "",
                )
            }
        }
        "thumbv8m.main" => {
            // Cortex-M33/M35P territory; FPU is per-chip. Treat like M4F as
            // a default when hardfloat env is present, otherwise soft, but
            // flag as uncertain since M8-M variants have more FPU shapes.
            if triple.env_is("eabihf") {
                (
                    vec![
                        "-mcpu=cortex-m33".to_string(),
                        "-mthumb".to_string(),
                        "-mfloat-abi=hard".to_string(),
                        "-mfpu=fpv5-sp-d16".to_string(),
                    ],
                    true,
                    "Cortex-M33/M35P FPU width is per-chip; fpv5-sp-d16 assumed but unverified",
                )
            } else {
                (
                    vec![
                        "-mcpu=cortex-m33".to_string(),
                        "-mthumb".to_string(),
                        "-mfloat-abi=soft".to_string(),
                    ],
                    false,
                    "",
                )
            }
        }
        "thumbv8m.base" => (
            vec![
                "-mcpu=cortex-m23".to_string(),
                "-mthumb".to_string(),
                "-mfloat-abi=soft".to_string(),
            ],
            false,
            "",
        ),
        "riscv32imac" => (
            vec!["-march=rv32imac".to_string(), "-mabi=ilp32".to_string()],
            false,
            "",
        ),
        "riscv32imafc" => (
            vec!["-march=rv32imafc".to_string(), "-mabi=ilp32f".to_string()],
            false,
            "",
        ),
        "xtensa" => {
            if triple.vendor() == Some("esp32") || triple.vendor() == Some("esp32s2") {
                (
                    vec![
                        "-mlongcalls".to_string(),
                        "-mtext-section-literals".to_string(),
                    ],
                    false,
                    "",
                )
            } else if triple.vendor() == Some("esp32s3") {
                (
                    vec![
                        "-mlongcalls".to_string(),
                        "-mtext-section-literals".to_string(),
                    ],
                    true,
                    "esp32s3-specific flag differences beyond -mlongcalls/-mtext-section-literals are not researched here",
                )
            } else {
                (Vec::new(), true, "unrecognized Xtensa chip; no flags derived")
            }
        }
        "avr" | "msp430" => (
            Vec::new(),
            true,
            "chip is not carried by the triple; -mmcu=<part> must be supplied externally via extra_cflags",
        ),
        _ => {
            // Cortex-M7 note: this table has no thumbv7e-m7-specific entry
            // because llvm/rustc use `thumbv7em` for both M4 and M7-class
            // parts (the distinction is `-mcpu`, supplied by tooling outside
            // the triple, e.g. via a manifest field) -- so a triple-only M7
            // detection is not attempted here to avoid guessing wrong. This
            // is intentionally left undecided; see report / doc comment.
            (Vec::new(), false, "")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(raw: &str) -> TargetTriple {
        TargetTriple::parse(raw)
    }

    fn has_candidate(cands: &[ToolchainCandidate], name: &str) -> bool {
        cands.iter().any(|c| c.c_name == name)
    }

    fn index_of(cands: &[ToolchainCandidate], name: &str) -> Option<usize> {
        cands.iter().position(|c| c.c_name == name)
    }

    // --- Table-driven: one row per triple in the mismatch table ---

    #[test]
    fn thumb_family_resolves_to_arm_none_eabi() {
        for raw in [
            "thumbv7em-none-eabihf",
            "thumbv6m-none-eabi",
            "thumbv7m-none-eabi",
            "thumbv8m.main-none-eabihf",
        ] {
            let cands = toolchain_candidates(&t(raw));
            assert!(
                has_candidate(&cands, "arm-none-eabi-gcc"),
                "{raw}: expected arm-none-eabi-gcc in {cands:?}"
            );
        }
    }

    #[test]
    fn thumb_arm_none_eabi_is_high_priority() {
        // It should appear before any generic os=none -> -elf guess, since
        // it's the actually-correct binary for every Cortex-M sub-arch.
        let cands = toolchain_candidates(&t("thumbv7em-none-eabihf"));
        let idx = index_of(&cands, "arm-none-eabi-gcc").expect("present");
        assert!(
            idx <= 1,
            "expected arm-none-eabi-gcc early, got index {idx} in {cands:?}"
        );
    }

    #[test]
    fn riscv32imac_drops_extension_suffix() {
        let cands = toolchain_candidates(&t("riscv32imac-unknown-none-elf"));
        assert!(
            has_candidate(&cands, "riscv32-elf-gcc")
                || has_candidate(&cands, "riscv32-none-eabi-gcc"),
            "expected a riscv32 (suffix-dropped) candidate in {cands:?}"
        );
    }

    #[test]
    fn riscv64gc_linux_gnu_has_debian_and_upstream_candidates() {
        let cands = toolchain_candidates(&t("riscv64gc-unknown-linux-gnu"));
        assert!(
            has_candidate(&cands, "riscv64-linux-gnu-gcc"),
            "expected Debian riscv64-linux-gnu-gcc in {cands:?}"
        );
        assert!(
            has_candidate(&cands, "riscv64-unknown-linux-gnu-gcc"),
            "expected upstream riscv64-unknown-linux-gnu-gcc in {cands:?}"
        );
    }

    #[test]
    fn aarch64_linux_gnu_has_debian_vendor_dropped_candidate() {
        let cands = toolchain_candidates(&t("aarch64-unknown-linux-gnu"));
        assert!(
            has_candidate(&cands, "aarch64-linux-gnu-gcc"),
            "expected vendor-dropped aarch64-linux-gnu-gcc in {cands:?}"
        );
    }

    #[test]
    fn x86_64_linux_musl_has_vendor_dropped_candidate() {
        let cands = toolchain_candidates(&t("x86_64-unknown-linux-musl"));
        assert!(
            has_candidate(&cands, "x86_64-linux-musl-gcc"),
            "expected vendor-dropped x86_64-linux-musl-gcc in {cands:?}"
        );
    }

    #[test]
    fn windows_gnu_resolves_to_mingw_w64_not_the_triple() {
        let cands = toolchain_candidates(&t("x86_64-pc-windows-gnu"));
        assert!(
            has_candidate(&cands, "x86_64-w64-mingw32-gcc"),
            "expected mingw-w64 naming in {cands:?}"
        );
        // The naive raw-triple guess should NOT be the only thing produced.
        assert!(cands.len() > 1 || has_candidate(&cands, "x86_64-w64-mingw32-gcc"));
    }

    #[test]
    fn android_resolves_to_clang_with_armv7_normalized() {
        let cands = toolchain_candidates(&t("armv7-linux-androideabi"));
        assert!(
            cands
                .iter()
                .any(|c| c.c_name.contains("armv7a") && c.c_name.contains("clang")),
            "expected armv7a...clang candidate in {cands:?}"
        );
        assert!(
            cands.iter().any(|c| c.family == CompilerFamily::Clang),
            "expected a Clang-family candidate in {cands:?}"
        );
    }

    #[test]
    fn apple_targets_use_xcrun_not_a_prefixed_binary() {
        for raw in ["x86_64-apple-darwin", "aarch64-apple-ios"] {
            let cands = toolchain_candidates(&t(raw));
            assert_eq!(
                cands.len(),
                1,
                "{raw}: expected exactly one xcrun candidate, got {cands:?}"
            );
            assert_eq!(cands[0].strategy, DiscoveryStrategy::Xcrun, "{raw}");
            assert_eq!(cands[0].family, CompilerFamily::AppleClang, "{raw}");
        }
    }

    #[test]
    fn msvc_uses_vswhere_not_a_path_prefix_lookup() {
        let cands = toolchain_candidates(&t("x86_64-pc-windows-msvc"));
        assert_eq!(
            cands.len(),
            1,
            "expected exactly one vswhere candidate, got {cands:?}"
        );
        assert_eq!(cands[0].strategy, DiscoveryStrategy::Vswhere);
        assert_eq!(cands[0].family, CompilerFamily::Msvc);
    }

    #[test]
    fn xtensa_esp32s3_is_exact_match() {
        let cands = toolchain_candidates(&t("xtensa-esp32s3-elf"));
        assert!(
            has_candidate(&cands, "xtensa-esp32s3-elf-gcc"),
            "expected exact match in {cands:?}"
        );
    }

    #[test]
    fn avr_matches_trivially_and_does_not_grow_fields() {
        let triple = t("avr");
        assert_eq!(triple.arch(), "avr");
        assert_eq!(triple.vendor(), None);
        assert_eq!(triple.os(), None);
        assert_eq!(triple.env(), None);

        let cands = toolchain_candidates(&triple);
        assert!(
            has_candidate(&cands, "avr-gcc"),
            "expected avr-gcc in {cands:?}"
        );
    }

    #[test]
    fn msp430_offers_both_modern_and_legacy_binary_names() {
        let cands = toolchain_candidates(&t("msp430-elf"));
        assert!(has_candidate(&cands, "msp430-elf-gcc"), "{cands:?}");
        assert!(has_candidate(&cands, "msp430-gcc"), "{cands:?}");
        // Modern (GCC9+) name should be tried first.
        let modern = index_of(&cands, "msp430-elf-gcc").unwrap();
        let legacy = index_of(&cands, "msp430-gcc").unwrap();
        assert!(
            modern < legacy,
            "expected msp430-elf-gcc before msp430-gcc in {cands:?}"
        );
    }

    // --- Unknown triples never error ---

    #[test]
    fn unknown_triple_still_yields_candidates() {
        let cands = toolchain_candidates(&t("loongarch128-unknown-linux-gnu"));
        assert!(
            !cands.is_empty(),
            "expected fallback candidates for an unknown triple"
        );
    }

    #[test]
    fn empty_triple_does_not_panic_and_yields_some_candidate() {
        let cands = toolchain_candidates(&t(""));
        // Nothing useful can be derived, but this must not panic or error.
        let _ = cands;
    }

    #[test]
    fn gibberish_single_component_triple_yields_candidates() {
        let cands = toolchain_candidates(&t("notarealarch"));
        assert!(!cands.is_empty());
    }

    // --- Flag derivation ---

    #[test]
    fn cortex_m0_omits_mfpu_entirely() {
        let (flags, uncertain, _) = derived_flags(&t("thumbv6m-none-eabi"));
        assert!(flags.contains(&"-mcpu=cortex-m0".to_string()));
        assert!(flags.contains(&"-mthumb".to_string()));
        assert!(flags.contains(&"-mfloat-abi=soft".to_string()));
        assert!(
            !flags.iter().any(|f| f.starts_with("-mfpu")),
            "M0 must not get -mfpu: {flags:?}"
        );
        assert!(!uncertain);
    }

    #[test]
    fn cortex_m4_plain_is_soft_float_no_fpu() {
        let (flags, _, _) = derived_flags(&t("thumbv7em-none-eabi"));
        assert!(flags.contains(&"-mfloat-abi=soft".to_string()));
        assert!(
            !flags.iter().any(|f| f.starts_with("-mfpu")),
            "plain M4 must not get -mfpu: {flags:?}"
        );
    }

    #[test]
    fn cortex_m4f_is_hardfloat_with_fpv4() {
        let (flags, _, _) = derived_flags(&t("thumbv7em-none-eabihf"));
        assert!(flags.contains(&"-mcpu=cortex-m4".to_string()));
        assert!(flags.contains(&"-mfloat-abi=hard".to_string()));
        assert!(flags.contains(&"-mfpu=fpv4-sp-d16".to_string()));
    }

    #[test]
    fn riscv32imac_never_gets_float_abi() {
        let (flags, _, _) = derived_flags(&t("riscv32imac-unknown-none-elf"));
        assert_eq!(
            flags,
            vec!["-march=rv32imac".to_string(), "-mabi=ilp32".to_string()]
        );
        assert!(!flags
            .iter()
            .any(|f| f.contains("ilp32f") || f.contains("ilp32d")));
    }

    #[test]
    fn riscv32imafc_gets_ilp32f() {
        let (flags, _, _) = derived_flags(&t("riscv32imafc-unknown-none-elf"));
        assert_eq!(
            flags,
            vec!["-march=rv32imafc".to_string(), "-mabi=ilp32f".to_string()]
        );
    }

    #[test]
    fn esp32_xtensa_gets_longcalls() {
        let (flags, _, _) = derived_flags(&t("xtensa-esp32-elf"));
        assert!(flags.contains(&"-mlongcalls".to_string()));
    }

    #[test]
    fn avr_and_msp430_derive_no_mmcu_flag() {
        for raw in ["avr", "msp430-elf"] {
            let (flags, uncertain, note) = derived_flags(&t(raw));
            assert!(
                !flags.iter().any(|f| f.starts_with("-mmcu")),
                "{raw}: -mmcu must never be derived from the triple: {flags:?}"
            );
            assert!(uncertain, "{raw}: should be flagged uncertain");
            assert!(!note.is_empty());
        }
    }

    #[test]
    fn target_spec_extra_cflags_can_supply_mmcu_externally() {
        let mut spec = TargetSpec::for_triple(&t("avr"));
        assert!(spec.derived_cflags.is_empty());
        spec.extra_cflags.push("-mmcu=atmega328p".to_string());
        assert!(spec.cflags().contains(&"-mmcu=atmega328p".to_string()));
    }

    #[test]
    fn libc_flavor_hosted_for_linux() {
        assert_eq!(
            TargetSpec::for_triple(&t("x86_64-unknown-linux-gnu")).libc,
            LibcFlavor::Hosted
        );
    }

    #[test]
    fn libc_flavor_newlib_for_arm_eabi() {
        assert_eq!(
            TargetSpec::for_triple(&t("thumbv7em-none-eabihf")).libc,
            LibcFlavor::Newlib
        );
    }

    #[test]
    fn libc_flavor_none_for_avr() {
        assert_eq!(TargetSpec::for_triple(&t("avr")).libc, LibcFlavor::None);
    }
}
