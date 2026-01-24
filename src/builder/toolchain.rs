//! Toolchain abstraction for different C compilers.
//!
//! This module provides a unified interface for generating compiler/linker
//! commands across different toolchains (GCC, Clang, MSVC).

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

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

    /// Generate a compile command.
    fn compile_command(&self, input: &CompileInput) -> CommandSpec;

    /// Generate an archive command (create static library).
    fn archive_command(&self, input: &ArchiveInput) -> CommandSpec;

    /// Generate a link command for shared library.
    fn link_shared_command(&self, input: &LinkInput) -> CommandSpec;

    /// Generate a link command for executable.
    fn link_exe_command(&self, input: &LinkInput) -> CommandSpec;

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
    /// Path to the archiver
    pub ar: PathBuf,
    /// Compiler family (gcc, clang, apple-clang)
    pub family: ToolchainPlatform,
}

impl GccToolchain {
    /// Create a new GCC-style toolchain.
    pub fn new(cc: PathBuf, ar: PathBuf, family: ToolchainPlatform) -> Self {
        GccToolchain { cc, ar, family }
    }
}

impl Toolchain for GccToolchain {
    fn platform(&self) -> ToolchainPlatform {
        self.family
    }

    fn compile_command(&self, input: &CompileInput) -> CommandSpec {
        let mut cmd = CommandSpec::new(&self.cc);

        // Compile only
        cmd = cmd.arg("-c");

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

    fn link_shared_command(&self, input: &LinkInput) -> CommandSpec {
        let mut cmd = CommandSpec::new(&self.cc);

        // Shared library flag
        cmd = cmd.arg("-shared");

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

    fn link_exe_command(&self, input: &LinkInput) -> CommandSpec {
        let mut cmd = CommandSpec::new(&self.cc);

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

    fn compile_command(&self, input: &CompileInput) -> CommandSpec {
        let mut cmd = CommandSpec::new(&self.cl);

        // Quiet logo, compile only
        cmd = cmd.arg("/nologo");
        cmd = cmd.arg("/c");

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

    fn link_shared_command(&self, input: &LinkInput) -> CommandSpec {
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

    fn link_exe_command(&self, input: &LinkInput) -> CommandSpec {
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

    // Try to find cl.exe
    let cl = match which("cl") {
        Ok(p) => p,
        Err(_) => return Ok(None),
    };

    // Verify MSVC environment is set up
    if std::env::var("INCLUDE").is_err() || std::env::var("LIB").is_err() {
        bail!(
            "MSVC compiler found but environment not configured.\n\
             Run from Visual Studio Developer Command Prompt,\n\
             or run vcvarsall.bat first."
        );
    }

    // Find lib.exe and link.exe
    let lib = which("lib").map_err(|_| {
        anyhow::anyhow!("MSVC cl.exe found but lib.exe not in PATH")
    })?;
    let link = which("link").map_err(|_| {
        anyhow::anyhow!("MSVC cl.exe found but link.exe not in PATH")
    })?;

    Ok(Some(Box::new(MsvcToolchain::new(cl, lib, link))))
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

    Ok(Some(Box::new(GccToolchain::new(cc, ar, family))))
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
    let output = std::process::Command::new(cc)
        .arg("--version")
        .output();

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
    let output = std::process::Command::new(cc)
        .arg("--version")
        .output();

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

        let cmd = toolchain.compile_command(&input);
        assert_eq!(cmd.program, PathBuf::from("gcc"));
        assert!(cmd.args.contains(&"-c".to_string()));
        assert!(cmd.args.contains(&"-I/usr/include".to_string()));
        assert!(cmd.args.contains(&"-DDEBUG".to_string()));
        assert!(cmd.args.contains(&"-DVERSION=1".to_string()));
        assert!(cmd.args.contains(&"-Wall".to_string()));
    }

    #[test]
    fn test_gcc_archive_command() {
        let toolchain = GccToolchain::new(
            PathBuf::from("gcc"),
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

        let cmd = toolchain.compile_command(&input);
        assert_eq!(cmd.program, PathBuf::from("cl"));
        assert!(cmd.args.contains(&"/nologo".to_string()));
        assert!(cmd.args.contains(&"/c".to_string()));
        assert!(cmd.args.iter().any(|a| a.starts_with("/I")));
        assert!(cmd.args.contains(&"/DDEBUG".to_string()));
        assert!(cmd.args.contains(&"/DVERSION=1".to_string()));
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
