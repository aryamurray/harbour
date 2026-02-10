//! Shared utilities

pub mod config;
pub mod context;
pub mod diagnostic;
pub mod fs;
pub mod hash;
pub mod interning;
pub mod process;
pub mod shell;
pub mod vcpkg;

pub use config::{
    global_config_dir, global_toolchain_config_path, load_toolchain_config,
    project_toolchain_config_path, Config, ToolchainConfig, ToolchainSettings,
};
pub use context::GlobalContext;
pub use diagnostic::Diagnostic;
pub use interning::InternedString;
pub use shell::{ColorChoice, Progress, Shell, ShellMode, Span, Status, Verbosity};
pub use vcpkg::VcpkgIntegration;
