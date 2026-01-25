//! CLI definitions using clap.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;

/// Message output format for build commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum MessageFormat {
    /// Human-readable output (default)
    #[default]
    Human,
    /// Machine-readable JSON output
    Json,
}

/// Harbour - A Cargo-like package manager and build system for C
#[derive(Parser)]
#[command(name = "harbour")]
#[command(author, version, about, long_about = None)]
pub struct Cli {
    /// Suppress non-error output
    #[arg(short, long, global = true)]
    pub quiet: bool,

    /// Enable verbose output (debug/info)
    #[arg(short, long, global = true)]
    pub verbose: bool,

    /// Color output: auto, always, never
    #[arg(long, global = true, default_value = "auto")]
    pub color: String,

    /// Run without network access
    #[arg(long, global = true)]
    pub offline: bool,

    /// Require lockfile to be up-to-date (error if resolution would change it)
    #[arg(long, global = true)]
    pub locked: bool,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Create a new Harbour package
    New(NewArgs),

    /// Initialize a Harbour package in an existing directory
    Init(InitArgs),

    /// Build the current package
    Build(BuildArgs),

    /// Add a dependency to Harbor.toml
    Add(AddArgs),

    /// Remove a dependency from Harbor.toml
    Remove(RemoveArgs),

    /// Update dependencies (modifies Harbor.lock)
    Update(UpdateArgs),

    /// Remove build artifacts
    Clean(CleanArgs),

    /// Display the dependency tree
    Tree(TreeArgs),

    /// Show compile/link flags for a target
    Flags(FlagsArgs),

    /// Explain why a package is in the dependency graph
    Explain(ExplainArgs),

    /// Show the link order and sources for a target
    Linkplan(LinkplanArgs),

    /// Run tests
    Test(TestArgs),

    /// Toolchain management
    Toolchain(ToolchainArgs),

    /// Backend management
    Backend(BackendArgs),

    /// FFI bundling operations
    Ffi(FfiArgs),

    /// Check environment and toolchain health
    Doctor(DoctorArgs),

    /// Verify a package (CI-grade validation)
    Verify(VerifyArgs),

    /// Generate shell completions
    Completions(CompletionsArgs),
}

#[derive(Args)]
pub struct DoctorArgs {
    /// Show detailed information
    #[arg(short, long)]
    pub verbose: bool,

    /// Skip network checks
    #[arg(long)]
    pub offline: bool,
}

#[derive(Args)]
pub struct VerifyArgs {
    /// Package name to verify
    pub package: String,

    /// Package version (uses latest if not specified)
    #[arg(long)]
    pub version: Option<String>,

    /// Linkage to verify (static, shared, both, auto)
    #[arg(long, default_value = "auto")]
    pub linkage: String,

    /// Target platform to verify for
    #[arg(long)]
    pub platform: Option<String>,

    /// Output directory for artifacts
    #[arg(short, long)]
    pub output: Option<std::path::PathBuf>,

    /// Skip consumer test harness
    #[arg(long)]
    pub skip_harness: bool,
}

#[derive(Args)]
pub struct CompletionsArgs {
    /// Shell to generate completions for
    #[arg(value_enum)]
    pub shell: Shell,
}

#[derive(Args)]
pub struct NewArgs {
    /// Package name
    pub name: String,

    /// Create a library instead of an executable
    #[arg(long)]
    pub lib: bool,

    /// Directory to create the project in (defaults to name)
    #[arg(long)]
    pub path: Option<PathBuf>,
}

#[derive(Args)]
pub struct InitArgs {
    /// Package name (defaults to directory name)
    #[arg(long)]
    pub name: Option<String>,

    /// Create a library instead of an executable
    #[arg(long)]
    pub lib: bool,

    /// Directory to initialize (defaults to current directory)
    pub path: Option<PathBuf>,
}

#[derive(Args)]
pub struct BuildArgs {
    /// Build in release mode
    #[arg(short, long)]
    pub release: bool,

    /// Package(s) to build (can be specified multiple times)
    #[arg(short, long)]
    pub package: Vec<String>,

    /// Specific targets to build within selected packages
    #[arg(long)]
    pub target: Vec<String>,

    /// Disable compile_commands.json generation (enabled by default)
    #[arg(long)]
    pub no_compile_commands: bool,

    /// Emit build plan as JSON (no build)
    #[arg(long)]
    pub plan: bool,

    /// Number of parallel jobs
    #[arg(short, long)]
    pub jobs: Option<usize>,

    /// C++ standard version (11, 14, 17, 20, 23)
    #[arg(long, value_name = "VERSION")]
    pub std: Option<String>,

    /// Build backend to use (native, cmake, meson, custom)
    #[arg(long, value_name = "BACKEND")]
    pub backend: Option<String>,

    /// Library linkage preference (static, shared, auto)
    #[arg(long, value_name = "LINKAGE", default_value = "auto")]
    pub linkage: String,

    /// Build for FFI consumption (forces shared libraries, enables runtime bundling)
    #[arg(long)]
    pub ffi: bool,

    /// Target triple for cross-compilation (e.g., x86_64-unknown-linux-gnu)
    #[arg(long, value_name = "TRIPLE")]
    pub target_triple: Option<String>,

    /// Output format: human (default) or json
    #[arg(long, value_name = "FMT", default_value = "human")]
    pub message_format: MessageFormat,
}

#[derive(Args)]
pub struct AddArgs {
    /// Package name
    pub name: String,

    /// Path to local dependency
    #[arg(long)]
    pub path: Option<String>,

    /// Git repository URL
    #[arg(long)]
    pub git: Option<String>,

    /// Git branch
    #[arg(long)]
    pub branch: Option<String>,

    /// Git tag
    #[arg(long)]
    pub tag: Option<String>,

    /// Git revision
    #[arg(long)]
    pub rev: Option<String>,

    /// Version requirement
    #[arg(long)]
    pub version: Option<String>,

    /// Add as optional dependency
    #[arg(long)]
    pub optional: bool,

    /// Show what would change without modifying files
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Args)]
pub struct RemoveArgs {
    /// Package name to remove
    pub name: String,

    /// Show what would change without modifying files
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Args)]
pub struct UpdateArgs {
    /// Specific packages to update
    pub packages: Vec<String>,

    /// Dry run - show what would be updated
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Args)]
pub struct CleanArgs {
    /// Only clean the target directory
    #[arg(long)]
    pub target: bool,

    /// Remove everything including the cache
    #[arg(long)]
    pub all: bool,
}

#[derive(Args)]
pub struct TreeArgs {
    /// Package to show tree for (defaults to root)
    pub package: Option<String>,

    /// Maximum depth to display
    #[arg(short, long)]
    pub depth: Option<usize>,

    /// Show duplicate packages
    #[arg(long)]
    pub duplicates: bool,
}

#[derive(Args)]
pub struct FlagsArgs {
    /// Target to show flags for
    pub target: String,

    /// Show compile flags only
    #[arg(long)]
    pub compile: bool,

    /// Show link flags only
    #[arg(long)]
    pub link: bool,
}

#[derive(Args)]
pub struct ExplainArgs {
    /// Package to explain
    pub package: String,
}

#[derive(Args)]
pub struct LinkplanArgs {
    /// Target to show link plan for
    pub target: String,
}

#[derive(Args)]
pub struct TestArgs {
    /// Specific test targets to run (defaults to all test targets)
    pub targets: Vec<String>,

    /// Build in release mode
    #[arg(short, long)]
    pub release: bool,

    /// Number of parallel jobs
    #[arg(short, long)]
    pub jobs: Option<usize>,
}

#[derive(Args)]
pub struct ToolchainArgs {
    #[command(subcommand)]
    pub command: ToolchainCommands,
}

#[derive(Subcommand)]
pub enum ToolchainCommands {
    /// Show current toolchain configuration
    Show,

    /// Override the toolchain for this project
    Override(ToolchainOverrideArgs),
}

#[derive(Args)]
pub struct ToolchainOverrideArgs {
    /// C compiler path
    #[arg(long)]
    pub cc: Option<PathBuf>,

    /// Archiver path
    #[arg(long)]
    pub ar: Option<PathBuf>,

    /// Target triple
    #[arg(long)]
    pub target: Option<String>,
}

#[derive(Args)]
pub struct BackendArgs {
    #[command(subcommand)]
    pub command: BackendCommands,
}

#[derive(Subcommand)]
pub enum BackendCommands {
    /// List all available backends
    List,

    /// Show detailed information about a backend
    Show(BackendShowArgs),

    /// Check if a backend is available
    Check(BackendCheckArgs),
}

#[derive(Args)]
pub struct BackendShowArgs {
    /// Backend name (native, cmake, meson, custom)
    pub backend: String,
}

#[derive(Args)]
pub struct BackendCheckArgs {
    /// Backend name (native, cmake, meson, custom)
    pub backend: String,
}

#[derive(Args)]
pub struct FfiArgs {
    #[command(subcommand)]
    pub command: FfiCommands,
}

#[derive(Subcommand)]
pub enum FfiCommands {
    /// Create an FFI bundle with shared library and dependencies
    Bundle(FfiBundleArgs),

    /// Generate language bindings from C headers
    Generate(FfiGenerateArgs),
}

#[derive(Args)]
pub struct FfiBundleArgs {
    /// Output directory for the bundle
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    /// Build in release mode
    #[arg(short, long)]
    pub release: bool,

    /// Don't include transitive dependencies
    #[arg(long)]
    pub no_transitive: bool,

    /// Don't rewrite RPATH
    #[arg(long)]
    pub no_rpath: bool,

    /// Dry run - don't actually copy files
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Args)]
pub struct FfiGenerateArgs {
    /// Target language (typescript, python, csharp, rust)
    #[arg(short, long, value_name = "LANG")]
    pub lang: String,

    /// FFI bundler/runtime (koffi, ffi-napi, ctypes, cffi, pinvoke)
    #[arg(short, long, value_name = "BUNDLER")]
    pub bundler: Option<String>,

    /// Output directory for generated bindings
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    /// Header files to parse (glob patterns)
    #[arg(long)]
    pub header: Vec<PathBuf>,

    /// Target name to generate bindings for
    #[arg(short, long)]
    pub target: Option<String>,

    /// Prefix to strip from function names
    #[arg(long)]
    pub strip_prefix: Option<String>,

    /// Generate async function wrappers
    #[arg(long)]
    pub async_wrappers: bool,

    /// Library file path (relative to output)
    #[arg(long)]
    pub lib_path: Option<String>,

    /// Build in release mode
    #[arg(short, long)]
    pub release: bool,
}
