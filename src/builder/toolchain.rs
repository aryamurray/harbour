//! Toolchain abstraction for C/C++ compilers.
//!
//! This module provides a unified interface for generating compiler/linker
//! commands across different toolchains (GCC, Clang, MSVC).

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

use crate::core::manifest::{CppRuntime, MsvcRuntime};
use crate::core::target::{CppStandard, Language};

/// C++ compilation options.
///
/// These options affect the entire build graph and are computed
/// from CppConstraints.
#[derive(Debug, Clone)]
pub struct CxxOptions {
    /// Effective C++ standard for the build
    pub std: Option<CppStandard>,
    /// Whether exceptions are enabled (graph-wide)
    pub exceptions: bool,
    /// Whether RTTI is enabled (graph-wide)
    pub rtti: bool,
    /// C++ runtime library (non-MSVC)
    pub runtime: Option<CppRuntime>,
    /// MSVC runtime library (Windows only)
    pub msvc_runtime: MsvcRuntime,
    /// Whether this is a debug build (affects MSVC runtime flag)
    pub is_debug: bool,
}

impl Default for CxxOptions {
    fn default() -> Self {
        CxxOptions {
            std: None,
            exceptions: true,
            rtti: true,
            runtime: None,
            msvc_runtime: MsvcRuntime::default(),
            is_debug: false,
        }
    }
}

/// Link mode for executables and shared libraries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkMode {
    Executable,
    SharedLib,
}

/// A command to execute, with program, arguments, and environment.
#[derive(Debug, Clone)]
pub struct CommandSpec {
    /// The program to run (e.g., "gcc", "cl.exe")
    pub program: PathBuf,
    /// Command arguments
    pub args: Vec<String>,
    /// Environment variables to set
    pub env: Vec<(String, String)>,
}

impl CommandSpec {
    /// Create a new command spec.
    pub fn new(program: impl Into<PathBuf>) -> Self {
        CommandSpec {
            program: program.into(),
            args: Vec::new(),
            env: Vec::new(),
        }
    }

    /// Add an argument.
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Add multiple arguments.
    pub fn args(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.args.extend(args.into_iter().map(|a| a.into()));
        self
    }

    /// Add an environment variable.
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }
}

/// Input for a compile step.
#[derive(Debug, Clone)]
pub struct CompileInput {
    /// Source file to compile
    pub source: PathBuf,
    /// Output object file
    pub output: PathBuf,
    /// Include directories
    pub include_dirs: Vec<PathBuf>,
    /// Preprocessor defines (name, optional value)
    pub defines: Vec<(String, Option<String>)>,
    /// Additional compiler flags
    pub cflags: Vec<String>,
}

/// Input for an archive step (creating static library).
#[derive(Debug, Clone)]
pub struct ArchiveInput {
    /// Object files to archive
    pub objects: Vec<PathBuf>,
    /// Output archive file
    pub output: PathBuf,
}

/// Input for a link step.
#[derive(Debug, Clone)]
pub struct LinkInput {
    /// Object files to link
    pub objects: Vec<PathBuf>,
    /// Output file (executable or shared library)
    pub output: PathBuf,
    /// Library search paths
    pub lib_dirs: Vec<PathBuf>,
    /// Libraries to link (without -l prefix)
    pub libs: Vec<String>,
    /// Additional linker flags
    pub ldflags: Vec<String>,
}

/// The platform/family of a toolchain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolchainPlatform {
    /// GCC (GNU Compiler Collection)
    Gcc,
    /// Clang/LLVM
    Clang,
    /// Apple Clang (macOS)
    AppleClang,
    /// Microsoft Visual C++
    Msvc,
}

impl ToolchainPlatform {
    /// Get the platform name as a string.
    pub fn as_str(&self) -> &'static str {
        match self {
            ToolchainPlatform::Gcc => "gcc",
            ToolchainPlatform::Clang => "clang",
            ToolchainPlatform::AppleClang => "apple-clang",
            ToolchainPlatform::Msvc => "msvc",
        }
    }
}

/// Trait for toolchain implementations.
///
/// Each toolchain knows how to generate commands for its specific compiler.
pub trait Toolchain: Send + Sync {
    /// Get the toolchain platform.
    fn platform(&self) -> ToolchainPlatform;

    /// Get the C compiler path.
    fn compiler_path(&self) -> &Path;

    /// Get the C++ compiler path.
    fn cxx_compiler_path(&self) -> &Path;

    /// Generate a compile command.
    ///
    /// # Arguments
    /// * `input` - Compile input (source, output, flags, etc.)
    /// * `lang` - Source language (C or C++)
    /// * `cxx_opts` - C++ options (only used when lang = Cxx)
    fn compile_command(
        &self,
        input: &CompileInput,
        lang: Language,
        cxx_opts: Option<&CxxOptions>,
    ) -> CommandSpec;

    /// Generate an archive command (create static library).
    /// Note: Static libraries always use ar/lib.exe, never C++ driver.
    fn archive_command(&self, input: &ArchiveInput) -> CommandSpec;

    /// Generate a link command for shared library.
    fn link_shared_command(
        &self,
        input: &LinkInput,
        driver: Language,
        cxx_opts: Option<&CxxOptions>,
    ) -> CommandSpec;

    /// Generate a link command for executable.
    fn link_exe_command(
        &self,
        input: &LinkInput,
        driver: Language,
        cxx_opts: Option<&CxxOptions>,
    ) -> CommandSpec;

    /// Get the object file extension.
    fn object_extension(&self) -> &str;

    /// Get the static library extension.
    fn static_lib_extension(&self) -> &str;

    /// Get the shared library extension.
    fn shared_lib_extension(&self) -> &str;

    /// Get the executable extension.
    fn exe_extension(&self) -> &str;

    /// Get the static library prefix (e.g., "lib" on Unix).
    fn static_lib_prefix(&self) -> &str;

    /// Get the shared library prefix.
    fn shared_lib_prefix(&self) -> &str;
}

/// GCC/Clang toolchain (Unix-like systems).
#[derive(Debug, Clone)]
pub struct GccToolchain {
    /// Path to the C compiler
    pub cc: PathBuf,
    /// Path to the C++ compiler
    pub cxx: PathBuf,
    /// Path to the archiver
    pub ar: PathBuf,
    /// Compiler family (gcc, clang, apple-clang)
    pub family: ToolchainPlatform,
}

impl GccToolchain {
    /// Create a new GCC-style toolchain.
    pub fn new(cc: PathBuf, cxx: PathBuf, ar: PathBuf, family: ToolchainPlatform) -> Self {
        GccToolchain {
            cc,
            cxx,
            ar,
            family,
        }
    }

    /// Infer C++ compiler path from C compiler path.
    ///
    /// Handles common patterns:
    /// - gcc, x86_64-linux-gnu-gcc -> g++, x86_64-linux-gnu-g++
    /// - clang -> clang++
    /// - cc, /usr/bin/cc -> c++, /usr/bin/c++
    pub fn infer_cxx(cc: &Path) -> PathBuf {
        let cc_str = cc.to_string_lossy();

        // gcc or *-gcc -> g++ or *-g++
        if cc_str.ends_with("gcc") {
            return PathBuf::from(format!("{}++", &cc_str[..cc_str.len() - 2]));
        }

        // clang -> clang++
        if cc_str.ends_with("clang") {
            return PathBuf::from(format!("{}++", cc_str));
        }

        // Only match "cc" when it's a complete basename (not "mycc")
        // Check for: "cc", "/cc", or "-cc" at the end
        let is_standalone_cc = cc_str == "cc"
            || cc_str.ends_with("/cc")
            || cc_str.ends_with("\\cc")
            || cc_str.ends_with("-cc");

        if is_standalone_cc {
            // Replace the final "cc" with "c++"
            return PathBuf::from(format!("{}++", &cc_str[..cc_str.len() - 1]));
        }

        // Fallback: append ++ (handles edge cases like "tcc" -> "tcc++")
        PathBuf::from(format!("{}++", cc_str))
    }
}

impl Toolchain for GccToolchain {
    fn platform(&self) -> ToolchainPlatform {
        self.family
    }

    fn compiler_path(&self) -> &Path {
        &self.cc
    }

    fn cxx_compiler_path(&self) -> &Path {
        &self.cxx
    }

    fn compile_command(
        &self,
        input: &CompileInput,
        lang: Language,
        cxx_opts: Option<&CxxOptions>,
    ) -> CommandSpec {
        // Select compiler based on language
        let compiler = match lang {
            Language::C => &self.cc,
            Language::Cxx => &self.cxx,
        };

        let mut cmd = CommandSpec::new(compiler);

        // Compile only
        cmd = cmd.arg("-c");

        // C++ specific flags
        if lang == Language::Cxx {
            if let Some(opts) = cxx_opts {
                // C++ standard
                if let Some(std) = opts.std {
                    cmd = cmd.arg(format!("-std={}", std.as_flag_value()));
                }

                // Exceptions
                if !opts.exceptions {
                    cmd = cmd.arg("-fno-exceptions");
                }

                // RTTI
                if !opts.rtti {
                    cmd = cmd.arg("-fno-rtti");
                }

                // C++ runtime library (clang only, and only on non-Apple platforms)
                if self.family == ToolchainPlatform::Clang {
                    if let Some(runtime) = opts.runtime {
                        cmd = cmd.arg(runtime.as_flag());
                    }
                }
            }
        }

        // Include directories
        for dir in &input.include_dirs {
            cmd = cmd.arg(format!("-I{}", dir.display()));
        }

        // Defines
        for (name, value) in &input.defines {
            match value {
                Some(v) => cmd = cmd.arg(format!("-D{}={}", name, v)),
                None => cmd = cmd.arg(format!("-D{}", name)),
            }
        }

        // Custom flags
        cmd = cmd.args(input.cflags.iter().cloned());

        // Input and output
        cmd = cmd.arg(input.source.display().to_string());
        cmd = cmd.arg("-o");
        cmd = cmd.arg(input.output.display().to_string());

        cmd
    }

    fn archive_command(&self, input: &ArchiveInput) -> CommandSpec {
        let mut cmd = CommandSpec::new(&self.ar);

        // Create archive with symbol index, replace files
        cmd = cmd.arg("rcs");
        cmd = cmd.arg(input.output.display().to_string());

        // Object files
        for obj in &input.objects {
            cmd = cmd.arg(obj.display().to_string());
        }

        cmd
    }

    fn link_shared_command(
        &self,
        input: &LinkInput,
        driver: Language,
        cxx_opts: Option<&CxxOptions>,
    ) -> CommandSpec {
        // Select linker driver based on language
        let linker = match driver {
            Language::C => &self.cc,
            Language::Cxx => &self.cxx,
        };

        let mut cmd = CommandSpec::new(linker);

        // Shared library flag
        cmd = cmd.arg("-shared");

        // C++ runtime library (for linking)
        if driver == Language::Cxx {
            if let Some(opts) = cxx_opts {
                if self.family == ToolchainPlatform::Clang {
                    if let Some(runtime) = opts.runtime {
                        cmd = cmd.arg(runtime.as_flag());
                    }
                }
            }
        }

        // Output
        cmd = cmd.arg("-o");
        cmd = cmd.arg(input.output.display().to_string());

        // Object files
        for obj in &input.objects {
            cmd = cmd.arg(obj.display().to_string());
        }

        // Library search paths
        for dir in &input.lib_dirs {
            cmd = cmd.arg(format!("-L{}", dir.display()));
        }

        // Libraries
        for lib in &input.libs {
            cmd = cmd.arg(format!("-l{}", lib));
        }

        // Custom flags
        cmd = cmd.args(input.ldflags.iter().cloned());

        cmd
    }

    fn link_exe_command(
        &self,
        input: &LinkInput,
        driver: Language,
        cxx_opts: Option<&CxxOptions>,
    ) -> CommandSpec {
        // Select linker driver based on language
        let linker = match driver {
            Language::C => &self.cc,
            Language::Cxx => &self.cxx,
        };

        let mut cmd = CommandSpec::new(linker);

        // C++ runtime library (for linking)
        if driver == Language::Cxx {
            if let Some(opts) = cxx_opts {
                if self.family == ToolchainPlatform::Clang {
                    if let Some(runtime) = opts.runtime {
                        cmd = cmd.arg(runtime.as_flag());
                    }
                }
            }
        }

        // Output
        cmd = cmd.arg("-o");
        cmd = cmd.arg(input.output.display().to_string());

        // Object files
        for obj in &input.objects {
            cmd = cmd.arg(obj.display().to_string());
        }

        // Library search paths
        for dir in &input.lib_dirs {
            cmd = cmd.arg(format!("-L{}", dir.display()));
        }

        // Libraries
        for lib in &input.libs {
            cmd = cmd.arg(format!("-l{}", lib));
        }

        // Custom flags
        cmd = cmd.args(input.ldflags.iter().cloned());

        cmd
    }

    fn object_extension(&self) -> &str {
        "o"
    }

    fn static_lib_extension(&self) -> &str {
        "a"
    }

    fn shared_lib_extension(&self) -> &str {
        if cfg!(target_os = "macos") {
            "dylib"
        } else {
            "so"
        }
    }

    fn exe_extension(&self) -> &str {
        ""
    }

    fn static_lib_prefix(&self) -> &str {
        "lib"
    }

    fn shared_lib_prefix(&self) -> &str {
        "lib"
    }
}

/// MSVC toolchain (Windows).
#[derive(Debug, Clone)]
pub struct MsvcToolchain {
    /// Path to cl.exe (compiler)
    pub cl: PathBuf,
    /// Path to lib.exe (librarian)
    pub lib: PathBuf,
    /// Path to link.exe (linker)
    pub link: PathBuf,
}

impl MsvcToolchain {
    /// Create a new MSVC toolchain.
    pub fn new(cl: PathBuf, lib: PathBuf, link: PathBuf) -> Self {
        MsvcToolchain { cl, lib, link }
    }
}

impl Toolchain for MsvcToolchain {
    fn platform(&self) -> ToolchainPlatform {
        ToolchainPlatform::Msvc
    }

    fn compiler_path(&self) -> &Path {
        &self.cl
    }

    fn cxx_compiler_path(&self) -> &Path {
        // MSVC uses the same cl.exe for both C and C++
        &self.cl
    }

    fn compile_command(
        &self,
        input: &CompileInput,
        lang: Language,
        cxx_opts: Option<&CxxOptions>,
    ) -> CommandSpec {
        let mut cmd = CommandSpec::new(&self.cl);

        // Quiet logo, compile only
        cmd = cmd.arg("/nologo");
        cmd = cmd.arg("/c");

        // C++ specific flags
        if lang == Language::Cxx {
            // Force C++ compilation
            cmd = cmd.arg("/TP");

            if let Some(opts) = cxx_opts {
                // C++ standard
                if let Some(std) = opts.std {
                    cmd = cmd.arg(format!("/std:{}", std.as_msvc_flag_value()));
                }

                // Exceptions
                if opts.exceptions {
                    cmd = cmd.arg("/EHsc");
                } else {
                    cmd = cmd.arg("/EHs-c-");
                }

                // RTTI
                if !opts.rtti {
                    cmd = cmd.arg("/GR-");
                }

                // MSVC runtime (/MD or /MT)
                let runtime_flag = if opts.is_debug {
                    opts.msvc_runtime.as_debug_flag()
                } else {
                    opts.msvc_runtime.as_flag()
                };
                cmd = cmd.arg(runtime_flag);
            }
        }

        // Include directories
        for dir in &input.include_dirs {
            cmd = cmd.arg(format!("/I{}", dir.display()));
        }

        // Defines
        for (name, value) in &input.defines {
            match value {
                Some(v) => cmd = cmd.arg(format!("/D{}={}", name, v)),
                None => cmd = cmd.arg(format!("/D{}", name)),
            }
        }

        // Custom flags
        cmd = cmd.args(input.cflags.iter().cloned());

        // Input
        cmd = cmd.arg(input.source.display().to_string());

        // Output
        cmd = cmd.arg(format!("/Fo{}", input.output.display()));

        cmd
    }

    fn archive_command(&self, input: &ArchiveInput) -> CommandSpec {
        let mut cmd = CommandSpec::new(&self.lib);

        cmd = cmd.arg("/nologo");
        cmd = cmd.arg(format!("/OUT:{}", input.output.display()));

        // Object files
        for obj in &input.objects {
            cmd = cmd.arg(obj.display().to_string());
        }

        cmd
    }

    fn link_shared_command(
        &self,
        input: &LinkInput,
        _driver: Language,
        _cxx_opts: Option<&CxxOptions>,
    ) -> CommandSpec {
        // MSVC uses link.exe for both C and C++ linking
        let mut cmd = CommandSpec::new(&self.link);

        cmd = cmd.arg("/nologo");
        cmd = cmd.arg("/DLL");
        cmd = cmd.arg(format!("/OUT:{}", input.output.display()));

        // Object files
        for obj in &input.objects {
            cmd = cmd.arg(obj.display().to_string());
        }

        // Library search paths
        for dir in &input.lib_dirs {
            cmd = cmd.arg(format!("/LIBPATH:{}", dir.display()));
        }

        // Libraries
        for lib in &input.libs {
            cmd = cmd.arg(format!("{}.lib", lib));
        }

        // Custom flags
        cmd = cmd.args(input.ldflags.iter().cloned());

        cmd
    }

    fn link_exe_command(
        &self,
        input: &LinkInput,
        _driver: Language,
        _cxx_opts: Option<&CxxOptions>,
    ) -> CommandSpec {
        // MSVC uses link.exe for both C and C++ linking
        let mut cmd = CommandSpec::new(&self.link);

        cmd = cmd.arg("/nologo");
        cmd = cmd.arg(format!("/OUT:{}", input.output.display()));

        // Object files
        for obj in &input.objects {
            cmd = cmd.arg(obj.display().to_string());
        }

        // Library search paths
        for dir in &input.lib_dirs {
            cmd = cmd.arg(format!("/LIBPATH:{}", dir.display()));
        }

        // Libraries
        for lib in &input.libs {
            cmd = cmd.arg(format!("{}.lib", lib));
        }

        // Custom flags
        cmd = cmd.args(input.ldflags.iter().cloned());

        cmd
    }

    fn object_extension(&self) -> &str {
        "obj"
    }

    fn static_lib_extension(&self) -> &str {
        "lib"
    }

    fn shared_lib_extension(&self) -> &str {
        "dll"
    }

    fn exe_extension(&self) -> &str {
        "exe"
    }

    fn static_lib_prefix(&self) -> &str {
        ""
    }

    fn shared_lib_prefix(&self) -> &str {
        ""
    }
}

/// Detect the available toolchain.
///
/// Tries to find a C compiler and related tools:
/// - On Windows with MSVC: Uses cl.exe, lib.exe, link.exe
/// - On Unix-like systems: Uses CC or cc/gcc/clang, plus ar
pub fn detect_toolchain() -> Result<Box<dyn Toolchain>> {
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
         Set the CC environment variable or install a compiler."
    )
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

/// MSVC toolchain with captured environment variables.
/// Used when auto-detecting MSVC outside of Developer Command Prompt.
#[cfg(target_os = "windows")]
#[derive(Debug, Clone)]
pub struct MsvcToolchainWithEnv {
    /// Base toolchain paths
    inner: MsvcToolchain,
    /// Environment variables to set when invoking tools
    env_vars: Vec<(String, String)>,
}

#[cfg(target_os = "windows")]
impl Toolchain for MsvcToolchainWithEnv {
    fn platform(&self) -> ToolchainPlatform {
        ToolchainPlatform::Msvc
    }

    fn compiler_path(&self) -> &Path {
        &self.inner.cl
    }

    fn cxx_compiler_path(&self) -> &Path {
        &self.inner.cl
    }

    fn compile_command(
        &self,
        input: &CompileInput,
        lang: Language,
        cxx_opts: Option<&CxxOptions>,
    ) -> CommandSpec {
        let mut cmd = self.inner.compile_command(input, lang, cxx_opts);
        // Add captured environment variables
        for (key, value) in &self.env_vars {
            cmd = cmd.env(key, value);
        }
        cmd
    }

    fn archive_command(&self, input: &ArchiveInput) -> CommandSpec {
        let mut cmd = self.inner.archive_command(input);
        for (key, value) in &self.env_vars {
            cmd = cmd.env(key, value);
        }
        cmd
    }

    fn link_shared_command(
        &self,
        input: &LinkInput,
        driver: Language,
        cxx_opts: Option<&CxxOptions>,
    ) -> CommandSpec {
        let mut cmd = self.inner.link_shared_command(input, driver, cxx_opts);
        for (key, value) in &self.env_vars {
            cmd = cmd.env(key, value);
        }
        cmd
    }

    fn link_exe_command(
        &self,
        input: &LinkInput,
        driver: Language,
        cxx_opts: Option<&CxxOptions>,
    ) -> CommandSpec {
        let mut cmd = self.inner.link_exe_command(input, driver, cxx_opts);
        for (key, value) in &self.env_vars {
            cmd = cmd.env(key, value);
        }
        cmd
    }

    fn object_extension(&self) -> &str {
        self.inner.object_extension()
    }

    fn static_lib_extension(&self) -> &str {
        self.inner.static_lib_extension()
    }

    fn shared_lib_extension(&self) -> &str {
        self.inner.shared_lib_extension()
    }

    fn exe_extension(&self) -> &str {
        self.inner.exe_extension()
    }

    fn static_lib_prefix(&self) -> &str {
        self.inner.static_lib_prefix()
    }

    fn shared_lib_prefix(&self) -> &str {
        self.inner.shared_lib_prefix()
    }
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

    Ok(Some(Box::new(MsvcToolchainWithEnv {
        inner: MsvcToolchain::new(cl, lib, link),
        env_vars: captured_env,
    })))
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

#[cfg(not(target_os = "windows"))]
fn try_detect_msvc() -> Result<Option<Box<dyn Toolchain>>> {
    Ok(None)
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
