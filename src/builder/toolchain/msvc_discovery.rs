//! Finding MSVC without a Developer Command Prompt.
//!
//! `cl.exe` is useless on its own: it finds the C library headers and import
//! libraries through `INCLUDE`, `LIB` and `LIBPATH`, and its own DLLs through
//! `PATH`. A plain shell has none of that and no `cl` on `PATH`, so discovery
//! *computes* that environment: ask `vswhere.exe` where Visual Studio is
//! installed, read the install's default toolset version, find the newest
//! Windows SDK that has the C runtime headers (registry, else the default
//! install directory), and lay out `INCLUDE`, `LIB` and `PATH` from the
//! directories those have. No batch file runs and no other process is spawned
//! besides `vswhere`, so it takes milliseconds and does not depend on
//! `System32` being on `PATH` (which `vcvarsall.bat` does, to find the SDK
//! with `reg.exe` and `findstr.exe`). The environment is given to every `cl`,
//! `link` and `lib` invocation (see [`EnvWrapper`](super::EnvWrapper)).
//!
//! Order (see [`locate_msvc`]): an explicit `cc` override is handled before
//! this module is reached; then a `cl.exe` already on `PATH` whose
//! environment is already set up (a Developer Command Prompt, or CI that ran
//! `vcvarsall`) wins, because the user arranged it; `vswhere` is the fallback.
//!
//! Everything that decides something is a pure function over strings and maps
//! (architecture mapping, parsing `vswhere` output, version ordering, laying
//! out the environment), unit-tested on any OS. Everything that touches the
//! machine (processes, files, the registry) sits behind [`MsvcHost`].
//! [`SystemHost`] is the only real implementation and refuses to do anything
//! off Windows; the orchestration is tested with a fake one. Only
//! [`SystemHost`]'s registry access is behind `#[cfg(windows)]`, so the rest is
//! type-checked on every platform.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use crate::core::target::TargetTriple;
use crate::util::hash::Fingerprint;

/// The vswhere component that provides the x86/x64 compilers.
const COMPONENT_X86_X64: &str = "Microsoft.VisualStudio.Component.VC.Tools.x86.x64";
/// The vswhere component that provides the ARM64 compilers.
const COMPONENT_ARM64: &str = "Microsoft.VisualStudio.Component.VC.Tools.ARM64";

/// The registry key (under `HKLM`) that records where the Windows 10/11 SDK is.
const SDK_ROOTS_KEY: &str = r"SOFTWARE\Microsoft\Windows Kits\Installed Roots";
/// The same key as 32-bit software sees it, which is where installers that
/// write through WOW64 leave it.
const SDK_ROOTS_KEY_WOW64: &str = r"SOFTWARE\WOW6432Node\Microsoft\Windows Kits\Installed Roots";
/// The value of [`SDK_ROOTS_KEY`] holding the SDK root.
const SDK_ROOT_VALUE: &str = "KitsRoot10";

/// An architecture MSVC has a toolset for.
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

    /// The spelling the MSVC and Windows SDK directory layouts use
    /// (`bin\Hostx64\arm64`, `Lib\<v>\um\arm64`), and `VSCMD_ARG_TGT_ARCH`.
    pub fn name(self) -> &'static str {
        match self {
            VcArch::X86 => "x86",
            VcArch::X64 => "x64",
            VcArch::Arm64 => "arm64",
        }
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

/// `<install>\VC\Auxiliary\Build\Microsoft.VCToolsVersion.default.txt`, which
/// names the toolset version Visual Studio uses when a project does not ask
/// for one.
pub fn toolset_version_file(install: &Path) -> PathBuf {
    install
        .join("VC")
        .join("Auxiliary")
        .join("Build")
        .join("Microsoft.VCToolsVersion.default.txt")
}

/// The toolset version in the contents of [`toolset_version_file`]: the first
/// non-blank line, trimmed (the file ends in a newline, and has a byte-order
/// mark on some installs). `None` when it holds nothing.
pub fn parse_toolset_version(text: &str) -> Option<String> {
    text.trim_start_matches('\u{feff}')
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
}

/// A dotted numeric version (`10.0.22000.0`) as numbers, so that `10.0.9`
/// sorts before `10.0.10`. `None` for anything with a non-numeric part, which
/// is how directories that are not SDK versions (`wdf`, `NETFXSDK`) drop out.
pub fn parse_numeric_version(name: &str) -> Option<Vec<u64>> {
    if name.is_empty() {
        return None;
    }
    name.split('.').map(|p| p.parse::<u64>().ok()).collect()
}

/// The highest of the version directories `names` for which `usable` holds,
/// ordered numerically.
pub fn pick_sdk_version(names: &[String], usable: &dyn Fn(&str) -> bool) -> Option<String> {
    names
        .iter()
        .filter_map(|n| parse_numeric_version(n).map(|v| (v, n)))
        .filter(|(_, n)| usable(n))
        .max_by(|(a, _), (b, _)| a.cmp(b))
        .map(|(_, n)| n.clone())
}

/// The first directory of the `;`-separated `path` that holds `exe`
/// (`stdio.h`, ...), judged by `exists`.
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

/// Locate `cl`, `lib` and `link` in `bin`, the toolset's directory for the
/// host/target pair. The error names every tool that is missing.
pub fn tools_in(bin: &Path, exists: &dyn Fn(&Path) -> bool) -> Result<MsvcTools, String> {
    let (cl, lib, link) = (
        bin.join("cl.exe"),
        bin.join("lib.exe"),
        bin.join("link.exe"),
    );
    let missing: Vec<&str> = [(&cl, "cl.exe"), (&lib, "lib.exe"), (&link, "link.exe")]
        .iter()
        .filter(|(p, _)| !exists(p))
        .map(|(_, n)| *n)
        .collect();
    if missing.is_empty() {
        Ok(MsvcTools { cl, lib, link })
    } else {
        Err(format!(
            "{} not found in {} (is the toolset installed for this host/target pair?)",
            missing.join(", "),
            bin.display()
        ))
    }
}

/// Check that `include` (an `INCLUDE` value) reaches the C runtime headers.
///
/// An environment can have `cl.exe` on PATH yet an `INCLUDE` without the
/// Windows SDK's `ucrt` directory (a Developer Command Prompt set up before
/// the SDK was installed, or by a `vcvarsall.bat` that could not find it). Such
/// an environment passes every other check and then fails each compile with
/// "Cannot open include file: 'stdio.h'", so it is refused here instead.
pub fn check_headers(include: Option<&str>, exists: &dyn Fn(&Path) -> bool) -> Result<(), String> {
    if find_in_path(include.unwrap_or(""), "stdio.h", exists).is_some() {
        return Ok(());
    }
    Err(
        "INCLUDE has no directory with stdio.h (the Windows SDK's ucrt headers); \
         the Windows SDK is not installed, or the environment was set up without finding it"
            .to_string(),
    )
}

/// The variables that decide *which* MSVC and Windows SDK a compile uses.
/// A change in one of these is a different toolchain even if `cl.exe` sits at
/// the same path (an in-place VS update keeps the path and changes the toolset
/// directory inside `INCLUDE`). The target architecture is in them too: `LIB`
/// has `...\x64` or `...\arm64` directories, and `VCToolsVersion` and
/// `WindowsSDKVersion` pin the versions explicitly.
const IDENTITY_VARS: &[&str] = &[
    "INCLUDE",
    "LIB",
    "LIBPATH",
    "VCTOOLSVERSION",
    "VCINSTALLDIR",
    "WINDOWSSDKVERSION",
    "WINDOWSSDKDIR",
    "UCRTVERSION",
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
    /// Located with `vswhere.exe`; the environment is computed from the
    /// toolset and Windows SDK directories.
    Vswhere {
        vswhere: PathBuf,
        install_path: PathBuf,
        /// The MSVC toolset (`...\VC\Tools\MSVC\14.35.32215`) and its version.
        toolset_dir: PathBuf,
        toolset_version: String,
        /// The Windows SDK root (`...\Windows Kits\10`) and the version used.
        sdk_root: PathBuf,
        sdk_version: String,
        host_arch: VcArch,
        target_arch: VcArch,
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
                toolset_version,
                sdk_version,
                host_arch,
                target_arch,
                ..
            } => format!(
                "found via vswhere: {} (MSVC {toolset_version}, Windows SDK {sdk_version}, {}-hosted {})",
                install_path.display(),
                host_arch.name(),
                target_arch.name()
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
    fn dir_exists(&self, path: &Path) -> bool;
    /// The contents of a text file, or why it could not be read.
    fn read_file(&self, path: &Path) -> Result<String, String>;
    /// The names of the entries of a directory, or why it could not be listed.
    fn list_dir(&self, path: &Path) -> Result<Vec<String>, String>;
    /// A string value of `HKEY_LOCAL_MACHINE\<key>`, if present.
    fn registry_string(&self, key: &str, value: &str) -> Option<String>;
    /// A variable of the process environment.
    fn env_var(&self, name: &str) -> Option<String>;
    /// Run `vswhere`, returning stdout, or why it could not be run.
    fn run_vswhere(&self, vswhere: &Path, args: &[String]) -> Result<String, String>;
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

    fn dir_exists(&self, path: &Path) -> bool {
        path.is_dir()
    }

    fn read_file(&self, path: &Path) -> Result<String, String> {
        std::fs::read_to_string(path).map_err(|e| e.to_string())
    }

    fn list_dir(&self, path: &Path) -> Result<Vec<String>, String> {
        let entries = std::fs::read_dir(path).map_err(|e| e.to_string())?;
        Ok(entries
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect())
    }

    #[cfg(windows)]
    fn registry_string(&self, key: &str, value: &str) -> Option<String> {
        windows_registry::LOCAL_MACHINE
            .open(key)
            .ok()?
            .get_string(value)
            .ok()
            .filter(|v| !v.is_empty())
    }

    #[cfg(not(windows))]
    fn registry_string(&self, _key: &str, _value: &str) -> Option<String> {
        None
    }

    fn env_var(&self, name: &str) -> Option<String> {
        std::env::var(name).ok().filter(|v| !v.is_empty())
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

/// The Windows SDK: its root and the highest version that has the C runtime
/// headers.
///
/// Roots, in order: the registry's `KitsRoot10` (64-bit view, then WOW64),
/// then `%ProgramFiles(x86)%\Windows Kits\10`. The first root with a usable
/// version wins.
fn find_sdk(host: &dyn MsvcHost) -> Result<(PathBuf, String), String> {
    let mut roots: Vec<PathBuf> = Vec::new();
    for key in [SDK_ROOTS_KEY, SDK_ROOTS_KEY_WOW64] {
        if let Some(root) = host.registry_string(key, SDK_ROOT_VALUE) {
            roots.push(PathBuf::from(root));
        }
    }
    if let Some(pf) = host.program_files_x86() {
        roots.push(pf.join("Windows Kits").join("10"));
    }
    let mut unique: Vec<PathBuf> = Vec::new();
    for root in roots {
        // Registry values end in a backslash; compare without it.
        let key = root
            .to_string_lossy()
            .trim_end_matches(['\\', '/'])
            .to_lowercase();
        if !unique.iter().any(|u| {
            u.to_string_lossy()
                .trim_end_matches(['\\', '/'])
                .to_lowercase()
                == key
        }) {
            unique.push(root);
        }
    }
    if unique.is_empty() {
        return Err(
            "no Windows SDK location: no KitsRoot10 registry value and no \
                    ProgramFiles(x86)"
                .to_string(),
        );
    }
    for root in &unique {
        let include = root.join("Include");
        let Ok(names) = host.list_dir(&include) else {
            continue;
        };
        let usable = |v: &str| host.file_exists(&include.join(v).join("ucrt").join("stdio.h"));
        if let Some(version) = pick_sdk_version(&names, &usable) {
            return Ok((root.clone(), version));
        }
    }
    Err(format!(
        "no Windows SDK with ucrt headers under {}",
        unique
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// Join directories into a `;`-separated list.
fn join_paths(dirs: &[PathBuf]) -> String {
    dirs.iter()
        .map(|d| d.display().to_string())
        .collect::<Vec<_>>()
        .join(";")
}

/// The environment `cl`, `link` and `lib` need, laid out from a toolset and an
/// SDK, as `vcvarsall.bat` would have produced it for the pair.
///
/// `INCLUDE`/`LIB`/`LIBPATH` are only the directories that exist. `PATH` is
/// the new directories followed by `current_path`: the toolset's directory for
/// the target (the tools), the toolset's host-native directory when
/// cross-compiling (the compiler's DLLs live there), and the SDK's `bin` for
/// the host (`rc.exe`, `mt.exe`).
#[allow(clippy::too_many_arguments)]
fn build_environment(
    host: &dyn MsvcHost,
    toolset_dir: &Path,
    toolset_version: &str,
    install: &Path,
    sdk_root: &Path,
    sdk_version: &str,
    host_arch: VcArch,
    target_arch: VcArch,
    current_path: &str,
) -> Vec<(String, String)> {
    let (h, t) = (host_arch.name(), target_arch.name());
    let keep = |dirs: Vec<PathBuf>| -> Vec<PathBuf> {
        dirs.into_iter().filter(|d| host.dir_exists(d)).collect()
    };

    let sdk_include = sdk_root.join("Include").join(sdk_version);
    let sdk_lib = sdk_root.join("Lib").join(sdk_version);
    let atlmfc = toolset_dir.join("atlmfc");

    let mut include = vec![toolset_dir.join("include"), atlmfc.join("include")];
    include.extend(
        ["ucrt", "um", "shared", "winrt", "cppwinrt"]
            .iter()
            .map(|d| sdk_include.join(d)),
    );
    let mut lib = vec![toolset_dir.join("lib").join(t), atlmfc.join("lib").join(t)];
    lib.extend(["ucrt", "um"].iter().map(|d| sdk_lib.join(d).join(t)));
    let libpath = vec![atlmfc.join("lib").join(t), toolset_dir.join("lib").join(t)];

    let host_dir = format!("Host{h}");
    let mut path = vec![toolset_dir.join("bin").join(&host_dir).join(t)];
    if host_arch != target_arch {
        path.push(toolset_dir.join("bin").join(&host_dir).join(h));
    }
    path.push(sdk_root.join("bin").join(sdk_version).join(h));
    let path = keep(path);

    let mut path_value = join_paths(&path);
    if !current_path.is_empty() {
        path_value.push(';');
        path_value.push_str(current_path);
    }

    let vc_install = toolset_dir
        .ancestors()
        .nth(3)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| install.join("VC"));
    let with_slash = |p: &Path| format!("{}\\", p.display());

    vec![
        ("INCLUDE".to_string(), join_paths(&keep(include))),
        ("LIB".to_string(), join_paths(&keep(lib))),
        ("LIBPATH".to_string(), join_paths(&keep(libpath))),
        ("PATH".to_string(), path_value),
        ("UCRTVersion".to_string(), sdk_version.to_string()),
        ("VCINSTALLDIR".to_string(), with_slash(&vc_install)),
        ("VCToolsInstallDir".to_string(), with_slash(toolset_dir)),
        ("VCToolsVersion".to_string(), toolset_version.to_string()),
        ("WindowsSdkDir".to_string(), with_slash(sdk_root)),
        ("WindowsSDKVersion".to_string(), format!("{sdk_version}\\")),
    ]
}

/// Discover MSVC through `vswhere`, the toolset directory and the Windows SDK,
/// for `target_arch` built on `host_arch`.
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
            target_arch.name()
        )
    })?;

    let version_file = toolset_version_file(&install_path);
    if !host.file_exists(&version_file) {
        return Err(format!(
            "toolset version file {} missing",
            version_file.display()
        ));
    }
    let text = host.read_file(&version_file).map_err(|e| {
        format!(
            "could not read toolset version file {}: {e}",
            version_file.display()
        )
    })?;
    let toolset_version = parse_toolset_version(&text)
        .ok_or_else(|| format!("toolset version file {} is empty", version_file.display()))?;
    let toolset_dir = install_path
        .join("VC")
        .join("Tools")
        .join("MSVC")
        .join(&toolset_version);
    if !host.dir_exists(&toolset_dir) {
        return Err(format!(
            "MSVC toolset directory {} missing (toolset {toolset_version})",
            toolset_dir.display()
        ));
    }
    let bin = toolset_dir
        .join("bin")
        .join(format!("Host{}", host_arch.name()))
        .join(target_arch.name());
    let tools = tools_in(&bin, &|p| host.file_exists(p))?;

    let (sdk_root, sdk_version) = find_sdk(host)?;

    let env = build_environment(
        host,
        &toolset_dir,
        &toolset_version,
        &install_path,
        &sdk_root,
        &sdk_version,
        host_arch,
        target_arch,
        &host.env_var("PATH").unwrap_or_default(),
    );
    let include = env.iter().find(|(k, _)| k == "INCLUDE").map(|(_, v)| v);
    check_headers(include.map(String::as_str), &|p| host.file_exists(p))?;

    Ok(MsvcFound {
        source: MsvcSource::Vswhere {
            vswhere,
            install_path,
            toolset_dir,
            toolset_version,
            sdk_root,
            sdk_version,
            host_arch,
            target_arch,
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
    check_headers(host.env_var("INCLUDE").as_deref(), &|p| host.file_exists(p))
        .map_err(|e| format!("{} is on PATH but {e}", cl.display()))?;
    if !native {
        match host.env_var("VSCMD_ARG_TGT_ARCH") {
            Some(a) if a.eq_ignore_ascii_case(target_arch.name()) => {}
            Some(a) => {
                return Err(format!(
                    "the cl.exe on PATH targets {a}, not {}",
                    target_arch.name()
                ))
            }
            None => {
                return Err(format!(
                    "cannot tell which architecture the cl.exe on PATH targets \
                     (VSCMD_ARG_TGT_ARCH unset), wanted {}",
                    target_arch.name()
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
/// A build context asks several times (the target toolchain, the host
/// toolchain for generators, `toolchain show`), and each answer runs
/// `vswhere.exe`, so the answer is remembered.
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

    // --- (a) architectures and vswhere ---

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
    fn architectures_are_spelled_as_the_msvc_directories_are() {
        assert_eq!(VcArch::X86.name(), "x86");
        assert_eq!(VcArch::X64.name(), "x64");
        assert_eq!(VcArch::Arm64.name(), "arm64");
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

    // --- (c) toolset and SDK versions ---

    #[test]
    fn the_toolset_version_file_lives_under_vc_auxiliary_build() {
        let p = toolset_version_file(Path::new("C:\\VS"));
        let s = p.to_string_lossy().replace('\\', "/");
        assert!(
            s.ends_with("VS/VC/Auxiliary/Build/Microsoft.VCToolsVersion.default.txt"),
            "{s}"
        );
    }

    #[test]
    fn the_toolset_version_is_trimmed() {
        assert_eq!(
            parse_toolset_version("14.35.32215\r\n").as_deref(),
            Some("14.35.32215")
        );
        assert_eq!(
            parse_toolset_version("\u{feff}\r\n  14.40.33807  \n").as_deref(),
            Some("14.40.33807")
        );
        assert_eq!(parse_toolset_version(" \r\n"), None);
    }

    #[test]
    fn versions_compare_numerically_not_as_strings() {
        assert!(
            parse_numeric_version("10.0.9.0").unwrap()
                < parse_numeric_version("10.0.10.0").unwrap()
        );
        assert_eq!(parse_numeric_version("wdf"), None);
        assert_eq!(parse_numeric_version("10.0.x"), None);
        assert_eq!(parse_numeric_version(""), None);
    }

    #[test]
    fn the_highest_usable_sdk_version_wins() {
        let names: Vec<String> = [
            "10.0.9.0",
            "10.0.10.0",
            "wdf",
            "10.0.22000.0",
            "10.0.19041.0",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            pick_sdk_version(&names, &|_| true).as_deref(),
            Some("10.0.22000.0")
        );
        // The newest one is skipped when it has no ucrt headers.
        assert_eq!(
            pick_sdk_version(&names, &|v| v != "10.0.22000.0").as_deref(),
            Some("10.0.19041.0")
        );
        // Numeric, not lexical: 10.0.10.0 beats 10.0.9.0.
        assert_eq!(
            pick_sdk_version(&names, &|v| v == "10.0.9.0" || v == "10.0.10.0").as_deref(),
            Some("10.0.10.0")
        );
        assert_eq!(pick_sdk_version(&names, &|_| false), None);
    }

    // --- (d) tools, headers, identity ---

    fn present(set: &[&str]) -> impl Fn(&Path) -> bool {
        let set: HashSet<String> = set.iter().map(|s| s.replace('\\', "/")).collect();
        move |p: &Path| set.contains(&p.to_string_lossy().replace('\\', "/"))
    }

    #[test]
    fn tools_come_from_the_given_directory() {
        let exists = present(&["C:/b/cl.exe", "C:/b/lib.exe", "C:/b/link.exe"]);
        let tools = tools_in(Path::new("C:/b"), &exists).unwrap();
        assert!(tools.cl.ends_with("cl.exe") && tools.link.ends_with("link.exe"));
    }

    #[test]
    fn missing_tools_are_named() {
        let err = tools_in(Path::new("C:/b"), &present(&["C:/b/cl.exe"])).unwrap_err();
        assert!(err.contains("lib.exe, link.exe not found in"), "{err}");
        assert!(!err.contains("cl.exe,"), "{err}");
    }

    #[test]
    fn empty_path_entries_are_ignored() {
        let found = find_in_path(";;C:\\a;", "cl.exe", &present(&["C:\\a/cl.exe"]));
        assert!(found.is_some());
        assert_eq!(find_in_path("", "cl.exe", &|_| true), None);
    }

    #[test]
    fn headers_are_found_in_any_include_directory() {
        let exists = present(&["C:\\sdk\\ucrt/stdio.h"]);
        assert!(check_headers(Some("C:\\msvc\\include;;C:\\sdk\\ucrt"), &exists).is_ok());
        assert!(check_headers(Some("C:\\msvc\\include"), &exists).is_err());
        assert!(check_headers(None, &exists).is_err());
    }

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
        let sdk_a = identity_digest(&v(&[("WindowsSDKVersion", "10.0.19041.0\\")])).unwrap();
        let sdk_b = identity_digest(&v(&[("WindowsSDKVersion", "10.0.22000.0\\")])).unwrap();
        assert_ne!(sdk_a, sdk_b);
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
        dirs: HashSet<String>,
        contents: HashMap<String, String>,
        listings: HashMap<String, Vec<String>>,
        registry: HashMap<String, String>,
        env: HashMap<String, String>,
        vswhere_out: Option<Result<String, String>>,
        vswhere_runs: RefCell<Vec<Vec<String>>>,
    }

    fn norm(p: &Path) -> String {
        let mut s = p.to_string_lossy().replace('\\', "/");
        while s.contains("//") {
            s = s.replace("//", "/");
        }
        s.trim_end_matches('/').to_string()
    }

    impl FakeHost {
        fn file(&mut self, p: &str) {
            self.files.insert(norm(Path::new(p)));
        }
        fn dir(&mut self, p: &str) {
            self.dirs.insert(norm(Path::new(p)));
        }
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
        fn dir_exists(&self, path: &Path) -> bool {
            self.dirs.contains(&norm(path))
        }
        fn read_file(&self, path: &Path) -> Result<String, String> {
            self.contents
                .get(&norm(path))
                .cloned()
                .ok_or_else(|| "not found".to_string())
        }
        fn list_dir(&self, path: &Path) -> Result<Vec<String>, String> {
            self.listings
                .get(&norm(path))
                .cloned()
                .ok_or_else(|| "The system cannot find the path specified.".to_string())
        }
        fn registry_string(&self, key: &str, value: &str) -> Option<String> {
            self.registry.get(&format!("{key}\\{value}")).cloned()
        }
        fn env_var(&self, name: &str) -> Option<String> {
            self.env.get(name).cloned()
        }
        fn run_vswhere(&self, _: &Path, args: &[String]) -> Result<String, String> {
            self.vswhere_runs.borrow_mut().push(args.to_vec());
            self.vswhere_out
                .clone()
                .unwrap_or_else(|| Err("not stubbed".into()))
        }
    }

    const VS: &str = "C:\\VS\\2022\\Community";
    const VSWHERE: &str = "C:\\PF86/Microsoft Visual Studio/Installer/vswhere.exe";
    const TOOLSET: &str = "C:\\VS\\2022\\Community/VC/Tools/MSVC/14.35.32215";
    const KITS: &str = "C:\\Kits\\10\\";
    const VERSION_FILE: &str =
        "C:\\VS\\2022\\Community/VC/Auxiliary/Build/Microsoft.VCToolsVersion.default.txt";

    fn registry_key() -> String {
        format!("{SDK_ROOTS_KEY}\\{SDK_ROOT_VALUE}")
    }

    fn add_sdk(h: &mut FakeHost, root: &str, versions: &[&str]) {
        let include = format!("{root}Include");
        h.listings.insert(
            norm(Path::new(&include)),
            versions.iter().map(|s| s.to_string()).collect(),
        );
        for ver in versions {
            h.file(&format!("{include}/{ver}/ucrt/stdio.h"));
            for d in ["ucrt", "um", "shared", "winrt", "cppwinrt"] {
                h.dir(&format!("{include}/{ver}/{d}"));
            }
            for arch in ["x86", "x64", "arm64"] {
                h.dir(&format!("{root}Lib/{ver}/ucrt/{arch}"));
                h.dir(&format!("{root}Lib/{ver}/um/{arch}"));
                h.dir(&format!("{root}bin/{ver}/{arch}"));
            }
        }
    }

    /// A machine with VS 14.35.32215 (x64 and arm64 tools) and three SDKs.
    fn vs_host() -> FakeHost {
        let mut h = FakeHost {
            windows: true,
            pf86: Some(PathBuf::from("C:\\PF86")),
            vswhere_out: Some(Ok(format!("{VS}\r\n"))),
            ..Default::default()
        };
        h.env.insert("PATH".into(), "C:\\Windows\\System32".into());
        h.file(VSWHERE);
        h.file(VERSION_FILE);
        h.contents
            .insert(norm(Path::new(VERSION_FILE)), "14.35.32215\r\n".into());
        h.dir(TOOLSET);
        h.dir(&format!("{TOOLSET}/include"));
        for arch in ["x64", "arm64", "x86"] {
            h.dir(&format!("{TOOLSET}/lib/{arch}"));
            for host in ["x64", "x86", "arm64"] {
                h.dir(&format!("{TOOLSET}/bin/Host{host}/{arch}"));
                for exe in ["cl", "lib", "link"] {
                    h.file(&format!("{TOOLSET}/bin/Host{host}/{arch}/{exe}.exe"));
                }
            }
        }
        h.registry.insert(registry_key(), KITS.to_string());
        add_sdk(
            &mut h,
            KITS,
            &["10.0.18362.0", "10.0.19041.0", "10.0.22000.0"],
        );
        h
    }

    fn var<'a>(found: &'a MsvcFound, name: &str) -> &'a str {
        found
            .env
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
            .unwrap_or_else(|| panic!("{name} not set in {:?}", found.env))
    }

    fn norm_list(s: &str) -> Vec<String> {
        s.split(';').map(|p| p.replace('\\', "/")).collect()
    }

    #[test]
    fn native_discovery_lays_out_the_environment() {
        let host = vs_host();
        let found = discover_via_vswhere(VcArch::X64, VcArch::X64, &host).unwrap();
        match &found.source {
            MsvcSource::Vswhere {
                install_path,
                toolset_version,
                sdk_version,
                ..
            } => {
                assert_eq!(install_path, Path::new(VS));
                assert_eq!(toolset_version, "14.35.32215");
                assert_eq!(sdk_version, "10.0.22000.0");
            }
            other => panic!("{other:?}"),
        }
        assert!(norm(&found.tools.cl).ends_with("14.35.32215/bin/Hostx64/x64/cl.exe"));
        assert!(norm(&found.tools.link).ends_with("Hostx64/x64/link.exe"));

        let include = norm_list(var(&found, "INCLUDE"));
        assert_eq!(include.len(), 6, "{include:?}"); // no atlmfc in this fake
        assert!(include[0].ends_with("14.35.32215/include"));
        assert!(include[1].ends_with("Include/10.0.22000.0/ucrt"));
        assert!(include.iter().any(|d| d.ends_with("/cppwinrt")));
        let lib = norm_list(var(&found, "LIB"));
        assert!(lib[0].ends_with("14.35.32215/lib/x64"), "{lib:?}");
        assert!(lib.iter().any(|d| d.ends_with("Lib/10.0.22000.0/ucrt/x64")));
        assert!(lib.iter().any(|d| d.ends_with("Lib/10.0.22000.0/um/x64")));
        assert_eq!(var(&found, "VCToolsVersion"), "14.35.32215");
        assert_eq!(var(&found, "WindowsSDKVersion"), "10.0.22000.0\\");

        // Tools first, the SDK's rc.exe directory next, then the old PATH.
        let path = norm_list(var(&found, "PATH"));
        assert!(path[0].ends_with("bin/Hostx64/x64"), "{path:?}");
        assert!(path[1].ends_with("bin/10.0.22000.0/x64"), "{path:?}");
        assert_eq!(path.last().unwrap(), "C:/Windows/System32");
        assert_eq!(path.len(), 3);

        assert!(found.found_via().contains("vswhere"));
        assert!(found.found_via().contains(VS));
        assert!(found.found_via().contains("14.35.32215"));
        assert!(found.found_via().contains("10.0.22000.0"));
        assert!(host.vswhere_runs.borrow()[0]
            .iter()
            .any(|a| a == COMPONENT_X86_X64));
        assert_eq!(host.vswhere_runs.borrow().len(), 1);
    }

    #[test]
    fn a_cross_target_uses_the_cross_tools_and_both_bin_directories() {
        let host = vs_host();
        let found = discover_via_vswhere(VcArch::X64, VcArch::Arm64, &host).unwrap();
        assert!(host.vswhere_runs.borrow()[0]
            .iter()
            .any(|a| a == COMPONENT_ARM64));
        assert!(norm(&found.tools.cl).ends_with("bin/Hostx64/arm64/cl.exe"));
        let path = norm_list(var(&found, "PATH"));
        assert!(path[0].ends_with("bin/Hostx64/arm64"), "{path:?}");
        // The compiler's own DLLs are in the host-native directory.
        assert!(path[1].ends_with("bin/Hostx64/x64"), "{path:?}");
        assert!(path[2].ends_with("bin/10.0.22000.0/x64"), "{path:?}");
        let lib = norm_list(var(&found, "LIB"));
        assert!(lib.iter().all(|d| !d.ends_with("/x64")), "{lib:?}");
        assert!(lib.iter().any(|d| d.ends_with("um/arm64")), "{lib:?}");
        assert!(found.found_via().contains("x64-hosted arm64"));
    }

    #[test]
    fn the_target_architecture_changes_the_identity() {
        let host = vs_host();
        let x64 = discover_via_vswhere(VcArch::X64, VcArch::X64, &host).unwrap();
        let arm = discover_via_vswhere(VcArch::X64, VcArch::Arm64, &host).unwrap();
        assert_ne!(identity_digest(&x64.env), identity_digest(&arm.env));
        // ... and so do the toolset and the SDK.
        let mut newer = vs_host();
        newer
            .contents
            .insert(norm(Path::new(VERSION_FILE)), "14.36.32532".into());
        let t2 = format!("{VS}/VC/Tools/MSVC/14.36.32532");
        newer.dir(&t2);
        newer.dir(&format!("{t2}/include"));
        newer.dir(&format!("{t2}/lib/x64"));
        for exe in ["cl", "lib", "link"] {
            newer.file(&format!("{t2}/bin/Hostx64/x64/{exe}.exe"));
        }
        let bumped = discover_via_vswhere(VcArch::X64, VcArch::X64, &newer).unwrap();
        assert_ne!(identity_digest(&x64.env), identity_digest(&bumped.env));
        let mut no_new_sdk = vs_host();
        no_new_sdk.listings.insert(
            "C:/Kits/10/Include".into(),
            vec!["10.0.19041.0".into(), "10.0.18362.0".into()],
        );
        let older = discover_via_vswhere(VcArch::X64, VcArch::X64, &no_new_sdk).unwrap();
        assert_ne!(identity_digest(&x64.env), identity_digest(&older.env));
    }

    #[test]
    fn atlmfc_is_added_only_when_present() {
        let mut host = vs_host();
        host.dir(&format!("{TOOLSET}/atlmfc/include"));
        host.dir(&format!("{TOOLSET}/atlmfc/lib/x64"));
        let found = discover_via_vswhere(VcArch::X64, VcArch::X64, &host).unwrap();
        let include = norm_list(var(&found, "INCLUDE"));
        assert!(include.iter().any(|d| d.ends_with("atlmfc/include")));
        let lib = norm_list(var(&found, "LIB"));
        assert!(lib.iter().any(|d| d.ends_with("atlmfc/lib/x64")));
    }

    #[test]
    fn discovery_works_when_system32_is_not_on_path() {
        // The case vcvarsall.bat half-handled: nothing in the inherited PATH.
        let mut host = vs_host();
        host.env.clear();
        let found = discover_via_vswhere(VcArch::X64, VcArch::X64, &host).unwrap();
        assert!(norm_list(var(&found, "INCLUDE"))
            .iter()
            .any(|d| d.ends_with("ucrt")));
        assert_eq!(norm_list(var(&found, "PATH")).len(), 2);
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
        host.files.remove(&norm(Path::new(VSWHERE)));
        let err = discover_via_vswhere(VcArch::X64, VcArch::X64, &host).unwrap_err();
        assert!(err.contains("vswhere.exe not found"), "{err}");
        assert!(err.contains("PF86"), "{err}");
    }

    #[test]
    fn vswhere_falls_back_to_path() {
        let mut host = vs_host();
        host.files.remove(&norm(Path::new(VSWHERE)));
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
    fn a_missing_toolset_version_file_is_reported() {
        let mut host = vs_host();
        host.files.remove(&norm(Path::new(VERSION_FILE)));
        let err = discover_via_vswhere(VcArch::X64, VcArch::X64, &host).unwrap_err();
        assert!(err.contains("toolset version file"), "{err}");
        assert!(
            err.contains("Microsoft.VCToolsVersion.default.txt"),
            "{err}"
        );
        assert!(err.ends_with("missing"), "{err}");
    }

    #[test]
    fn an_empty_toolset_version_file_is_reported() {
        let mut host = vs_host();
        host.contents
            .insert(norm(Path::new(VERSION_FILE)), "\r\n".into());
        let err = discover_via_vswhere(VcArch::X64, VcArch::X64, &host).unwrap_err();
        assert!(err.contains("is empty"), "{err}");
    }

    #[test]
    fn a_missing_toolset_directory_is_reported() {
        let mut host = vs_host();
        host.dirs.remove(&norm(Path::new(TOOLSET)));
        let err = discover_via_vswhere(VcArch::X64, VcArch::X64, &host).unwrap_err();
        assert!(err.contains("toolset directory"), "{err}");
        assert!(err.contains("14.35.32215"), "{err}");
    }

    #[test]
    fn missing_tools_for_the_pair_are_reported() {
        let mut host = vs_host();
        host.files.remove(&format!(
            "{}/bin/Hostx64/arm64/link.exe",
            norm(Path::new(TOOLSET))
        ));
        let err = discover_via_vswhere(VcArch::X64, VcArch::Arm64, &host).unwrap_err();
        assert!(err.starts_with("link.exe not found in"), "{err}");
        assert!(err.contains("Hostx64"), "{err}");
        assert!(discover_via_vswhere(VcArch::X64, VcArch::X64, &host).is_ok());
    }

    #[test]
    fn the_sdk_falls_back_to_program_files_without_a_registry_value() {
        let mut host = vs_host();
        host.registry.clear();
        // Move the SDK to where the installer puts it by default.
        host.listings.clear();
        add_sdk(&mut host, "C:\\PF86\\Windows Kits\\10\\", &["10.0.19041.0"]);
        let found = discover_via_vswhere(VcArch::X64, VcArch::X64, &host).unwrap();
        match found.source {
            MsvcSource::Vswhere {
                sdk_root,
                sdk_version,
                ..
            } => {
                assert!(norm(&sdk_root).ends_with("Windows Kits/10"));
                assert_eq!(sdk_version, "10.0.19041.0");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_wow64_registry_view_is_read_too() {
        let mut host = vs_host();
        host.registry.clear();
        host.registry.insert(
            format!("{SDK_ROOTS_KEY_WOW64}\\{SDK_ROOT_VALUE}"),
            KITS.to_string(),
        );
        let found = discover_via_vswhere(VcArch::X64, VcArch::X64, &host).unwrap();
        assert!(found.found_via().contains("10.0.22000.0"));
    }

    #[test]
    fn an_sdk_version_without_ucrt_headers_is_skipped() {
        let mut host = vs_host();
        host.files
            .remove("C:/Kits/10/Include/10.0.22000.0/ucrt/stdio.h");
        let found = discover_via_vswhere(VcArch::X64, VcArch::X64, &host).unwrap();
        assert!(found.found_via().contains("10.0.19041.0"));
    }

    #[test]
    fn no_windows_sdk_is_reported_with_where_it_looked() {
        let mut host = vs_host();
        host.registry.clear();
        host.listings.clear();
        let err = discover_via_vswhere(VcArch::X64, VcArch::X64, &host).unwrap_err();
        assert!(
            err.starts_with("no Windows SDK with ucrt headers under"),
            "{err}"
        );
        assert!(err.contains("Windows Kits"), "{err}");
    }

    #[test]
    fn a_registry_root_and_the_default_root_are_not_listed_twice() {
        let mut host = vs_host();
        host.registry
            .insert(registry_key(), "C:\\PF86\\Windows Kits\\10\\".into());
        host.listings.clear();
        let err = discover_via_vswhere(VcArch::X64, VcArch::X64, &host).unwrap_err();
        assert_eq!(err.matches("Windows Kits").count(), 1, "{err}");
    }

    // --- locate_msvc ---

    fn configured_host() -> FakeHost {
        let mut host = vs_host();
        for (n, p) in [
            ("cl", "C:\\dev\\cl.exe"),
            ("lib", "C:\\dev\\lib.exe"),
            ("link", "C:\\dev\\link.exe"),
        ] {
            host.path_programs.insert(n.into(), PathBuf::from(p));
        }
        host.env.insert("INCLUDE".into(), "C:\\dev\\inc".into());
        host.env.insert("LIB".into(), "l".into());
        host.file("C:/dev/inc/stdio.h");
        host
    }

    #[test]
    fn a_configured_environment_without_headers_falls_through_to_vswhere() {
        let mut host = configured_host();
        host.files.remove("C:/dev/inc/stdio.h");
        let found = locate_msvc(&t("x86_64-pc-windows-msvc"), Some(VcArch::X64), &host).unwrap();
        assert!(matches!(found.source, MsvcSource::Vswhere { .. }));
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
        match found.source {
            MsvcSource::Vswhere {
                host_arch,
                target_arch,
                ..
            } => {
                assert_eq!((host_arch, target_arch), (VcArch::X64, VcArch::Arm64));
            }
            other => panic!("{other:?}"),
        }
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
