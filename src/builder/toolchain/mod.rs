//! Toolchain abstraction for C/C++ compilers.
//!
//! This module provides a unified interface for generating compiler/linker
//! commands across different toolchains (GCC, Clang, MSVC).
//!
//! Toolchain detection priority:
//! 1. Toolchain config file (`.harbour/toolchain.toml` or `~/.harbour/toolchain.toml`)
//! 2. Environment variables (CC, CXX, AR)
//! 3. Auto-detection (searching PATH for common compilers)

use std::path::{Path, PathBuf};

use crate::core::manifest::{CppRuntime, MsvcRuntime};
use crate::core::target::CppStandard;

mod detect;
mod gcc;
mod msvc;

pub use detect::detect_toolchain;
pub use gcc::GccToolchain;
pub use msvc::MsvcToolchain;

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

// Re-export Language from target module for convenience
pub use crate::core::target::Language;

/// A generic wrapper that injects environment variables into all commands.
///
/// This wrapper delegates all `Toolchain` methods to the inner toolchain
/// and automatically adds the configured environment variables to every
/// `CommandSpec` returned.
///
/// # Example
///
/// ```ignore
/// let gcc = GccToolchain::new(...);
/// let wrapped = EnvWrapper::new(gcc, vec![
///     ("CC".to_string(), "/usr/bin/gcc".to_string()),
///     ("CXX".to_string(), "/usr/bin/g++".to_string()),
/// ]);
/// ```
#[derive(Debug, Clone)]
pub struct EnvWrapper<T> {
    inner: T,
    env_vars: Vec<(String, String)>,
}

impl<T> EnvWrapper<T> {
    /// Create a new environment wrapper.
    pub fn new(inner: T, env_vars: Vec<(String, String)>) -> Self {
        EnvWrapper { inner, env_vars }
    }

    /// Get a reference to the inner toolchain.
    pub fn inner(&self) -> &T {
        &self.inner
    }

    /// Inject environment variables into a CommandSpec.
    fn inject_env(&self, mut cmd: CommandSpec) -> CommandSpec {
        for (key, value) in &self.env_vars {
            cmd = cmd.env(key, value);
        }
        cmd
    }
}

impl<T: Toolchain> Toolchain for EnvWrapper<T> {
    fn platform(&self) -> ToolchainPlatform {
        self.inner.platform()
    }

    fn compiler_path(&self) -> &Path {
        self.inner.compiler_path()
    }

    fn cxx_compiler_path(&self) -> &Path {
        self.inner.cxx_compiler_path()
    }

    fn compile_command(
        &self,
        input: &CompileInput,
        lang: Language,
        cxx_opts: Option<&CxxOptions>,
    ) -> CommandSpec {
        self.inject_env(self.inner.compile_command(input, lang, cxx_opts))
    }

    fn archive_command(&self, input: &ArchiveInput) -> CommandSpec {
        self.inject_env(self.inner.archive_command(input))
    }

    fn link_shared_command(
        &self,
        input: &LinkInput,
        driver: Language,
        cxx_opts: Option<&CxxOptions>,
    ) -> CommandSpec {
        self.inject_env(self.inner.link_shared_command(input, driver, cxx_opts))
    }

    fn link_exe_command(
        &self,
        input: &LinkInput,
        driver: Language,
        cxx_opts: Option<&CxxOptions>,
    ) -> CommandSpec {
        self.inject_env(self.inner.link_exe_command(input, driver, cxx_opts))
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
