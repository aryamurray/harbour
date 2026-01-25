//! Shared utilities

pub mod config;
pub mod context;
pub mod diagnostic;
pub mod fs;
pub mod hash;
pub mod interning;
pub mod process;
pub mod shell;

pub use config::Config;
pub use context::GlobalContext;
pub use diagnostic::Diagnostic;
pub use interning::InternedString;
pub use shell::{ColorChoice, Progress, Shell, ShellMode, Span, Status, Verbosity};
