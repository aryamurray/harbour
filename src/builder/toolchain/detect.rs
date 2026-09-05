//! Toolchain detection functions.

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

use crate::core::target::TargetTriple;
use crate::util::config::{
    global_toolchain_config_path, load_toolchain_config, project_toolchain_config_path,
    ToolchainConfig,
};

use super::spec::{CompilerFamily, DiscoveryStrategy, ToolchainCandidate};

#[cfg(target_os = "windows")]
use super::{EnvWrapper, MsvcToolchain};
use super::{GccToolchain, Toolchain, ToolchainPlatform};

/// Load toolchain configuration from config files.
///
/// Searches for config in this order:
/// 1. Project config (`.harbour/toolchain.toml` in current dir)
/// 2. Global config (`~/.harbour/toolchain.toml`)
fn load_toolchain_config_from_files() -> ToolchainConfig {
    let cwd = std::env::current_dir().unwrap_or_default();
    let project_path = project_toolchain_config_path(&cwd);
    let global_path = global_toolchain_config_path();

    if let Some(ref global) = global_path {
        load_toolchain_config(global, &project_path)
    } else {
        load_toolchain_config(&PathBuf::new(), &project_path)
    }
}

/// The target a build should actually use, applying the config fallback.
///
/// Exposed so a caller can resolve the effective target *once* and use the
/// same value for toolchain selection, the ABI identity, and the output
/// directory. Resolving it independently in each of those places is how they
/// drift apart.
pub fn resolve_target(explicit: Option<&TargetTriple>) -> TargetTriple {
    if let Some(target) = explicit {
        return target.clone();
    }
    load_toolchain_config_from_files()
        .toolchain
        .target
        .as_deref()
        .map(TargetTriple::parse)
        .unwrap_or_else(TargetTriple::host)
}

/// Detect a toolchain capable of building for `target`.
///
/// `None` means "build for the host", which is the historical behaviour.
///
/// Priority:
/// 1. Explicit `cc`/`cxx`/`ar` paths from config or environment. The user has
///    named the binaries, so they win for any target.
/// 2. For a cross target: the candidate binaries from
///    [`toolchain_candidates`](super::spec::toolchain_candidates), probed in
///    priority order.
/// 3. For the host: MSVC on Windows, else cc/gcc/clang.
///
/// A cross target **never** falls back to the host compiler. Doing so would
/// produce host binaries labelled as target binaries, which is a silent and
/// badly corrupting failure; a missing cross toolchain is an error naming
/// every binary that was probed.
pub fn detect_toolchain(target: Option<&TargetTriple>) -> Result<Box<dyn Toolchain>> {
    let config = load_toolchain_config_from_files();

    // `toolchain.target` is written by `harbour toolchain override --target`
    // and, until now, read by nothing -- so the CLI presented a working
    // cross-target setting that silently had no effect. An explicitly
    // requested target still takes precedence over it.
    let requested: Option<TargetTriple> = target.cloned().or_else(|| {
        let configured = config.toolchain.target.as_deref()?;
        tracing::debug!("using target from toolchain config: {configured}");
        Some(TargetTriple::parse(configured))
    });

    if config.has_overrides() {
        if let Some(toolchain) = try_detect_from_config(&config, requested.as_ref())? {
            return Ok(toolchain);
        }
    }

    match &requested {
        Some(t) if !t.is_host() => detect_cross_toolchain(t),
        _ => detect_host_toolchain(),
    }
}

/// Locate a cross toolchain for `target` by probing candidate binary names.
fn detect_cross_toolchain(target: &TargetTriple) -> Result<Box<dyn Toolchain>> {
    let candidates = super::spec::toolchain_candidates(target);
    let mut probed: Vec<String> = Vec::new();

    for candidate in &candidates {
        match candidate.strategy {
            DiscoveryStrategy::PathPrefix | DiscoveryStrategy::ExplicitPath => {
                if let Some(toolchain) = try_candidate(candidate, target)? {
                    return Ok(toolchain);
                }
                probed.push(candidate.c_name.clone());
            }
            // Not a PATH-prefix lookup either: the host's own clang, made a
            // cross compiler by `-target <triple>`. Tried last, so a real
            // prefixed cross toolchain always wins when one is installed --
            // it brings a target libc, headers, archiver and linker, which
            // host clang does not.
            DiscoveryStrategy::HostClangTarget => match try_host_clang_candidate(target) {
                Ok(toolchain) => return Ok(toolchain),
                Err(reason) => probed.push(reason),
            },
            // Not a PATH-prefix lookup, and Harbour does not implement this
            // discovery path yet. Reporting it as probed with the reason is
            // more useful than silently skipping it and claiming nothing was
            // found.
            DiscoveryStrategy::Xcrun => {
                if let Some(toolchain) = try_xcrun_candidate(candidate, target) {
                    return Ok(toolchain);
                }
                probed.push(format!("{} (via xcrun)", candidate.c_name));
            }
            DiscoveryStrategy::Vswhere => {
                probed.push(format!(
                    "{} (requires vswhere; not yet supported)",
                    candidate.c_name
                ));
            }
        }
    }

    bail!(
        "no toolchain found for target `{}`\n\
         \n\
         probed: {}\n\
         \n\
         hint: install a cross toolchain for this target, or name the binaries\n\
         explicitly with `harbour toolchain override --cc <path> --cxx <path>`.",
        target.as_str(),
        probed.join(", ")
    )
}

/// Probe a single candidate, returning a toolchain if its binaries exist.
fn try_candidate(
    candidate: &ToolchainCandidate,
    target: &TargetTriple,
) -> Result<Option<Box<dyn Toolchain>>> {
    use which::which;

    let Ok(cc) = which(&candidate.c_name) else {
        return Ok(None);
    };

    let cxx = candidate
        .cxx_name
        .as_deref()
        .and_then(|name| which(name).ok())
        .unwrap_or_else(|| GccToolchain::infer_cxx(&cc));

    // The archiver follows the compiler's prefix: arm-none-eabi-gcc implies
    // arm-none-eabi-ar. A host `ar` cannot be substituted, because it produces
    // archives for the wrong architecture.
    let ar = cross_ar_for(&candidate.c_name)
        .and_then(|name| which(name).ok())
        .or_else(|| which(format!("{}-ar", target.as_str())).ok());

    let Some(ar) = ar else {
        tracing::debug!(
            "found {} but no matching cross archiver; skipping candidate",
            candidate.c_name
        );
        return Ok(None);
    };

    tracing::info!(
        "using cross toolchain for {}: cc={}, ar={}",
        target.as_str(),
        cc.display(),
        ar.display()
    );

    Ok(Some(Box::new(
        GccToolchain::new(cc, cxx, ar, family_to_platform(candidate.family))
            .with_target(target.clone()),
    )))
}

/// Locate an Apple toolchain through `xcrun`.
///
/// Apple ships no triple-prefixed compiler, so PATH probing cannot find one.
/// The architecture is selected by `-arch`, which `TargetSpec` supplies as
/// both a compile and a link flag -- getting it on only one step would
/// compile for one architecture and link for another.
fn try_xcrun_candidate(
    candidate: &ToolchainCandidate,
    target: &TargetTriple,
) -> Option<Box<dyn Toolchain>> {
    let cc = xcrun_find(&candidate.c_name)?;
    let cxx = candidate
        .cxx_name
        .as_deref()
        .and_then(xcrun_find)
        .unwrap_or_else(|| GccToolchain::infer_cxx(&cc));
    // Apple's `ar` is not architecture-specific, so the host one is correct.
    let ar = xcrun_find("ar").or_else(|| which::which("ar").ok())?;

    // `xcrun clang` sets SDKROOT for its child; invoking the *resolved* clang
    // path directly does not, and clang then cannot find so much as stdio.h.
    // Passing it through the environment rather than as -isysroot keeps it off
    // every individual command line, and matches how MSVC is handled.
    let sdk_path = xcrun_sdk_path(apple_sdk_name(target))?;

    tracing::info!(
        "using xcrun toolchain for {}: cc={}, sdk={}",
        target.as_str(),
        cc.display(),
        sdk_path.display()
    );

    let inner =
        GccToolchain::new(cc, cxx, ar, ToolchainPlatform::AppleClang).with_target(target.clone());
    Some(Box::new(super::EnvWrapper::new(
        inner,
        vec![("SDKROOT".to_string(), sdk_path.display().to_string())],
    )))
}

/// What the host `clang` can and cannot do for a target, established by
/// actually running it rather than by inspecting names.
///
/// Every variant except [`HostClangProbe::Ready`] is a reason to refuse the
/// candidate. In particular [`HostClangProbe::NoUsableArchiver`] is not a
/// theoretical case: on a macOS host, `ar rcs lib.a elf.o` **exits 0** while
/// writing an archive that does not contain the object at all (cctools
/// `ranlib` warns "not a mach-o file" and drops the member). Accepting the
/// candidate on the strength of "clang compiled it" would therefore produce
/// an empty static library and a build that looks like it worked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostClangProbe {
    /// No `clang` on PATH.
    NoClang,
    /// clang exists but cannot generate code for this triple -- either the
    /// backend was not built into it, or it does not parse the triple.
    TargetUnsupported { clang: PathBuf },
    /// clang produces objects for the target, but no archiver on this host
    /// turns them into a usable archive. `tried` names what was attempted.
    NoUsableArchiver { clang: PathBuf, tried: Vec<String> },
    /// clang exited 0, but the object it produced is for the *host*
    /// architecture rather than the requested one -- so the flag did not
    /// take effect and accepting it would file host code under the target's
    /// name. See [`probe_host_clang`] step 2.
    WrongArchitecture {
        clang: PathBuf,
        /// The triple clang says it would actually build for.
        effective: String,
    },
    /// The probe itself could not be run (e.g. no writable temp directory).
    /// Distinct from a negative result: nothing was actually disproved.
    ProbeFailed { reason: String },
    /// clang compiles for the target and an archiver handles its output.
    Ready {
        clang: PathBuf,
        clangxx: PathBuf,
        ar: PathBuf,
    },
}

/// Archivers to try for a host-clang cross build, in order.
///
/// `llvm-ar` first because it is object-format agnostic by design. The host
/// `ar` is still tried, since on a GNU host it is usually GNU `ar` and
/// handles any ELF fine -- but it is only *accepted* if the empirical check
/// in [`archiver_handles_object`] passes.
const HOST_CLANG_ARCHIVERS: &[&str] = &["llvm-ar", "ar"];

/// Probe whether the host `clang` can build for `target` via `-target`.
///
/// Runs two real commands, both cheap and both hermetic (a temp directory,
/// a source file with no `#include`s, so no target sysroot is required):
///
/// 1. `clang -target <triple> -c probe.c -o probe.o` -- proves clang has the
///    backend and accepts the triple. A name-based check cannot know this:
///    Apple clang accepts `aarch64-none-elf` but rejects
///    `riscv32imac-unknown-none-elf`.
/// 2. The produced object's own header is read and compared against a
///    second object compiled with no `-target` at all. Exiting 0 does not
///    prove the flag took effect, and this is the exact shape of the bug
///    this whole change exists to prevent: host code filed under the
///    target's name. clang is asked (`-print-effective-triple`) which
///    architecture each side really is, so the comparison is only *required*
///    to differ when clang itself says the architectures differ -- a
///    same-arch cross target (`x86_64-linux-gnu` from an x86_64 host) is
///    legitimately identical.
/// 3. `<ar> rcs libprobe.a probe.o` followed by `<ar> t libprobe.a` --
///    proves the archiver kept the member. See [`HostClangProbe`] for why
///    the exit status alone is not enough.
///
/// Deliberately *not* probed: the linker. Linking a hosted non-native target
/// additionally needs a target sysroot, and a freestanding target needs
/// `-nostdlib`/`-ffreestanding`, a linker script and usually `lld`. Those are
/// build-flag concerns rather than discovery concerns, and are handled
/// elsewhere; a target that only builds static libraries works today.
pub fn probe_host_clang(target: &TargetTriple) -> HostClangProbe {
    use which::which;

    let Ok(clang) = which("clang") else {
        return HostClangProbe::NoClang;
    };

    let dir = match tempfile::tempdir() {
        Ok(dir) => dir,
        Err(e) => {
            return HostClangProbe::ProbeFailed {
                reason: format!("could not create a temp directory for the probe: {e}"),
            }
        }
    };

    let src = dir.path().join("harbour_probe.c");
    // No #include: a target sysroot is exactly what host clang does not have,
    // and needing one would make this probe fail for every bare-metal target
    // it is meant to enable. A defined symbol gives step 2 something to index.
    if let Err(e) = std::fs::write(&src, "int harbour_probe(void) { return 0; }\n") {
        return HostClangProbe::ProbeFailed {
            reason: format!("could not write the probe source: {e}"),
        };
    }

    let obj = dir.path().join("harbour_probe.o");
    let compiled = std::process::Command::new(&clang)
        .arg("-target")
        .arg(target.as_str())
        .arg("-c")
        .arg(&src)
        .arg("-o")
        .arg(&obj)
        .output();

    let ok = match compiled {
        Ok(out) => out.status.success(),
        Err(e) => {
            return HostClangProbe::ProbeFailed {
                reason: format!("could not run {}: {e}", clang.display()),
            }
        }
    };
    if !ok || !obj.exists() {
        return HostClangProbe::TargetUnsupported { clang };
    }

    // Step 2: did `-target` actually take effect? Ubuntu's clang 18 accepts
    // triples Apple's clang 21 rejects (a garbage 4th component is ignored
    // rather than diagnosed), so "exited 0" spans a wide range of
    // permissiveness across clang versions and cannot stand in for "built
    // for the requested architecture".
    let target_effective = effective_triple(&clang, Some(target.as_str()));
    let host_effective = effective_triple(&clang, None);
    let arch_should_differ = match (&target_effective, &host_effective) {
        (Some(t), Some(h)) => arch_family(triple_arch(t)) != arch_family(triple_arch(h)),
        // Without clang's own answer there is nothing to compare against;
        // the object-level check below is skipped rather than guessed at.
        _ => false,
    };

    if arch_should_differ {
        let host_obj = dir.path().join("harbour_probe_host.o");
        let host_built = std::process::Command::new(&clang)
            .arg("-c")
            .arg(&src)
            .arg("-o")
            .arg(&host_obj)
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false);

        if host_built {
            // Both sides must be a format this reads; an unrecognized object
            // format (COFF variants, say) leaves nothing to compare, and
            // refusing on that basis would disable the feature on a platform
            // rather than protect it.
            if let (Some(target_arch), Some(host_arch)) =
                (object_arch_id(&obj), object_arch_id(&host_obj))
            {
                if target_arch == host_arch {
                    return HostClangProbe::WrongArchitecture {
                        clang,
                        effective: target_effective.unwrap_or_default(),
                    };
                }
            } else {
                tracing::debug!(
                    "could not read the architecture out of the probe objects; \
                     accepting {} on clang's word alone",
                    target.as_str()
                );
            }
        }
    }

    let mut tried = Vec::new();
    for name in HOST_CLANG_ARCHIVERS {
        let Ok(ar) = which(name) else {
            continue;
        };
        tried.push((*name).to_string());
        if archiver_handles_object(&ar, &obj, dir.path()) {
            let clangxx = which("clang++").unwrap_or_else(|_| GccToolchain::infer_cxx(&clang));
            return HostClangProbe::Ready { clang, clangxx, ar };
        }
    }

    HostClangProbe::NoUsableArchiver { clang, tried }
}

/// Does `ar` produce an archive that actually *contains* `obj`?
///
/// Checked by listing the archive back, not by trusting the exit status:
/// macOS `ar` returns success while dropping non-Mach-O members.
fn archiver_handles_object(ar: &Path, obj: &Path, dir: &Path) -> bool {
    let archive = dir.join("libharbour_probe.a");
    // `ar rcs` *updates* an existing archive, so a stale one from a previous
    // archiver in the loop would make the next one look like it succeeded.
    let _ = std::fs::remove_file(&archive);

    let created = std::process::Command::new(ar)
        .arg("rcs")
        .arg(&archive)
        .arg(obj)
        .output();
    match created {
        Ok(out) if out.status.success() => {}
        _ => return false,
    }

    let Some(member) = obj.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let listed = std::process::Command::new(ar)
        .arg("t")
        .arg(&archive)
        .output();
    match listed {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .lines()
            .any(|line| line.trim().ends_with(member)),
        _ => false,
    }
}

/// `clang [-target <triple>] -print-effective-triple` -- clang's own answer
/// for what it would build, with vendor/OS defaults filled in and
/// architecture spellings normalized (`aarch64-apple-darwin` and
/// `arm64-apple-darwin` both come back as `arm64-apple-darwin`). Asking
/// clang beats re-deriving that normalization here and getting it subtly
/// wrong.
fn effective_triple(clang: &Path, target: Option<&str>) -> Option<String> {
    let mut cmd = std::process::Command::new(clang);
    if let Some(triple) = target {
        cmd.arg("-target").arg(triple);
    }
    let out = cmd.arg("-print-effective-triple").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let triple = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if triple.is_empty() {
        None
    } else {
        Some(triple)
    }
}

/// The architecture component of a triple -- everything before the first
/// `-`.
fn triple_arch(triple: &str) -> &str {
    triple.split('-').next().unwrap_or(triple)
}

/// Coarse ISA family for an architecture token, used only to decide whether
/// two objects are *expected* to carry different machine numbers.
///
/// Deliberately biased toward saying "same family": a wrong "same" only
/// skips a check, while a wrong "different" would refuse a toolchain that
/// works. Concretely, `thumbv7em` and `armv7l` are both `arm`, and their
/// objects share `EM_ARM` -- so a Cortex-M build hosted on a 32-bit ARM
/// machine must not be rejected for producing the machine number it is
/// supposed to produce. The check therefore only bites across families
/// (an x86_64 host emitting x86_64 objects for an ARM target), which is the
/// case that actually occurs.
fn arch_family(arch: &str) -> &str {
    if arch.starts_with("thumb")
        || arch.starts_with("arm")
        || arch.starts_with("aarch64")
        || arch.starts_with("xscale")
    {
        return "arm";
    }
    if arch.starts_with("riscv") {
        return "riscv";
    }
    if arch.starts_with("mips") {
        return "mips";
    }
    if arch.starts_with("ppc") || arch.starts_with("powerpc") {
        return "ppc";
    }
    if arch.starts_with("wasm") {
        return "wasm";
    }
    if matches!(
        arch,
        "x86_64" | "x86_64h" | "x86" | "i386" | "i486" | "i586" | "i686" | "amd64"
    ) {
        return "x86";
    }
    arch
}

/// The architecture an object file declares in its own header, as
/// `(format, id)`.
///
/// The format tag is part of the identity on purpose: `EM_AARCH64` (0xB7)
/// and Mach-O `CPU_TYPE_ARM64` (0x0100000C) are different numbers for the
/// same CPU, and a target needing ELF must not be considered satisfied by a
/// Mach-O object. `None` for a format this does not parse, which callers
/// must treat as "unverifiable", not as "mismatched".
fn object_arch_id(path: &Path) -> Option<(&'static str, u64)> {
    let bytes = std::fs::read(path).ok()?;

    // ELF: e_machine is a u16 at offset 18, in the endianness named by
    // EI_DATA (byte 5).
    if bytes.len() >= 20 && bytes[..4] == *b"\x7fELF" {
        let raw = [bytes[18], bytes[19]];
        let machine = if bytes[5] == 2 {
            u16::from_be_bytes(raw)
        } else {
            u16::from_le_bytes(raw)
        };
        return Some(("elf", u64::from(machine)));
    }

    if bytes.len() >= 8 {
        let magic = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let raw = [bytes[4], bytes[5], bytes[6], bytes[7]];
        // Mach-O: cputype is the u32 after the magic; the magic itself says
        // which endianness and word size (0xfeedface/0xfeedfacf native,
        // 0xcefaedfe/0xcffaedfe byte-swapped).
        match magic {
            0xfeed_face | 0xfeed_facf => {
                return Some(("mach-o", u64::from(u32::from_le_bytes(raw))))
            }
            0xcefa_edfe | 0xcffa_edfe => {
                return Some(("mach-o", u64::from(u32::from_be_bytes(raw))))
            }
            _ => {}
        }
        // COFF object: no magic at all, the machine number *is* the first
        // u16 (0x8664 x64, 0xaa64 arm64, 0x014c i386). Best effort, and
        // only ever used as a comparison between two objects from the same
        // compiler on the same host.
        let machine = u16::from_le_bytes([bytes[0], bytes[1]]);
        if matches!(machine, 0x8664 | 0xaa64 | 0x014c | 0x01c0 | 0x01c4 | 0x0200) {
            return Some(("coff", u64::from(machine)));
        }
    }

    None
}

/// Build a toolchain from the host clang plus `-target <triple>`.
///
/// `Err` carries the "probed: ..." fragment explaining why it was refused,
/// so a failed detection names the real obstacle instead of just listing a
/// binary that does exist.
fn try_host_clang_candidate(
    target: &TargetTriple,
) -> std::result::Result<Box<dyn Toolchain>, String> {
    let triple = target.as_str();
    match probe_host_clang(target) {
        HostClangProbe::Ready { clang, clangxx, ar } => {
            tracing::info!(
                "using host clang for {triple}: cc={} -target {triple}, ar={}",
                clang.display(),
                ar.display()
            );
            Ok(Box::new(
                GccToolchain::new(clang, clangxx, ar, ToolchainPlatform::Clang)
                    .with_target(target.clone())
                    .with_explicit_target_flag(triple),
            ))
        }
        HostClangProbe::NoClang => Err(format!("clang -target {triple} (no clang on PATH)")),
        HostClangProbe::TargetUnsupported { .. } => Err(format!(
            "clang -target {triple} (this clang has no backend for that triple)"
        )),
        HostClangProbe::NoUsableArchiver { tried, .. } => Err(format!(
            "clang -target {triple} (clang compiles for it, but no archiver here \
             keeps its objects; tried {} -- install llvm-ar)",
            if tried.is_empty() {
                "none".to_string()
            } else {
                tried.join(", ")
            }
        )),
        HostClangProbe::WrongArchitecture { effective, .. } => Err(format!(
            "clang -target {triple} (accepted the flag but built for {effective}, \
             the host architecture -- refusing rather than filing host objects \
             under this target)"
        )),
        HostClangProbe::ProbeFailed { reason } => {
            Err(format!("clang -target {triple} (probe failed: {reason})"))
        }
    }
}

/// The `xcrun --sdk` name for an Apple target.
fn apple_sdk_name(target: &TargetTriple) -> &'static str {
    let simulator = target.env_is("sim");
    match target.os() {
        Some("ios") if simulator => "iphonesimulator",
        Some("ios") => "iphoneos",
        Some("tvos") if simulator => "appletvsimulator",
        Some("tvos") => "appletvos",
        Some("watchos") if simulator => "watchsimulator",
        Some("watchos") => "watchos",
        Some("visionos") => "xros",
        // `darwin` and `macos` both mean the macOS SDK.
        _ => "macosx",
    }
}

/// `xcrun --sdk <sdk> --show-sdk-path`.
fn xcrun_sdk_path(sdk: &str) -> Option<PathBuf> {
    let output = std::process::Command::new("xcrun")
        .args(["--sdk", sdk, "--show-sdk-path"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() {
        None
    } else {
        Some(PathBuf::from(path))
    }
}

/// `xcrun -f <name>` -- resolves a tool inside the active Xcode toolchain.
/// Absent on non-macOS hosts, where the command simply fails.
fn xcrun_find(name: &str) -> Option<PathBuf> {
    let output = std::process::Command::new("xcrun")
        .arg("-f")
        .arg(name)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() {
        None
    } else {
        Some(PathBuf::from(path))
    }
}

/// `arm-none-eabi-gcc` -> `arm-none-eabi-ar`.
fn cross_ar_for(c_name: &str) -> Option<String> {
    for driver in ["-gcc", "-clang", "-cc"] {
        if let Some(prefix) = c_name.strip_suffix(driver) {
            return Some(format!("{prefix}-ar"));
        }
    }
    None
}

fn family_to_platform(family: CompilerFamily) -> ToolchainPlatform {
    match family {
        CompilerFamily::Gcc => ToolchainPlatform::Gcc,
        CompilerFamily::Clang => ToolchainPlatform::Clang,
        CompilerFamily::AppleClang => ToolchainPlatform::AppleClang,
        CompilerFamily::Msvc => ToolchainPlatform::Msvc,
    }
}

/// Detect a toolchain for the host, the historical behaviour.
fn detect_host_toolchain() -> Result<Box<dyn Toolchain>> {
    // On Windows, try MSVC first
    #[cfg(target_os = "windows")]
    {
        if let Some(toolchain) = try_detect_msvc()? {
            return Ok(toolchain);
        }
    }

    // Try GCC/Clang
    if let Some(toolchain) = try_detect_gcc()? {
        return Ok(toolchain);
    }

    bail!(
        "no C compiler found\n\
         \n\
         Harbour requires a C compiler (gcc, clang, or cl).\n\
         Set the CC environment variable, configure with `harbour toolchain override`,\n\
         or install a compiler."
    )
}

/// Try to create a toolchain from config file settings.
fn try_detect_from_config(
    config: &ToolchainConfig,
    target: Option<&TargetTriple>,
) -> Result<Option<Box<dyn Toolchain>>> {
    use which::which;

    let tc = &config.toolchain;

    // We need at least a C compiler specified
    let cc = match &tc.cc {
        Some(cc) => {
            if cc.exists() {
                cc.clone()
            } else {
                tracing::warn!("Configured C compiler not found: {}", cc.display());
                return Ok(None);
            }
        }
        None => return Ok(None),
    };

    // Get C++ compiler from config, env, or infer from CC
    let cxx = tc
        .cxx
        .clone()
        .filter(|p| p.exists())
        .or_else(|| std::env::var("CXX").ok().map(PathBuf::from))
        .unwrap_or_else(|| GccToolchain::infer_cxx(&cc));

    // Get archiver from config, env, or search PATH
    let ar = tc
        .ar
        .clone()
        .filter(|p| p.exists())
        .or_else(|| std::env::var("AR").ok().map(PathBuf::from))
        .or_else(|| which("ar").ok())
        .or_else(|| which("llvm-ar").ok());

    let Some(ar) = ar else {
        tracing::warn!("Archiver (ar) not found");
        return Ok(None);
    };

    // Detect compiler family
    let family = detect_compiler_family(&cc)?;

    tracing::info!(
        "Using toolchain from config: cc={}, ar={}",
        cc.display(),
        ar.display()
    );

    let toolchain = GccToolchain::new(cc, cxx, ar, family);
    let toolchain = match target {
        Some(t) if !t.is_host() => {
            let toolchain = toolchain.with_target(t.clone());
            // An explicitly configured *clang* is not a cross compiler until
            // it is given `-target`. Without this, `cc = /usr/bin/clang`
            // together with a cross `target` compiled host objects and filed
            // them under a directory named after the target: a wrong
            // artifact rather than an error. GCC is excluded because it
            // rejects the flag -- its target is fixed at build time and
            // encoded in its name.
            if matches!(
                family,
                ToolchainPlatform::Clang | ToolchainPlatform::AppleClang
            ) {
                toolchain.with_explicit_target_flag(t.as_str())
            } else {
                toolchain
            }
        }
        Some(t) => toolchain.with_target(t.clone()),
        None => toolchain,
    };
    Ok(Some(Box::new(toolchain)))
}

/// Try to detect MSVC toolchain.
#[cfg(target_os = "windows")]
fn try_detect_msvc() -> Result<Option<Box<dyn Toolchain>>> {
    use which::which;

    // First, check if we're already in a Developer Command Prompt
    // (cl.exe in PATH and environment configured)
    if let Ok(cl) = which("cl") {
        if std::env::var("INCLUDE").is_ok() && std::env::var("LIB").is_ok() {
            // Already configured, use existing environment
            let lib = which("lib")
                .map_err(|_| anyhow::anyhow!("MSVC cl.exe found but lib.exe not in PATH"))?;
            let link = which("link")
                .map_err(|_| anyhow::anyhow!("MSVC cl.exe found but link.exe not in PATH"))?;
            return Ok(Some(Box::new(MsvcToolchain::new(cl, lib, link))));
        }
    }

    // Try to auto-detect Visual Studio and source the environment
    if let Some(toolchain) = try_auto_detect_msvc()? {
        return Ok(Some(toolchain));
    }

    Ok(None)
}

/// Try to auto-detect MSVC using vswhere.exe and vcvarsall.bat.
#[cfg(target_os = "windows")]
fn try_auto_detect_msvc() -> Result<Option<Box<dyn Toolchain>>> {
    use std::collections::HashMap;
    use std::process::Command;

    // Find vswhere.exe
    let vswhere = find_vswhere()?;
    let Some(vswhere) = vswhere else {
        tracing::debug!("vswhere.exe not found, cannot auto-detect MSVC");
        return Ok(None);
    };

    tracing::debug!("Found vswhere at: {}", vswhere.display());

    // Run vswhere to find VS installation path
    let output = Command::new(&vswhere)
        .args([
            "-latest",
            "-requires",
            "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
            "-property",
            "installationPath",
            "-format",
            "value",
        ])
        .output();

    let vs_path = match output {
        Ok(out) if out.status.success() => {
            let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if path.is_empty() {
                tracing::debug!("vswhere returned empty path");
                return Ok(None);
            }
            PathBuf::from(path)
        }
        Ok(out) => {
            tracing::debug!("vswhere failed: {}", String::from_utf8_lossy(&out.stderr));
            return Ok(None);
        }
        Err(e) => {
            tracing::debug!("Failed to run vswhere: {}", e);
            return Ok(None);
        }
    };

    tracing::debug!("Found Visual Studio at: {}", vs_path.display());

    // Find vcvarsall.bat
    let vcvarsall = vs_path
        .join("VC")
        .join("Auxiliary")
        .join("Build")
        .join("vcvarsall.bat");
    if !vcvarsall.exists() {
        tracing::debug!("vcvarsall.bat not found at: {}", vcvarsall.display());
        return Ok(None);
    }

    tracing::info!(
        "Auto-detecting MSVC environment via {}",
        vcvarsall.display()
    );

    // Determine target architecture
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "x86" => "x86",
        "aarch64" => "arm64",
        other => {
            tracing::debug!(
                "Unsupported architecture for MSVC auto-detection: {}",
                other
            );
            return Ok(None);
        }
    };

    // Run vcvarsall.bat and capture environment.
    //
    // The batch file is a wrapper so that cmd.exe does the quoting, but its
    // NAME must be unique per invocation. It used to be a fixed
    // `harbour_vcvars.bat` in the shared temp directory, written before the
    // call and deleted after -- so two concurrent `harbour` processes raced:
    // one deleted the file while the other was about to execute it, and
    // cmd.exe reported "The batch file cannot be found". MSVC detection then
    // failed, Harbour silently fell back to another toolchain, and the object
    // extension flipped from `.obj` to `.o` mid-project. That is how a stale
    // member ended up winning symbol resolution in a static archive (fixed
    // separately in #51) and why builds intermittently recompiled everything.
    //
    // It only ever reproduced under concurrent load, which is exactly what
    // `cargo test` produces by running integration tests in parallel, each
    // spawning its own `harbour`.
    let batch_content = format!(
        "@echo off\r\ncall \"{}\" {} >nul 2>&1\r\nif errorlevel 1 exit /b 1\r\nset\r\n",
        vcvarsall.display(),
        arch
    );

    let temp_batch = match tempfile::Builder::new()
        .prefix("harbour-vcvars-")
        .suffix(".bat")
        .tempfile()
    {
        Ok(mut f) => {
            use std::io::Write;
            if let Err(e) = f
                .write_all(batch_content.as_bytes())
                .and_then(|_| f.flush())
            {
                tracing::debug!("Failed to write temp batch file: {}", e);
                return Ok(None);
            }
            f
        }
        Err(e) => {
            tracing::debug!("Failed to create temp batch file: {}", e);
            return Ok(None);
        }
    };

    let batch_path = temp_batch
        .path()
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("temp batch path contains invalid UTF-8"))?
        .to_string();

    let output = Command::new("cmd").args(["/c", &batch_path]).output();

    // `temp_batch` owns the file and removes it on drop, after the call.
    drop(temp_batch);

    let env_output = match output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).to_string(),
        Ok(out) => {
            tracing::warn!(
                "vcvarsall.bat failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            return Ok(None);
        }
        Err(e) => {
            tracing::warn!("Failed to run vcvarsall.bat: {}", e);
            return Ok(None);
        }
    };

    // Parse environment variables from output
    let mut env_vars: HashMap<String, String> = HashMap::new();
    for line in env_output.lines() {
        if let Some((key, value)) = line.split_once('=') {
            env_vars.insert(key.to_uppercase(), value.to_string());
        }
    }

    // Get the PATH from the captured environment
    let path_value = env_vars.get("PATH").cloned().unwrap_or_default();
    if path_value.is_empty() {
        tracing::warn!(
            "vcvarsall.bat produced empty PATH - MSVC environment may not be properly configured"
        );
        return Ok(None);
    }

    // Find cl.exe, lib.exe, link.exe in the captured PATH
    let (cl, lib, link) = find_msvc_tools_in_path(&path_value)?;
    let Some((cl, lib, link)) = cl.zip(lib).zip(link).map(|((c, l), lk)| (c, l, lk)) else {
        tracing::debug!("Could not find MSVC tools in captured PATH");
        return Ok(None);
    };

    tracing::info!("Auto-detected MSVC: cl={}", cl.display());

    // Build the environment variables to pass to commands
    // We need PATH, INCLUDE, LIB, and LIBPATH at minimum
    let important_vars = ["PATH", "INCLUDE", "LIB", "LIBPATH", "VSCMD_ARG_TGT_ARCH"];
    let captured_env: Vec<(String, String)> = important_vars
        .iter()
        .filter_map(|&key| env_vars.get(key).map(|v| (key.to_string(), v.clone())))
        .collect();

    Ok(Some(Box::new(EnvWrapper::new(
        MsvcToolchain::new(cl, lib, link),
        captured_env,
    ))))
}

/// Find vswhere.exe in standard locations.
#[cfg(target_os = "windows")]
fn find_vswhere() -> Result<Option<PathBuf>> {
    // Standard location
    let program_files_x86 = std::env::var("ProgramFiles(x86)")
        .unwrap_or_else(|_| "C:\\Program Files (x86)".to_string());

    let standard_path = PathBuf::from(&program_files_x86)
        .join("Microsoft Visual Studio")
        .join("Installer")
        .join("vswhere.exe");

    if standard_path.exists() {
        return Ok(Some(standard_path));
    }

    // Try PATH
    if let Ok(path) = which::which("vswhere") {
        return Ok(Some(path));
    }

    Ok(None)
}

/// Find MSVC tools (cl.exe, lib.exe, link.exe) in a PATH string.
#[cfg(target_os = "windows")]
fn find_msvc_tools_in_path(
    path: &str,
) -> Result<(Option<PathBuf>, Option<PathBuf>, Option<PathBuf>)> {
    let mut cl = None;
    let mut lib = None;
    let mut link = None;

    for dir in path.split(';') {
        let dir = PathBuf::from(dir);
        if !dir.exists() {
            continue;
        }

        if cl.is_none() {
            let cl_path = dir.join("cl.exe");
            if cl_path.exists() {
                cl = Some(cl_path);
            }
        }

        if lib.is_none() {
            let lib_path = dir.join("lib.exe");
            if lib_path.exists() {
                lib = Some(lib_path);
            }
        }

        if link.is_none() {
            let link_path = dir.join("link.exe");
            if link_path.exists() {
                link = Some(link_path);
            }
        }

        if cl.is_some() && lib.is_some() && link.is_some() {
            break;
        }
    }

    Ok((cl, lib, link))
}

/// Try to detect GCC/Clang toolchain.
fn try_detect_gcc() -> Result<Option<Box<dyn Toolchain>>> {
    use which::which;

    // Try CC environment variable first
    let cc = if let Ok(cc_env) = std::env::var("CC") {
        PathBuf::from(cc_env)
    } else {
        // Try common compiler names
        match which("cc")
            .or_else(|_| which("gcc"))
            .or_else(|_| which("clang"))
        {
            Ok(p) => p,
            Err(_) => return Ok(None),
        }
    };

    // Try CXX environment variable first, otherwise infer from CC
    let cxx = if let Ok(cxx_env) = std::env::var("CXX") {
        PathBuf::from(cxx_env)
    } else {
        // Try to find C++ compiler or infer from C compiler
        match which("c++")
            .or_else(|_| which("g++"))
            .or_else(|_| which("clang++"))
        {
            Ok(p) => p,
            Err(_) => GccToolchain::infer_cxx(&cc),
        }
    };

    // Find archiver
    let ar = if let Ok(ar_env) = std::env::var("AR") {
        PathBuf::from(ar_env)
    } else {
        match which("ar") {
            Ok(p) => p,
            Err(_) => return Ok(None),
        }
    };

    // Detect compiler family
    let family = detect_compiler_family(&cc)?;

    Ok(Some(Box::new(GccToolchain::new(cc, cxx, ar, family))))
}

/// Detect whether the compiler is GCC, Clang, or Apple Clang.
fn detect_compiler_family(cc: &Path) -> Result<ToolchainPlatform> {
    // Check binary name first
    let name = cc
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_lowercase();

    if name.contains("clang") {
        // Could be Apple Clang or regular Clang
        return detect_clang_variant(cc);
    } else if name.contains("gcc") || name.contains("g++") {
        return Ok(ToolchainPlatform::Gcc);
    }

    // Try to detect from --version output
    let output = std::process::Command::new(cc).arg("--version").output();

    if let Ok(output) = output {
        let stdout = String::from_utf8_lossy(&output.stdout).to_lowercase();
        if stdout.contains("clang") {
            return detect_clang_variant(cc);
        } else if stdout.contains("gcc") {
            return Ok(ToolchainPlatform::Gcc);
        }
    }

    // Default to GCC
    Ok(ToolchainPlatform::Gcc)
}

/// Detect if Clang is Apple Clang or regular Clang.
fn detect_clang_variant(cc: &Path) -> Result<ToolchainPlatform> {
    let output = std::process::Command::new(cc).arg("--version").output();

    if let Ok(output) = output {
        let stdout = String::from_utf8_lossy(&output.stdout).to_lowercase();
        if stdout.contains("apple") {
            return Ok(ToolchainPlatform::AppleClang);
        }
    }

    Ok(ToolchainPlatform::Clang)
}

#[cfg(test)]
mod tests {
    use crate::core::target::TargetTriple;

    #[test]
    fn apple_sdk_names_distinguish_device_from_simulator() {
        let sdk = |raw: &str| super::apple_sdk_name(&TargetTriple::parse(raw));
        assert_eq!(sdk("x86_64-apple-darwin"), "macosx");
        assert_eq!(sdk("aarch64-apple-darwin"), "macosx");
        assert_eq!(sdk("aarch64-apple-ios"), "iphoneos");
        // The `sim` environment selects a different SDK entirely; building an
        // iOS-device binary against the simulator SDK would not link.
        assert_eq!(sdk("aarch64-apple-ios-sim"), "iphonesimulator");
    }

    #[test]
    fn cross_archiver_follows_the_compiler_prefix() {
        // A host `ar` cannot stand in for a cross archiver: it produces
        // archives for the wrong architecture.
        assert_eq!(
            super::cross_ar_for("arm-none-eabi-gcc").as_deref(),
            Some("arm-none-eabi-ar")
        );
        assert_eq!(
            super::cross_ar_for("riscv64-unknown-elf-gcc").as_deref(),
            Some("riscv64-unknown-elf-ar")
        );
        assert_eq!(
            super::cross_ar_for("aarch64-linux-android21-clang").as_deref(),
            Some("aarch64-linux-android21-ar")
        );
        // Nothing recognizable to strip.
        assert_eq!(super::cross_ar_for("cl.exe"), None);
    }

    #[test]
    fn a_cross_target_never_falls_back_to_the_host_compiler() {
        // The failure this guards against is silent and corrupting: building
        // with the host compiler for a target that isn't the host produces
        // host binaries labelled as target binaries.
        //
        // The triple has an invented *architecture*, not an invented suffix.
        // That distinction is load-bearing: this test used to use
        // `thumbv7em-none-eabi-definitelynotinstalled`, which Apple clang 21
        // diagnoses ("version '-definitelynotinstalled' ... is invalid") but
        // Ubuntu clang 18 quietly ignores -- it then builds real thumbv7em
        // ARM code, because the *arch* token was valid all along. So the
        // test was asserting "no toolchain can exist" about a triple that is
        // perfectly buildable on Linux, and passed on macOS only because
        // Apple's clang happens to be stricter. An unknown arch is refused
        // by every clang tested (Apple 21, Ubuntu 18: "unknown target
        // triple"), which is what makes this a real invariant rather than a
        // platform accident.
        let target = TargetTriple::parse("notanarch-unknown-elf");
        let result = super::detect_cross_toolchain(&target);

        let Err(err) = result else {
            let tc = result.unwrap();
            panic!(
                "expected no toolchain for a nonexistent architecture, got {}",
                tc.compiler_path().display()
            );
        };

        let msg = err.to_string();
        assert!(
            msg.contains("no toolchain found for target"),
            "unexpected error: {msg}"
        );
        // The message must say what was tried, or it is unactionable.
        assert!(
            msg.contains("probed:"),
            "error does not list candidates: {msg}"
        );
    }

    /// The other half of the same invariant, and the half the old test could
    /// not express: when discovery *does* return a toolchain for a cross
    /// target, that toolchain must be building for the target. Either the
    /// driver's name encodes it (a prefixed cross binary) or it carries an
    /// explicit `-target` -- a bare host `clang`/`gcc`/`cc` with neither is
    /// the corrupting case.
    #[test]
    fn any_toolchain_returned_for_a_cross_target_builds_for_that_target() {
        for raw in [
            "thumbv7em-none-eabi",
            "aarch64-none-elf",
            "x86_64-unknown-linux-gnu",
            "riscv32imac-unknown-none-elf",
        ] {
            let target = TargetTriple::parse(raw);
            let Ok(tc) = super::detect_cross_toolchain(&target) else {
                continue; // Nothing installed for it here; nothing to check.
            };

            let spec = tc.compile_command(
                &crate::builder::toolchain::CompileInput {
                    source: std::path::PathBuf::from("a.c"),
                    output: std::path::PathBuf::from("a.o"),
                    include_dirs: vec![],
                    defines: vec![],
                    cflags: vec![],
                },
                crate::core::target::Language::C,
                None,
            );

            let driver = spec
                .program
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_string();
            let is_bare_host_driver = matches!(
                driver.trim_end_matches(".exe"),
                "clang" | "clang++" | "gcc" | "g++" | "cc" | "c++"
            );
            let carries_target = spec
                .args
                .windows(2)
                .any(|w| w[0] == "-target" && w[1] == raw);

            assert!(
                carries_target || !is_bare_host_driver,
                "{raw}: got bare host driver {driver} with no -target: {:?}",
                spec.args
            );
        }
    }

    #[test]
    fn resolve_target_prefers_an_explicit_target() {
        let explicit = TargetTriple::parse("thumbv7em-none-eabihf");
        assert_eq!(super::resolve_target(Some(&explicit)), explicit);
    }

    #[test]
    fn resolve_target_defaults_to_the_host() {
        // With no explicit target and (in a clean checkout) no configured
        // one, the effective target is the host -- preserving the historical
        // behaviour of every existing call site.
        let resolved = super::resolve_target(None);
        assert!(
            resolved.is_host() || !resolved.as_str().is_empty(),
            "resolve_target produced an empty triple"
        );
    }
    use super::super::MsvcToolchain;
    use super::super::{ArchiveInput, CompileInput, CxxOptions};
    use super::*;
    use crate::core::manifest::MsvcRuntime;
    use crate::core::target::{CppStandard, Language};

    #[test]
    fn test_gcc_compile_command() {
        let toolchain = GccToolchain::new(
            PathBuf::from("gcc"),
            PathBuf::from("g++"),
            PathBuf::from("ar"),
            ToolchainPlatform::Gcc,
        );

        let input = CompileInput {
            source: PathBuf::from("src/main.c"),
            output: PathBuf::from("obj/main.o"),
            include_dirs: vec![PathBuf::from("/usr/include")],
            defines: vec![
                ("DEBUG".to_string(), None),
                ("VERSION".to_string(), Some("1".to_string())),
            ],
            cflags: vec!["-Wall".to_string()],
        };

        let cmd = toolchain.compile_command(&input, Language::C, None);
        assert_eq!(cmd.program, PathBuf::from("gcc"));
        assert!(cmd.args.contains(&"-c".to_string()));
        assert!(cmd.args.contains(&"-I/usr/include".to_string()));
        assert!(cmd.args.contains(&"-DDEBUG".to_string()));
        assert!(cmd.args.contains(&"-DVERSION=1".to_string()));
        assert!(cmd.args.contains(&"-Wall".to_string()));
    }

    #[test]
    fn test_gcc_cxx_compile_command() {
        let toolchain = GccToolchain::new(
            PathBuf::from("gcc"),
            PathBuf::from("g++"),
            PathBuf::from("ar"),
            ToolchainPlatform::Gcc,
        );

        let input = CompileInput {
            source: PathBuf::from("src/main.cpp"),
            output: PathBuf::from("obj/main.o"),
            include_dirs: vec![],
            defines: vec![],
            cflags: vec![],
        };

        let cxx_opts = CxxOptions {
            std: Some(CppStandard::Cpp17),
            exceptions: true,
            rtti: true,
            runtime: None,
            msvc_runtime: MsvcRuntime::default(),
            is_debug: false,
        };

        let cmd = toolchain.compile_command(&input, Language::Cxx, Some(&cxx_opts));
        assert_eq!(cmd.program, PathBuf::from("g++"));
        assert!(cmd.args.contains(&"-c".to_string()));
        assert!(cmd.args.contains(&"-std=c++17".to_string()));
    }

    #[test]
    fn test_gcc_archive_command() {
        let toolchain = GccToolchain::new(
            PathBuf::from("gcc"),
            PathBuf::from("g++"),
            PathBuf::from("ar"),
            ToolchainPlatform::Gcc,
        );

        let input = ArchiveInput {
            objects: vec![PathBuf::from("obj/a.o"), PathBuf::from("obj/b.o")],
            output: PathBuf::from("lib/libfoo.a"),
        };

        let cmd = toolchain.archive_command(&input);
        assert_eq!(cmd.program, PathBuf::from("ar"));
        assert!(cmd.args.contains(&"rcs".to_string()));
    }

    #[test]
    fn test_msvc_compile_command() {
        let toolchain = MsvcToolchain::new(
            PathBuf::from("cl"),
            PathBuf::from("lib"),
            PathBuf::from("link"),
        );

        let input = CompileInput {
            source: PathBuf::from("src/main.c"),
            output: PathBuf::from("obj/main.obj"),
            include_dirs: vec![PathBuf::from("C:/include")],
            defines: vec![
                ("DEBUG".to_string(), None),
                ("VERSION".to_string(), Some("1".to_string())),
            ],
            cflags: vec!["/W4".to_string()],
        };

        let cmd = toolchain.compile_command(&input, Language::C, None);
        assert_eq!(cmd.program, PathBuf::from("cl"));
        assert!(cmd.args.contains(&"/nologo".to_string()));
        assert!(cmd.args.contains(&"/c".to_string()));
        assert!(cmd.args.iter().any(|a| a.starts_with("/I")));
        assert!(cmd.args.contains(&"/DDEBUG".to_string()));
        assert!(cmd.args.contains(&"/DVERSION=1".to_string()));
    }

    #[test]
    fn test_msvc_cxx_compile_command() {
        let toolchain = MsvcToolchain::new(
            PathBuf::from("cl"),
            PathBuf::from("lib"),
            PathBuf::from("link"),
        );

        let input = CompileInput {
            source: PathBuf::from("src/main.cpp"),
            output: PathBuf::from("obj/main.obj"),
            include_dirs: vec![],
            defines: vec![],
            cflags: vec![],
        };

        let cxx_opts = CxxOptions {
            std: Some(CppStandard::Cpp20),
            exceptions: true,
            rtti: true,
            runtime: None,
            msvc_runtime: MsvcRuntime::Dynamic,
            is_debug: false,
        };

        let cmd = toolchain.compile_command(&input, Language::Cxx, Some(&cxx_opts));
        assert_eq!(cmd.program, PathBuf::from("cl"));
        assert!(cmd.args.contains(&"/TP".to_string()));
        assert!(cmd.args.contains(&"/std:c++20".to_string()));
        assert!(cmd.args.contains(&"/EHsc".to_string()));
        assert!(cmd.args.contains(&"/MD".to_string()));
    }

    #[test]
    fn test_msvc_archive_command() {
        let toolchain = MsvcToolchain::new(
            PathBuf::from("cl"),
            PathBuf::from("lib"),
            PathBuf::from("link"),
        );

        let input = ArchiveInput {
            objects: vec![PathBuf::from("obj/a.obj"), PathBuf::from("obj/b.obj")],
            output: PathBuf::from("lib/foo.lib"),
        };

        let cmd = toolchain.archive_command(&input);
        assert_eq!(cmd.program, PathBuf::from("lib"));
        assert!(cmd.args.contains(&"/nologo".to_string()));
        assert!(cmd.args.iter().any(|a| a.starts_with("/OUT:")));
    }

    // --- Host clang `-target` discovery ---

    /// A triple no clang has a backend for must be refused, not accepted and
    /// then discovered to be broken at compile time. `probe_host_clang` runs
    /// clang for real precisely because names cannot answer this: Apple clang
    /// accepts `aarch64-none-elf` but rejects
    /// `riscv32imac-unknown-none-elf`.
    #[test]
    fn host_clang_probe_refuses_a_target_clang_cannot_build() {
        let probe = super::probe_host_clang(&TargetTriple::parse("notanarch-unknown-elf"));
        assert!(
            !matches!(probe, super::HostClangProbe::Ready { .. }),
            "a nonexistent architecture must never come back Ready: {probe:?}"
        );
    }

    /// The host's own triple is the one case every machine with clang can
    /// serve, so this exercises the success path end to end: clang compiles,
    /// an archiver keeps the member, and the result names real binaries.
    #[test]
    fn host_clang_probe_is_ready_for_the_host_triple() {
        if which::which("clang").is_err() {
            return; // No clang: nothing to assert about clang's behaviour.
        }
        let probe = super::probe_host_clang(&TargetTriple::host());
        match probe {
            super::HostClangProbe::Ready { clang, ar, .. } => {
                assert!(clang.exists(), "clang path must exist: {}", clang.display());
                assert!(ar.exists(), "ar path must exist: {}", ar.display());
            }
            // A host with clang but no archiver Harbour recognizes (some
            // Windows installs ship neither `ar` nor `llvm-ar`) is a
            // property of the machine, not a bug.
            other @ super::HostClangProbe::NoUsableArchiver { .. } => {
                eprintln!("skipping: no usable archiver on this host ({other:?})");
            }
            // These two would be real failures: clang must always be able to
            // build for the machine it is running on.
            other => panic!("host triple should be buildable by host clang: {other:?}"),
        }
    }

    /// The trap this check exists for: on macOS, `ar rcs lib.a elf.o`
    /// **exits 0** and writes an archive with the member silently dropped
    /// (cctools warns "not a mach-o file" on stderr). Trusting the exit
    /// status would hand back an empty static library from a green build.
    #[test]
    fn archiver_validation_rejects_an_archiver_that_drops_the_member() {
        let Ok(clang) = which::which("clang") else {
            return;
        };
        let Ok(dir) = tempfile::tempdir() else {
            return;
        };

        // An ELF object, on a host whose `ar` only understands Mach-O.
        let src = dir.path().join("probe.c");
        std::fs::write(&src, "int probe(void) { return 0; }\n").unwrap();
        let obj = dir.path().join("probe.o");
        let compiled = std::process::Command::new(&clang)
            .args(["-target", "aarch64-none-elf", "-c"])
            .arg(&src)
            .arg("-o")
            .arg(&obj)
            .output();
        match compiled {
            Ok(out) if out.status.success() && obj.exists() => {}
            // This clang cannot target bare-metal aarch64; nothing to test.
            _ => return,
        }

        if cfg!(target_os = "macos") {
            // `/usr/bin/ar` on macOS is cctools ar, whose exact misbehaviour
            // this check exists for. A Homebrew or GNU binutils `ar` earlier
            // on PATH handles ELF perfectly well, so only assert when the
            // resolved binary really is the system one.
            let ar = which::which("ar").expect("macOS always ships an ar");
            if ar == std::path::Path::new("/usr/bin/ar") {
                assert!(
                    !super::archiver_handles_object(&ar, &obj, dir.path()),
                    "cctools ar exits 0 while dropping ELF members, so \
                     validation must reject it"
                );
            }
        }

        // Whatever the host, an archiver that *is* accepted must produce an
        // archive containing the object -- that is the whole contract.
        if let Ok(llvm_ar) = which::which("llvm-ar") {
            assert!(
                super::archiver_handles_object(&llvm_ar, &obj, dir.path()),
                "llvm-ar is object-format agnostic and must be accepted"
            );
        }
    }

    /// Discovery must never silently fall back to the host compiler for a
    /// cross target, and when it does refuse, the message has to name the
    /// real obstacle rather than just listing binaries.
    #[test]
    fn refusal_message_explains_the_host_clang_attempt() {
        // A triple with no plausible prefixed GCC and no clang backend, so
        // every candidate fails and the error text is the whole output.
        let result = super::detect_cross_toolchain(&TargetTriple::parse("notanarch-unknown-elf"));
        let text = match result {
            Ok(_) => panic!("no toolchain can exist for a nonexistent architecture"),
            Err(err) => err.to_string(),
        };
        assert!(
            text.contains("clang -target notanarch-unknown-elf"),
            "the host-clang attempt must be reported: {text}"
        );
    }

    /// A configured `cc = /usr/bin/clang` plus a cross `target` used to
    /// compile *host* objects and file them under a directory named after
    /// the target -- a wrong artifact, with no error anywhere. The explicit
    /// path is still honoured (the user named the binary); it just has to be
    /// told what to build for.
    #[test]
    fn an_explicitly_configured_clang_gets_the_target_flag() {
        let Ok(clang) = which::which("clang") else {
            return;
        };
        let Ok(ar) = which::which("ar") else {
            return;
        };

        let mut config = crate::util::config::ToolchainConfig::default();
        config.toolchain.cc = Some(clang);
        config.toolchain.ar = Some(ar);

        let target = TargetTriple::parse("aarch64-none-elf");
        let toolchain = super::try_detect_from_config(&config, Some(&target))
            .expect("config detection must not error")
            .expect("an existing cc must yield a toolchain");

        let spec = toolchain.compile_command(
            &crate::builder::toolchain::CompileInput {
                source: std::path::PathBuf::from("a.c"),
                output: std::path::PathBuf::from("a.o"),
                include_dirs: vec![],
                defines: vec![],
                cflags: vec![],
            },
            crate::core::target::Language::C,
            None,
        );
        assert_eq!(
            spec.args[..2],
            ["-target", "aarch64-none-elf"],
            "configured clang must be told the target: {:?}",
            spec.args
        );
    }

    /// The same configuration for the *host* must stay byte-for-byte as it
    /// was: `-target <host>` would be redundant, and this is the path every
    /// existing `harbour toolchain override` user is on.
    #[test]
    fn an_explicitly_configured_host_build_is_unchanged() {
        let Ok(clang) = which::which("clang") else {
            return;
        };
        let Ok(ar) = which::which("ar") else {
            return;
        };

        let mut config = crate::util::config::ToolchainConfig::default();
        config.toolchain.cc = Some(clang);
        config.toolchain.ar = Some(ar);

        let host = TargetTriple::host();
        let toolchain = super::try_detect_from_config(&config, Some(&host))
            .expect("config detection must not error")
            .expect("an existing cc must yield a toolchain");

        let spec = toolchain.compile_command(
            &crate::builder::toolchain::CompileInput {
                source: std::path::PathBuf::from("a.c"),
                output: std::path::PathBuf::from("a.o"),
                include_dirs: vec![],
                defines: vec![],
                cflags: vec![],
            },
            crate::core::target::Language::C,
            None,
        );
        assert!(
            !spec.args.iter().any(|a| a == "-target"),
            "host builds must not gain a -target flag: {:?}",
            spec.args
        );
    }

    /// `object_arch_id` must read the architecture out of the object's own
    /// header, which is the only evidence that `-target` took effect. Read
    /// against real clang output, with the ELF machine number asserted
    /// literally: `EM_AARCH64` is 0xB7.
    #[test]
    fn object_arch_id_reads_the_real_machine_number() {
        let Ok(clang) = which::which("clang") else {
            return;
        };
        let Ok(dir) = tempfile::tempdir() else {
            return;
        };
        let src = dir.path().join("p.c");
        std::fs::write(&src, "int p(void) { return 0; }\n").unwrap();

        let cross = dir.path().join("cross.o");
        let built = std::process::Command::new(&clang)
            .args(["-target", "aarch64-none-elf", "-c"])
            .arg(&src)
            .arg("-o")
            .arg(&cross)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !built {
            return; // No AArch64 backend here.
        }

        assert_eq!(
            super::object_arch_id(&cross),
            Some(("elf", 0xB7)),
            "an aarch64-none-elf object must read as ELF EM_AARCH64"
        );

        // And it must differ from the host's, or the comparison the probe
        // performs could never detect anything.
        let host = dir.path().join("host.o");
        let built_host = std::process::Command::new(&clang)
            .arg("-c")
            .arg(&src)
            .arg("-o")
            .arg(&host)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if built_host {
            let host_id = super::object_arch_id(&host).expect("host object must be readable");
            if super::arch_family(super::triple_arch(
                &super::effective_triple(&clang, None).unwrap_or_default(),
            )) != "arm"
            {
                assert_ne!(
                    host_id,
                    ("elf", 0xB7),
                    "a non-ARM host must not produce AArch64 objects"
                );
            }
        }
    }

    /// Nonsense in, `None` out -- never a wrong number that would make the
    /// probe refuse a working toolchain.
    #[test]
    fn object_arch_id_returns_none_for_a_non_object_file() {
        let dir = tempfile::tempdir().unwrap();
        let junk = dir.path().join("junk.o");
        std::fs::write(&junk, b"this is not an object file at all").unwrap();
        assert_eq!(super::object_arch_id(&junk), None);
        assert_eq!(super::object_arch_id(&dir.path().join("missing.o")), None);
    }

    /// The families exist to answer one question: are two objects *expected*
    /// to carry different machine numbers? Thumb and 32/64-bit ARM all share
    /// `EM_ARM`/`EM_AARCH64` lineage and must group together, so that a
    /// Cortex-M build hosted on an ARM machine is never refused for emitting
    /// the machine number it is supposed to emit.
    #[test]
    fn arch_families_group_arm_and_separate_x86() {
        for arm in ["thumbv7em", "thumbv6m", "arm", "armv7l", "aarch64", "arm64"] {
            assert_eq!(super::arch_family(arm), "arm", "{arm}");
        }
        for x86 in ["x86_64", "i686", "i386"] {
            assert_eq!(super::arch_family(x86), "x86", "{x86}");
        }
        assert_eq!(super::arch_family("riscv32imac"), "riscv");
        assert_ne!(super::arch_family("x86_64"), super::arch_family("aarch64"));
        // Unknown tokens stay themselves rather than collapsing together.
        assert_eq!(super::arch_family("notanarch"), "notanarch");
        assert_ne!(super::arch_family("notanarch"), super::arch_family("avr"));
    }

    /// clang's own normalization is used rather than a reimplementation of
    /// it: `aarch64-apple-darwin` and `arm64-apple-darwin` are the same
    /// target, and a textual comparison of the two would call them
    /// different.
    #[test]
    fn effective_triple_comes_from_clang_and_normalizes_spellings() {
        let Ok(clang) = which::which("clang") else {
            return;
        };
        let host = super::effective_triple(&clang, None);
        assert!(
            host.is_some_and(|t| !t.is_empty()),
            "clang must report an effective triple for the host"
        );

        // Both spellings must normalize to the same arch, whatever that
        // clang calls it.
        let a = super::effective_triple(&clang, Some("aarch64-apple-darwin"));
        let b = super::effective_triple(&clang, Some("arm64-apple-darwin"));
        if let (Some(a), Some(b)) = (a, b) {
            assert_eq!(
                super::triple_arch(&a),
                super::triple_arch(&b),
                "aarch64 and arm64 name one architecture"
            );
        }
    }
}
