//! Finding MSVC without a Developer Command Prompt.
//!
//! `cl.exe` is useless on its own: it finds the C library headers and import
//! libraries through `INCLUDE`, `LIB` and `LIBPATH`, and its own DLLs through
//! `PATH`, all of which `vcvarsall.bat` sets. A plain shell has none of that
//! and no `cl` on `PATH`, so discovery is two steps: ask `vswhere.exe` where
//! Visual Studio is installed, then run that install's `vcvarsall.bat` for the
//! right host/target architecture pair and *capture the environment it
//! produces*. That environment is then given to every `cl`, `link` and `lib`
//! invocation (see [`EnvWrapper`](super::EnvWrapper)).
//!
//! Order (see [`locate_msvc`]): an explicit `cc` override is handled before
//! this module is reached; then a `cl.exe` already on `PATH` whose
//! environment is already set up (a Developer Command Prompt, or CI that ran
//! `vcvarsall`) wins, because the user arranged it; `vswhere` is the fallback.
//!
//! Everything that decides something is a pure function over strings and maps
//! (architecture mapping, parsing `vswhere` and `set` output, choosing tool
//! paths), unit-tested on any OS. Everything that spawns a process or reads
//! the machine sits behind [`MsvcHost`]. [`SystemHost`] is the only real
//! implementation and refuses to do anything off Windows; the orchestration is
//! tested with a fake one. Nothing here is behind `#[cfg(windows)]`, so it is
//! type-checked on every platform. Only [`SystemHost`]'s Windows behaviour
//! (really running `vswhere.exe` and `cmd.exe`) is unexercised off Windows.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use crate::core::target::TargetTriple;
use crate::util::hash::Fingerprint;

/// The vswhere component that provides the x86/x64 compilers.
const COMPONENT_X86_X64: &str = "Microsoft.VisualStudio.Component.VC.Tools.x86.x64";
/// The vswhere component that provides the ARM64 compilers.
const COMPONENT_ARM64: &str = "Microsoft.VisualStudio.Component.VC.Tools.ARM64";

/// An architecture `vcvarsall.bat` knows by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VcArch {
    X86,
    X64,
    Arm64,
}

impl VcArch {
    /// The architecture of a triple's first component, or `None` for one MSVC
    /// has no toolset for.
    pub fn from_arch_token(arch: &str) -> Option<VcArch> {
        match arch {
            "x86_64" | "amd64" | "x86_64h" => Some(VcArch::X64),
            "i386" | "i486" | "i586" | "i686" | "x86" => Some(VcArch::X86),
            "aarch64" | "arm64" | "arm64ec" => Some(VcArch::Arm64),
            _ => None,
        }
    }

    /// The architecture of a Harbour target triple.
    pub fn from_triple(target: &TargetTriple) -> Option<VcArch> {
        VcArch::from_arch_token(target.arch())
    }

    /// The architecture this process runs on (`std::env::consts::ARCH`).
    pub fn host() -> Option<VcArch> {
        VcArch::from_arch_token(std::env::consts::ARCH)
    }

    /// The spelling `vcvarsall.bat` (and `VSCMD_ARG_TGT_ARCH`) uses.
    pub fn vcvars_name(self) -> &'static str {
        match self {
            VcArch::X86 => "x86",
            VcArch::X64 => "x64",
            VcArch::Arm64 => "arm64",
        }
    }
}

/// The `vcvarsall.bat` argument for building on `host` for `target`: the bare
/// architecture when they are the same (`x64`), else `<host>_<target>`
/// (`x64_arm64`).
pub fn vcvarsall_arg(host: VcArch, target: VcArch) -> String {
    if host == target {
        target.vcvars_name().to_string()
    } else {
        format!("{}_{}", host.vcvars_name(), target.vcvars_name())
    }
}

/// The Visual Studio component that must be installed to compile for `target`.
pub fn required_component(target: VcArch) -> &'static str {
    match target {
        VcArch::X86 | VcArch::X64 => COMPONENT_X86_X64,
        VcArch::Arm64 => COMPONENT_ARM64,
    }
}

/// The arguments `vswhere.exe` is run with to find the install directory.
pub fn vswhere_args(component: &str) -> Vec<String> {
    [
        "-latest",
        "-products",
        "*",
        "-requires",
        component,
        "-property",
        "installationPath",
        "-utf8",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// The install directory out of `vswhere` output: the first non-empty line.
///
/// Tolerates a leading byte-order mark, CRLF, and an `installationPath:`
/// label in case a vswhere version prints the property in `key: value` form.
/// `None` when nothing matched, which is how vswhere reports "no install has
/// that component".
pub fn parse_vswhere_output(stdout: &str) -> Option<PathBuf> {
    let line = stdout
        .trim_start_matches('\u{feff}')
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())?;
    let value = line
        .strip_prefix("installationPath:")
        .map(str::trim)
        .unwrap_or(line);
    if value.is_empty() {
        None
    } else {
        Some(PathBuf::from(value))
    }
}

/// `<install>\VC\Auxiliary\Build\vcvarsall.bat`.
pub fn vcvarsall_path(install: &Path) -> PathBuf {
    install
        .join("VC")
        .join("Auxiliary")
        .join("Build")
        .join("vcvarsall.bat")
}

/// The batch file that runs `vcvarsall.bat` and then dumps the environment.
///
/// A file rather than `cmd /c "call ... && set"` on the command line, because
/// Rust's argument quoting and `cmd.exe`'s quote stripping disagree whenever a
/// path contains a space (`C:\Program Files\...`, always). `vcvarsall`'s own
/// output is discarded so only `set`'s lines reach stdout; a failing
/// `vcvarsall` exits non-zero before `set` runs.
pub fn vcvars_script(vcvarsall: &Path, arg: &str) -> String {
    format!(
        "@echo off\r\ncall \"{}\" {} >nul 2>&1\r\nif errorlevel 1 exit /b 1\r\nset\r\n",
        vcvarsall.display(),
        arg
    )
}

/// Parse `set` output into a map keyed by upper-cased name.
///
/// Windows environment names are case-insensitive and `cmd` reports `Path`,
/// not `PATH`, so upper-casing is what makes `PATH` findable. Only the first
/// `=` separates name from value (`INCLUDE` values and many others contain
/// more). CRLF is stripped. A line with no `=`, and `cmd`'s hidden per-drive
/// variables (`=C:=C:\dir`, whose name is empty), are skipped.
pub fn parse_set_output(output: &str) -> BTreeMap<String, String> {
    let mut vars = BTreeMap::new();
    for line in output.split('\n') {
        let line = line.trim_end_matches('\r');
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        vars.insert(name.to_uppercase(), value.to_string());
    }
    vars
}

/// The first directory of the `;`-separated `path` that holds `exe`
/// (`cl.exe`, ...), judged by `exists`.
pub fn find_in_path(path: &str, exe: &str, exists: &dyn Fn(&Path) -> bool) -> Option<PathBuf> {
    path.split(';')
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(|d| Path::new(d).join(exe))
        .find(|candidate| exists(candidate))
}

/// The three MSVC tools Harbour drives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MsvcTools {
    pub cl: PathBuf,
    pub lib: PathBuf,
    pub link: PathBuf,
}

/// Resolve `cl`, `lib` and `link` from the `PATH` in `env`.
///
/// Resolved from *that* PATH, never the process's: under Git Bash the
/// process's `link` is GNU coreutils', and `vcvarsall` puts MSVC's first.
/// The error names every tool that was not found.
pub fn choose_tools(
    env: &BTreeMap<String, String>,
    exists: &dyn Fn(&Path) -> bool,
) -> Result<MsvcTools, String> {
    let path = env.get("PATH").map(String::as_str).unwrap_or("");
    if path.is_empty() {
        return Err("the captured environment has no PATH".to_string());
    }
    let cl = find_in_path(path, "cl.exe", exists);
    let lib = find_in_path(path, "lib.exe", exists);
    let link = find_in_path(path, "link.exe", exists);
    match (cl, lib, link) {
        (Some(cl), Some(lib), Some(link)) => Ok(MsvcTools { cl, lib, link }),
        (cl, lib, link) => {
            let missing: Vec<&str> = [
                (cl.is_none(), "cl.exe"),
                (lib.is_none(), "lib.exe"),
                (link.is_none(), "link.exe"),
            ]
            .iter()
            .filter(|(m, _)| *m)
            .map(|(_, n)| *n)
            .collect();
            Err(format!(
                "{} not found on the resulting PATH",
                missing.join(", ")
            ))
        }
    }
}

/// The variables that differ from `current`: the ones worth giving a child.
///
/// A captured environment is the whole of `cmd`'s, most of it inherited and
/// identical. Passing only the delta keeps every command small and leaves
/// anything the process already has alone. `current` is compared by
/// upper-cased name, as Windows does.
pub fn env_delta(
    captured: &BTreeMap<String, String>,
    current: &[(String, String)],
) -> Vec<(String, String)> {
    let current: HashMap<String, &str> = current
        .iter()
        .map(|(k, v)| (k.to_uppercase(), v.as_str()))
        .collect();
    captured
        .iter()
        .filter(|(k, v)| current.get(k.as_str()) != Some(&v.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// The variables that decide *which* MSVC and Windows SDK a compile uses.
/// Whatever else `vcvarsall` sets, a change in one of these is a different
/// toolchain even if `cl.exe` sits at the same path (an in-place VS update
/// keeps the path and changes the toolset directory inside `INCLUDE`).
const IDENTITY_VARS: &[&str] = &[
    "INCLUDE",
    "LIB",
    "LIBPATH",
    "VCTOOLSVERSION",
    "VCINSTALLDIR",
    "WINDOWSSDKVERSION",
    "WINDOWSSDKDIR",
    "UCRTVERSION",
    "VSCMD_ARG_TGT_ARCH",
    "VSCMD_ARG_HOST_ARCH",
];

/// A digest of [`IDENTITY_VARS`] in `env`, for the toolchain fingerprint.
/// `None` when none is present (a toolchain that sets up no environment of
/// its own), so those fingerprints are unchanged.
pub fn identity_digest(env: &[(String, String)]) -> Option<String> {
    let mut fp = Fingerprint::new();
    let mut any = false;
    for name in IDENTITY_VARS {
        if let Some((_, v)) = env.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)) {
            fp.update_str(name);
            fp.update_str(v);
            any = true;
        }
    }
    any.then(|| fp.finish_short())
}

/// Parse the version out of `cl`'s banner, which is printed to stderr:
/// `Microsoft (R) C/C++ Optimizing Compiler Version 19.40.33811 for x64`.
pub fn parse_cl_banner_version(banner: &str) -> Option<String> {
    let rest = banner.split("Compiler Version ").nth(1)?;
    let version = rest.split_whitespace().next()?;
    if version.chars().next()?.is_ascii_digit() {
        Some(version.to_string())
    } else {
        None
    }
}

/// How an MSVC toolchain was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MsvcSource {
    /// `cl.exe` was already on `PATH` with `INCLUDE`/`LIB` set.
    Environment,
    /// Located with `vswhere.exe`, environment captured from `vcvarsall.bat`.
    Vswhere {
        vswhere: PathBuf,
        install_path: PathBuf,
        vcvarsall: PathBuf,
        vcvars_arg: String,
    },
}

/// A usable MSVC toolchain and the environment its tools must run in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MsvcFound {
    pub source: MsvcSource,
    pub tools: MsvcTools,
    /// Variables to add to every tool invocation. Empty for
    /// [`MsvcSource::Environment`], whose process environment already has them.
    pub env: Vec<(String, String)>,
}

impl MsvcFound {
    /// One line saying how it was found, for `toolchain show` and `doctor`.
    pub fn found_via(&self) -> String {
        match &self.source {
            MsvcSource::Environment => "found via PATH (environment already set up)".to_string(),
            MsvcSource::Vswhere {
                install_path,
                vcvars_arg,
                ..
            } => format!(
                "found via vswhere: {} (vcvarsall {})",
                install_path.display(),
                vcvars_arg
            ),
        }
    }
}

/// Everything discovery reads from or does to the machine.
pub trait MsvcHost {
    /// Whether this machine can run MSVC at all.
    fn is_windows(&self) -> bool;
    /// `%ProgramFiles(x86)%`.
    fn program_files_x86(&self) -> Option<PathBuf>;
    /// Resolve a program on the process's `PATH`.
    fn path_lookup(&self, name: &str) -> Option<PathBuf>;
    fn file_exists(&self, path: &Path) -> bool;
    /// A variable of the process environment.
    fn env_var(&self, name: &str) -> Option<String>;
    /// The whole process environment.
    fn current_env(&self) -> Vec<(String, String)>;
    /// Run `vswhere`, returning stdout, or why it could not be run.
    fn run_vswhere(&self, vswhere: &Path, args: &[String]) -> Result<String, String>;
    /// Run a batch script under `cmd`, returning stdout, or why it failed.
    fn run_batch(&self, script: &str) -> Result<String, String>;
}

/// The real machine.
pub struct SystemHost;

impl MsvcHost for SystemHost {
    fn is_windows(&self) -> bool {
        cfg!(windows)
    }

    fn program_files_x86(&self) -> Option<PathBuf> {
        Some(
            std::env::var_os("ProgramFiles(x86)")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("C:\\Program Files (x86)")),
        )
    }

    fn path_lookup(&self, name: &str) -> Option<PathBuf> {
        which::which(name).ok()
    }

    fn file_exists(&self, path: &Path) -> bool {
        path.is_file()
    }

    fn env_var(&self, name: &str) -> Option<String> {
        std::env::var(name).ok().filter(|v| !v.is_empty())
    }

    fn current_env(&self) -> Vec<(String, String)> {
        std::env::vars().collect()
    }

    fn run_vswhere(&self, vswhere: &Path, args: &[String]) -> Result<String, String> {
        let out = std::process::Command::new(vswhere)
            .args(args)
            .output()
            .map_err(|e| format!("could not run {}: {e}", vswhere.display()))?;
        if !out.status.success() {
            return Err(format!(
                "{} exited with {}: {}",
                vswhere.display(),
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn run_batch(&self, script: &str) -> Result<String, String> {
        use std::io::Write;

        // The file's NAME must be unique per invocation. A fixed name in the
        // shared temp directory raced between concurrent `harbour` processes
        // (one deleted it as the other was about to run it: "The batch file
        // cannot be found"), MSVC detection failed, Harbour silently fell back
        // to another compiler and the object extension flipped from `.obj` to
        // `.o` mid-project.
        let mut file = tempfile::Builder::new()
            .prefix("harbour-vcvars-")
            .suffix(".bat")
            .tempfile()
            .map_err(|e| format!("could not create a temporary batch file: {e}"))?;
        file.write_all(script.as_bytes())
            .and_then(|_| file.flush())
            .map_err(|e| format!("could not write the temporary batch file: {e}"))?;

        let out = std::process::Command::new("cmd")
            .arg("/c")
            .arg(file.path())
            .output()
            .map_err(|e| format!("could not run cmd.exe: {e}"))?;
        // `file` removes the batch file on drop, after the call.
        if !out.status.success() {
            return Err(format!("exited with {}", out.status));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

/// Locate `vswhere.exe`: the Visual Studio Installer's fixed location, then
/// `PATH`.
fn find_vswhere(host: &dyn MsvcHost) -> Result<PathBuf, String> {
    let fixed = host.program_files_x86().map(|pf| {
        pf.join("Microsoft Visual Studio")
            .join("Installer")
            .join("vswhere.exe")
    });
    if let Some(path) = &fixed {
        if host.file_exists(path) {
            return Ok(path.clone());
        }
    }
    if let Some(path) = host.path_lookup("vswhere") {
        return Ok(path);
    }
    Err(match fixed {
        Some(p) => format!("vswhere.exe not found at {} or on PATH", p.display()),
        None => "vswhere.exe not found on PATH".to_string(),
    })
}

/// Discover MSVC through `vswhere` + `vcvarsall.bat` for `target`.
///
/// Every `Err` is the precise reason, written to be shown in a "probed:" list.
pub fn discover_via_vswhere(
    host_arch: VcArch,
    target_arch: VcArch,
    host: &dyn MsvcHost,
) -> Result<MsvcFound, String> {
    if !host.is_windows() {
        return Err("vswhere is only available on Windows".to_string());
    }
    let vswhere = find_vswhere(host)?;
    let component = required_component(target_arch);
    let stdout = host.run_vswhere(&vswhere, &vswhere_args(component))?;
    let install_path = parse_vswhere_output(&stdout).ok_or_else(|| {
        format!(
            "vswhere found no Visual Studio install with {component} \
             (install the C++ build tools for {})",
            target_arch.vcvars_name()
        )
    })?;

    let vcvarsall = vcvarsall_path(&install_path);
    if !host.file_exists(&vcvarsall) {
        return Err(format!("{} does not exist", vcvarsall.display()));
    }
    let vcvars_arg = vcvarsall_arg(host_arch, target_arch);
    let output = host
        .run_batch(&vcvars_script(&vcvarsall, &vcvars_arg))
        .map_err(|e| format!("vcvarsall.bat {vcvars_arg} failed: {e}"))?;

    let captured = parse_set_output(&output);
    let tools = choose_tools(&captured, &|p| host.file_exists(p))
        .map_err(|e| format!("vcvarsall.bat {vcvars_arg} ran but {e}"))?;
    let env = env_delta(&captured, &host.current_env());

    Ok(MsvcFound {
        source: MsvcSource::Vswhere {
            vswhere,
            install_path,
            vcvarsall,
            vcvars_arg,
        },
        tools,
        env,
    })
}

/// A `cl.exe` already on `PATH` with its environment configured, if it is
/// usable for `target_arch`.
///
/// A native build accepts it as is. For another architecture the only evidence
/// it targets that architecture is `VSCMD_ARG_TGT_ARCH` (set by `vcvarsall`);
/// without it, or if it disagrees, the answer is no rather than a guess that
/// would build host code under the target's name.
fn from_environment(
    target_arch: VcArch,
    native: bool,
    host: &dyn MsvcHost,
) -> Result<MsvcFound, String> {
    let cl = host
        .path_lookup("cl")
        .ok_or_else(|| "cl.exe is not on PATH".to_string())?;
    if host.env_var("INCLUDE").is_none() || host.env_var("LIB").is_none() {
        return Err(format!(
            "{} is on PATH but INCLUDE/LIB are not set",
            cl.display()
        ));
    }
    if !native {
        match host.env_var("VSCMD_ARG_TGT_ARCH") {
            Some(a) if a.eq_ignore_ascii_case(target_arch.vcvars_name()) => {}
            Some(a) => {
                return Err(format!(
                    "the cl.exe on PATH targets {a}, not {}",
                    target_arch.vcvars_name()
                ))
            }
            None => {
                return Err(format!(
                    "cannot tell which architecture the cl.exe on PATH targets \
                     (VSCMD_ARG_TGT_ARCH unset), wanted {}",
                    target_arch.vcvars_name()
                ))
            }
        }
    }
    let lib = host
        .path_lookup("lib")
        .ok_or_else(|| "cl.exe is on PATH but lib.exe is not".to_string())?;
    let link = host
        .path_lookup("link")
        .ok_or_else(|| "cl.exe is on PATH but link.exe is not".to_string())?;
    Ok(MsvcFound {
        source: MsvcSource::Environment,
        tools: MsvcTools { cl, lib, link },
        env: Vec::new(),
    })
}

/// Find MSVC for `target`: a configured environment first, `vswhere` second.
///
/// `Err` carries both reasons, `; `-separated, so a failure says what was
/// tried.
pub fn locate_msvc(
    target: &TargetTriple,
    host_arch: Option<VcArch>,
    host: &dyn MsvcHost,
) -> Result<MsvcFound, String> {
    let target_arch = VcArch::from_triple(target)
        .ok_or_else(|| format!("MSVC has no toolset for architecture `{}`", target.arch()))?;

    // "Native" is by architecture, not by `TargetTriple::is_host`: a Developer
    // Command Prompt for the host architecture serves any target that shares it.
    let native = host_arch == Some(target_arch);
    let env_reason = match from_environment(target_arch, native, host) {
        Ok(found) => return Ok(found),
        Err(reason) => reason,
    };
    let host_arch = host_arch.ok_or_else(|| {
        format!(
            "{env_reason}; vswhere: unsupported host architecture `{}`",
            std::env::consts::ARCH
        )
    })?;
    discover_via_vswhere(host_arch, target_arch, host)
        .map_err(|vswhere_reason| format!("{env_reason}; vswhere: {vswhere_reason}"))
}

/// [`locate_msvc`] against the real machine, once per process per target
/// architecture.
///
/// Running `vcvarsall.bat` costs a second or more and a build context asks
/// several times (the target toolchain, the host toolchain for generators,
/// `toolchain show`), so the answer is remembered.
pub fn locate_msvc_cached(target: &TargetTriple) -> Result<MsvcFound, String> {
    static CACHE: OnceLock<Mutex<HashMap<String, Result<MsvcFound, String>>>> = OnceLock::new();
    let key = target.arch().to_string();
    let cache = CACHE.get_or_init(Default::default);
    if let Ok(guard) = cache.lock() {
        if let Some(hit) = guard.get(&key) {
            return hit.clone();
        }
    }
    let result = locate_msvc(target, VcArch::host(), &SystemHost);
    if let Ok(mut guard) = cache.lock() {
        guard.insert(key, result.clone());
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashSet;

    fn t(triple: &str) -> TargetTriple {
        TargetTriple::parse(triple)
    }

    // --- (a) triple -> vcvarsall argument ---

    #[test]
    fn triples_map_to_vc_architectures() {
        for (triple, arch) in [
            ("x86_64-pc-windows-msvc", VcArch::X64),
            ("i686-pc-windows-msvc", VcArch::X86),
            ("i586-pc-windows-msvc", VcArch::X86),
            ("aarch64-pc-windows-msvc", VcArch::Arm64),
            ("arm64ec-pc-windows-msvc", VcArch::Arm64),
        ] {
            assert_eq!(VcArch::from_triple(&t(triple)), Some(arch), "{triple}");
        }
        assert_eq!(VcArch::from_triple(&t("riscv64-unknown-linux-gnu")), None);
        assert_eq!(VcArch::from_triple(&t("armv7-pc-windows-msvc")), None);
    }

    #[test]
    fn native_pairs_use_the_bare_architecture() {
        assert_eq!(vcvarsall_arg(VcArch::X64, VcArch::X64), "x64");
        assert_eq!(vcvarsall_arg(VcArch::X86, VcArch::X86), "x86");
        assert_eq!(vcvarsall_arg(VcArch::Arm64, VcArch::Arm64), "arm64");
    }

    #[test]
    fn cross_pairs_use_host_underscore_target() {
        assert_eq!(vcvarsall_arg(VcArch::X64, VcArch::Arm64), "x64_arm64");
        assert_eq!(vcvarsall_arg(VcArch::X64, VcArch::X86), "x64_x86");
        assert_eq!(vcvarsall_arg(VcArch::X86, VcArch::X64), "x86_x64");
        assert_eq!(vcvarsall_arg(VcArch::Arm64, VcArch::X64), "arm64_x64");
        assert_eq!(vcvarsall_arg(VcArch::Arm64, VcArch::X86), "arm64_x86");
    }

    #[test]
    fn arm64_targets_require_the_arm64_tools_component() {
        assert_eq!(
            required_component(VcArch::X64),
            "Microsoft.VisualStudio.Component.VC.Tools.x86.x64"
        );
        assert_eq!(
            required_component(VcArch::X86),
            required_component(VcArch::X64)
        );
        assert_eq!(
            required_component(VcArch::Arm64),
            "Microsoft.VisualStudio.Component.VC.Tools.ARM64"
        );
    }

    #[test]
    fn vswhere_is_asked_for_the_latest_install_with_the_component() {
        let args = vswhere_args("X");
        assert_eq!(
            args,
            [
                "-latest",
                "-products",
                "*",
                "-requires",
                "X",
                "-property",
                "installationPath",
                "-utf8"
            ]
        );
    }

    // --- (b) vswhere output ---

    #[test]
    fn vswhere_output_is_the_first_nonempty_line() {
        let out = "C:\\Program Files\\Microsoft Visual Studio\\2022\\Community\r\n";
        assert_eq!(
            parse_vswhere_output(out),
            Some(PathBuf::from(
                "C:\\Program Files\\Microsoft Visual Studio\\2022\\Community"
            ))
        );
    }

    #[test]
    fn vswhere_output_tolerates_bom_blank_lines_and_a_label() {
        assert_eq!(
            parse_vswhere_output("\u{feff}\r\n\r\nC:\\VS\r\nC:\\Other\r\n"),
            Some(PathBuf::from("C:\\VS"))
        );
        assert_eq!(
            parse_vswhere_output("installationPath: C:\\VS\r\n"),
            Some(PathBuf::from("C:\\VS"))
        );
    }

    #[test]
    fn empty_vswhere_output_means_no_install() {
        assert_eq!(parse_vswhere_output(""), None);
        assert_eq!(parse_vswhere_output("\r\n  \r\n"), None);
        assert_eq!(parse_vswhere_output("installationPath:"), None);
    }

    #[test]
    fn vcvarsall_lives_under_vc_auxiliary_build() {
        let p = vcvarsall_path(Path::new("C:\\VS"));
        let s = p.to_string_lossy().replace('\\', "/");
        assert!(s.ends_with("VS/VC/Auxiliary/Build/vcvarsall.bat"), "{s}");
    }

    #[test]
    fn the_batch_script_calls_vcvarsall_then_dumps_the_environment() {
        let s = vcvars_script(
            Path::new("C:\\Program Files\\VS\\vcvarsall.bat"),
            "x64_arm64",
        );
        assert!(s.contains("call \"C:\\Program Files\\VS\\vcvarsall.bat\" x64_arm64 >nul 2>&1"));
        assert!(s.contains("if errorlevel 1 exit /b 1"));
        assert!(s.trim_end().ends_with("set"));
        // call precedes set, so a failing vcvarsall never reaches `set`.
        assert!(s.find("call").unwrap() < s.rfind("set").unwrap());
    }

    // --- (c) `set` output ---

    #[test]
    fn set_output_keeps_equals_signs_in_values() {
        let vars = parse_set_output("A=b=c\r\nINCLUDE=C:\\x;C:\\y\r\nEMPTY=\r\n");
        assert_eq!(vars["A"], "b=c");
        assert_eq!(vars["INCLUDE"], "C:\\x;C:\\y");
        assert_eq!(vars["EMPTY"], "");
    }

    #[test]
    fn set_output_strips_crlf_and_accepts_lf() {
        let vars = parse_set_output("X=1\r\nY=2\nZ=3");
        assert_eq!(vars["X"], "1");
        assert_eq!(vars["Y"], "2");
        assert_eq!(vars["Z"], "3");
    }

    #[test]
    fn set_output_names_are_case_insensitive() {
        let vars = parse_set_output("Path=C:\\a;C:\\b\r\nUniversalCRTSdkDir=C:\\sdk\r\n");
        assert_eq!(vars["PATH"], "C:\\a;C:\\b");
        assert_eq!(vars["UNIVERSALCRTSDKDIR"], "C:\\sdk");
        assert!(!vars.contains_key("Path"));
    }

    #[test]
    fn set_output_skips_hidden_drive_variables_and_junk() {
        let vars =
            parse_set_output("=C:=C:\\work\r\n=ExitCode=00000000\r\nno equals here\r\nOK=1\r\n");
        assert_eq!(vars.len(), 1);
        assert_eq!(vars["OK"], "1");
    }

    #[test]
    fn a_later_duplicate_wins() {
        let vars = parse_set_output("Path=a\r\nPATH=b\r\n");
        assert_eq!(vars["PATH"], "b");
    }

    // --- (d) choosing tools ---

    fn env_with_path(path: &str) -> BTreeMap<String, String> {
        parse_set_output(&format!("Path={path}\r\n"))
    }

    fn present(set: &[&str]) -> impl Fn(&Path) -> bool {
        let set: HashSet<String> = set.iter().map(|s| s.replace('\\', "/")).collect();
        move |p: &Path| set.contains(&p.to_string_lossy().replace('\\', "/"))
    }

    #[test]
    fn tools_come_from_the_captured_path_in_order() {
        let env = env_with_path("C:\\msvc\\bin;C:\\git\\usr\\bin");
        // GNU coreutils' link is later on the PATH than MSVC's and must not win.
        let exists = present(&[
            "C:\\msvc\\bin/cl.exe",
            "C:\\msvc\\bin/lib.exe",
            "C:\\msvc\\bin/link.exe",
            "C:\\git\\usr\\bin/link.exe",
        ]);
        let tools = choose_tools(&env, &exists).unwrap();
        assert!(tools.cl.to_string_lossy().starts_with("C:\\msvc\\bin"));
        assert!(tools.link.to_string_lossy().starts_with("C:\\msvc\\bin"));
    }

    #[test]
    fn the_first_path_entry_holding_a_tool_wins() {
        let env = env_with_path("C:\\git;C:\\msvc");
        let exists = present(&[
            "C:\\git/link.exe",
            "C:\\msvc/cl.exe",
            "C:\\msvc/lib.exe",
            "C:\\msvc/link.exe",
        ]);
        let tools = choose_tools(&env, &exists).unwrap();
        // First hit in PATH order, which is vcvarsall's job to arrange.
        assert!(tools.link.to_string_lossy().starts_with("C:\\git"));
        assert!(tools.cl.to_string_lossy().starts_with("C:\\msvc"));
    }

    #[test]
    fn missing_tools_are_named() {
        let env = env_with_path("C:\\msvc");
        let err = choose_tools(&env, &present(&["C:\\msvc/cl.exe"])).unwrap_err();
        assert!(err.contains("lib.exe") && err.contains("link.exe"), "{err}");
        assert!(!err.contains("cl.exe,"), "{err}");
    }

    #[test]
    fn an_environment_without_path_is_an_error() {
        let err = choose_tools(&BTreeMap::new(), &|_| true).unwrap_err();
        assert!(err.contains("no PATH"), "{err}");
    }

    #[test]
    fn empty_path_entries_are_ignored() {
        let found = find_in_path(";;C:\\a;", "cl.exe", &present(&["C:\\a/cl.exe"]));
        assert!(found.is_some());
        assert_eq!(find_in_path("", "cl.exe", &|_| true), None);
    }

    #[test]
    fn only_changed_variables_are_passed_on() {
        let captured = parse_set_output("Path=new\r\nINCLUDE=inc\r\nSAME=1\r\nHOME=x\r\n");
        let current = vec![
            ("Path".to_string(), "old".to_string()),
            ("SAME".to_string(), "1".to_string()),
            ("home".to_string(), "x".to_string()),
        ];
        let delta = env_delta(&captured, &current);
        let names: Vec<&str> = delta.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(names, ["INCLUDE", "PATH"]);
    }

    // --- fingerprint identity ---

    fn v(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn identity_digest_tracks_the_toolset_and_sdk() {
        let a = identity_digest(&v(&[("INCLUDE", "C:\\VC\\14.40\\include")])).unwrap();
        let b = identity_digest(&v(&[("INCLUDE", "C:\\VC\\14.41\\include")])).unwrap();
        assert_ne!(a, b);
        let same = identity_digest(&v(&[("Include", "C:\\VC\\14.40\\include")])).unwrap();
        assert_eq!(a, same, "names are case-insensitive");
    }

    #[test]
    fn identity_digest_ignores_unrelated_variables_and_path() {
        let base = identity_digest(&v(&[("INCLUDE", "i"), ("PATH", "p1")])).unwrap();
        let other =
            identity_digest(&v(&[("INCLUDE", "i"), ("PATH", "p2"), ("TEMP", "t")])).unwrap();
        assert_eq!(base, other);
    }

    #[test]
    fn identity_digest_is_none_without_identity_variables() {
        assert_eq!(identity_digest(&[]), None);
        assert_eq!(identity_digest(&v(&[("PATH", "p")])), None);
    }

    #[test]
    fn cl_banner_version_is_parsed() {
        let banner = "Microsoft (R) C/C++ Optimizing Compiler Version 19.40.33811 for x64\r\n\
                      Copyright (C) Microsoft Corporation.  All rights reserved.\r\n";
        assert_eq!(
            parse_cl_banner_version(banner).as_deref(),
            Some("19.40.33811")
        );
        assert_eq!(parse_cl_banner_version("gcc (GCC) 13.2.0"), None);
        assert_eq!(parse_cl_banner_version("Compiler Version abc"), None);
    }

    // --- orchestration against a fake machine ---

    #[derive(Default)]
    struct FakeHost {
        windows: bool,
        pf86: Option<PathBuf>,
        path_programs: HashMap<String, PathBuf>,
        files: HashSet<String>,
        env: HashMap<String, String>,
        vswhere_out: Option<Result<String, String>>,
        batch_out: Option<Result<String, String>>,
        batches_run: RefCell<Vec<String>>,
        vswhere_runs: RefCell<Vec<Vec<String>>>,
    }

    fn norm(p: &Path) -> String {
        p.to_string_lossy().replace('\\', "/")
    }

    impl MsvcHost for FakeHost {
        fn is_windows(&self) -> bool {
            self.windows
        }
        fn program_files_x86(&self) -> Option<PathBuf> {
            self.pf86.clone()
        }
        fn path_lookup(&self, name: &str) -> Option<PathBuf> {
            self.path_programs.get(name).cloned()
        }
        fn file_exists(&self, path: &Path) -> bool {
            self.files.contains(&norm(path))
        }
        fn env_var(&self, name: &str) -> Option<String> {
            self.env.get(name).cloned()
        }
        fn current_env(&self) -> Vec<(String, String)> {
            self.env
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        }
        fn run_vswhere(&self, _: &Path, args: &[String]) -> Result<String, String> {
            self.vswhere_runs.borrow_mut().push(args.to_vec());
            self.vswhere_out
                .clone()
                .unwrap_or_else(|| Err("not stubbed".into()))
        }
        fn run_batch(&self, script: &str) -> Result<String, String> {
            self.batches_run.borrow_mut().push(script.to_string());
            self.batch_out
                .clone()
                .unwrap_or_else(|| Err("not stubbed".into()))
        }
    }

    const VS: &str = "C:\\VS\\2022\\Community";
    const VSWHERE: &str = "C:\\PF86/Microsoft Visual Studio/Installer/vswhere.exe";

    /// A machine with VS installed whose vcvarsall yields working tools.
    fn vs_host() -> FakeHost {
        let mut h = FakeHost {
            windows: true,
            pf86: Some(PathBuf::from("C:\\PF86")),
            vswhere_out: Some(Ok(format!("{VS}\r\n"))),
            batch_out: Some(Ok(
                "Path=C:\\VS\\bin;C:\\Windows\r\nINCLUDE=C:\\VS\\inc\r\nLIB=C:\\VS\\lib\r\nTEMP=t\r\n"
                    .to_string(),
            )),
            ..Default::default()
        };
        h.env.insert("TEMP".into(), "t".into());
        h.env.insert("PATH".into(), "C:\\Windows".into());
        for f in [
            VSWHERE,
            "C:\\VS\\2022\\Community/VC/Auxiliary/Build/vcvarsall.bat",
            "C:\\VS\\bin/cl.exe",
            "C:\\VS\\bin/lib.exe",
            "C:\\VS\\bin/link.exe",
        ] {
            h.files.insert(f.replace('\\', "/"));
        }
        h
    }

    #[test]
    fn vswhere_discovery_captures_the_environment() {
        let host = vs_host();
        let found = discover_via_vswhere(VcArch::X64, VcArch::X64, &host).unwrap();
        match &found.source {
            MsvcSource::Vswhere {
                install_path,
                vcvars_arg,
                ..
            } => {
                assert_eq!(install_path, Path::new(VS));
                assert_eq!(vcvars_arg, "x64");
            }
            other => panic!("{other:?}"),
        }
        assert!(norm(&found.tools.cl).ends_with("VS/bin/cl.exe"));
        // TEMP is identical to the process's, so it is not forwarded.
        let names: Vec<&str> = found.env.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(names, ["INCLUDE", "LIB", "PATH"]);
        assert!(found.found_via().contains("vswhere"));
        assert!(found.found_via().contains(VS));
        // vswhere was asked for the right component, and vcvarsall run once.
        assert!(host.vswhere_runs.borrow()[0].contains(&COMPONENT_X86_X64.to_string()));
        assert_eq!(host.batches_run.borrow().len(), 1);
        assert!(host.batches_run.borrow()[0].contains("vcvarsall.bat\" x64 "));
    }

    #[test]
    fn a_cross_target_runs_the_cross_vcvarsall_pair_and_needs_arm64_tools() {
        let host = vs_host();
        discover_via_vswhere(VcArch::X64, VcArch::Arm64, &host).unwrap();
        assert!(host.vswhere_runs.borrow()[0].contains(&COMPONENT_ARM64.to_string()));
        assert!(host.batches_run.borrow()[0].contains("vcvarsall.bat\" x64_arm64 "));
    }

    #[test]
    fn not_on_windows_is_a_precise_reason() {
        let mut host = vs_host();
        host.windows = false;
        let err = discover_via_vswhere(VcArch::X64, VcArch::X64, &host).unwrap_err();
        assert_eq!(err, "vswhere is only available on Windows");
        assert!(host.vswhere_runs.borrow().is_empty());
    }

    #[test]
    fn a_missing_vswhere_names_where_it_looked() {
        let mut host = vs_host();
        host.files.remove(&VSWHERE.replace('\\', "/"));
        let err = discover_via_vswhere(VcArch::X64, VcArch::X64, &host).unwrap_err();
        assert!(err.contains("vswhere.exe not found"), "{err}");
        assert!(err.contains("PF86"), "{err}");
    }

    #[test]
    fn vswhere_falls_back_to_path() {
        let mut host = vs_host();
        host.files.remove(&VSWHERE.replace('\\', "/"));
        host.path_programs
            .insert("vswhere".into(), PathBuf::from("C:\\tools\\vswhere.exe"));
        assert!(discover_via_vswhere(VcArch::X64, VcArch::X64, &host).is_ok());
    }

    #[test]
    fn no_visual_studio_install_is_reported() {
        let mut host = vs_host();
        host.vswhere_out = Some(Ok("\r\n".to_string()));
        let err = discover_via_vswhere(VcArch::X64, VcArch::X64, &host).unwrap_err();
        assert!(err.contains("no Visual Studio install"), "{err}");
        assert!(err.contains(COMPONENT_X86_X64), "{err}");
    }

    #[test]
    fn a_failing_vswhere_is_reported() {
        let mut host = vs_host();
        host.vswhere_out = Some(Err("exited with 1".to_string()));
        let err = discover_via_vswhere(VcArch::X64, VcArch::X64, &host).unwrap_err();
        assert_eq!(err, "exited with 1");
    }

    #[test]
    fn a_missing_vcvarsall_is_reported() {
        let mut host = vs_host();
        host.files
            .remove("C:/VS/2022/Community/VC/Auxiliary/Build/vcvarsall.bat");
        let err = discover_via_vswhere(VcArch::X64, VcArch::X64, &host).unwrap_err();
        assert!(err.contains("vcvarsall.bat does not exist"), "{err}");
    }

    #[test]
    fn a_failing_vcvarsall_names_the_architecture() {
        let mut host = vs_host();
        host.batch_out = Some(Err("exited with exit code: 1".to_string()));
        let err = discover_via_vswhere(VcArch::X64, VcArch::Arm64, &host).unwrap_err();
        assert!(err.contains("vcvarsall.bat x64_arm64 failed"), "{err}");
    }

    #[test]
    fn a_vcvarsall_without_tools_is_reported() {
        let mut host = vs_host();
        host.files.remove("C:/VS/bin/link.exe");
        let err = discover_via_vswhere(VcArch::X64, VcArch::X64, &host).unwrap_err();
        assert!(
            err.contains("link.exe not found on the resulting PATH"),
            "{err}"
        );
    }

    fn configured_host() -> FakeHost {
        let mut host = vs_host();
        for (n, p) in [
            ("cl", "C:\\dev\\cl.exe"),
            ("lib", "C:\\dev\\lib.exe"),
            ("link", "C:\\dev\\link.exe"),
        ] {
            host.path_programs.insert(n.into(), PathBuf::from(p));
        }
        host.env.insert("INCLUDE".into(), "i".into());
        host.env.insert("LIB".into(), "l".into());
        host
    }

    #[test]
    fn a_configured_environment_wins_over_vswhere() {
        let host = configured_host();
        let found = locate_msvc(&t("x86_64-pc-windows-msvc"), Some(VcArch::X64), &host).unwrap();
        assert_eq!(found.source, MsvcSource::Environment);
        assert!(found.env.is_empty());
        assert_eq!(found.tools.cl, PathBuf::from("C:\\dev\\cl.exe"));
        assert!(host.vswhere_runs.borrow().is_empty());
        assert!(found.found_via().contains("PATH"));
    }

    #[test]
    fn cl_on_path_without_include_and_lib_falls_through_to_vswhere() {
        let mut host = configured_host();
        host.env.remove("INCLUDE");
        let found = locate_msvc(&t("x86_64-pc-windows-msvc"), Some(VcArch::X64), &host).unwrap();
        assert!(matches!(found.source, MsvcSource::Vswhere { .. }));
    }

    #[test]
    fn a_configured_environment_is_not_reused_for_another_architecture() {
        let mut host = configured_host();
        // The environment says it builds for x64; the target is arm64.
        host.env.insert("VSCMD_ARG_TGT_ARCH".into(), "x64".into());
        let found = locate_msvc(&t("aarch64-pc-windows-msvc"), Some(VcArch::X64), &host).unwrap();
        assert!(matches!(found.source, MsvcSource::Vswhere { .. }));
        assert!(host.batches_run.borrow()[0].contains("x64_arm64"));
    }

    #[test]
    fn a_configured_environment_for_the_right_cross_architecture_is_used() {
        let mut host = configured_host();
        host.env.insert("VSCMD_ARG_TGT_ARCH".into(), "arm64".into());
        let found = locate_msvc(&t("aarch64-pc-windows-msvc"), Some(VcArch::X64), &host).unwrap();
        assert_eq!(found.source, MsvcSource::Environment);
    }

    #[test]
    fn the_configured_environment_is_not_trusted_for_a_cross_target_without_evidence() {
        let host = configured_host();
        let found = locate_msvc(&t("aarch64-pc-windows-msvc"), Some(VcArch::X64), &host).unwrap();
        assert!(matches!(found.source, MsvcSource::Vswhere { .. }));
    }

    #[test]
    fn both_reasons_are_reported_when_nothing_is_found() {
        let host = FakeHost::default();
        let err = locate_msvc(&t("x86_64-pc-windows-msvc"), Some(VcArch::X64), &host).unwrap_err();
        assert!(err.contains("cl.exe is not on PATH"), "{err}");
        assert!(
            err.contains("vswhere: vswhere is only available on Windows"),
            "{err}"
        );
    }

    #[test]
    fn an_architecture_msvc_cannot_target_is_refused() {
        let host = vs_host();
        let err = locate_msvc(&t("riscv64-pc-windows-msvc"), Some(VcArch::X64), &host).unwrap_err();
        assert!(err.contains("no toolset for architecture"), "{err}");
    }

    #[test]
    fn the_system_host_refuses_off_windows() {
        if cfg!(windows) {
            return;
        }
        let err = locate_msvc(
            &t("aarch64-pc-windows-msvc"),
            Some(VcArch::X64),
            &SystemHost,
        )
        .unwrap_err();
        assert!(err.contains("only available on Windows"), "{err}");
    }
}
