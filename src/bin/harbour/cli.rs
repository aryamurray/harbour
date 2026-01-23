//! CLI definitions using clap.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

/// Harbour - A Cargo-like package manager and build system for C
#[derive(Parser)]
#[command(name = "harbour")]
#[command(author, version, about, long_about = None)]
pub struct Cli {
    /// Enable verbose output
    #[arg(short, long, global = true)]
    pub verbose: bool,

    /// Disable colored output
    #[arg(long, global = true)]
    pub no_color: bool,

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

    /// Specific targets to build
    #[arg(long)]
    pub target: Vec<String>,

    /// Emit compile_commands.json
    #[arg(long)]
    pub emit_compile_commands: bool,

    /// Emit build plan as JSON (no build)
    #[arg(long)]
    pub plan: bool,

    /// Number of parallel jobs
    #[arg(short, long)]
    pub jobs: Option<usize>,
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
}

#[derive(Args)]
pub struct RemoveArgs {
    /// Package name to remove
    pub name: String,
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
