//! Toolchain detection functions.

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

use crate::util::config::{
    global_toolchain_config_path, load_toolchain_config, project_toolchain_config_path,
    ToolchainConfig,
};

use super::{EnvWrapper, GccToolchain, MsvcToolchain, Toolchain, ToolchainPlatform};

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

/// Detect the available toolchain.
///
/// Tries to find a C compiler and related tools with the following priority:
/// 1. Toolchain config file (`.harbour/toolchain.toml` or `~/.harbour/toolchain.toml`)
/// 2. Environment variables (CC, CXX, AR)
/// 3. On Windows with MSVC: Uses cl.exe, lib.exe, link.exe
/// 4. On Unix-like systems: Uses cc/gcc/clang, plus ar
pub fn detect_toolchain() -> Result<Box<dyn Toolchain>> {
    // Load toolchain config
    let config = load_toolchain_config_from_files();

    // Try config-based toolchain first
    if config.has_overrides() {
        if let Some(toolchain) = try_detect_from_config(&config)? {
            return Ok(toolchain);
        }
    }

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
fn try_detect_from_config(config: &ToolchainConfig) -> Result<Option<Box<dyn Toolchain>>> {
    use which::which;

    let tc = &config.toolchain;

    // We need at least a C compiler specified
    let cc = match &tc.cc {
        Some(cc) => {
            if cc.exists() {
                cc.clone()
            } else {
                tracing::warn!(
                    "Configured C compiler not found: {}",
                    cc.display()
                );
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

    Ok(Some(Box::new(GccToolchain::new(cc, cxx, ar, family))))
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

#[cfg(not(target_os = "windows"))]
fn try_detect_msvc() -> Result<Option<Box<dyn Toolchain>>> {
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

    // Run vcvarsall.bat and capture environment
    // We create a temporary batch file to avoid Windows cmd.exe quoting issues
    let temp_dir = std::env::temp_dir();
    let temp_batch = temp_dir.join("harbour_vcvars.bat");

    let batch_content = format!(
        "@echo off\r\ncall \"{}\" {} >nul 2>&1\r\nif errorlevel 1 exit /b 1\r\nset\r\n",
        vcvarsall.display(),
        arch
    );

    if let Err(e) = std::fs::write(&temp_batch, &batch_content) {
        tracing::debug!("Failed to write temp batch file: {}", e);
        return Ok(None);
    }

    let batch_path = temp_batch
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("temp batch path contains invalid UTF-8"))?;

    let output = Command::new("cmd").args(["/c", batch_path]).output();

    // Clean up temp file
    let _ = std::fs::remove_file(&temp_batch);

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
    use super::*;
    use crate::core::manifest::MsvcRuntime;
    use crate::core::target::{CppStandard, Language};
    use super::super::{ArchiveInput, CompileInput, CxxOptions};

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
}
