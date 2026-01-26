//! User-friendly diagnostic messages.
//!
//! Implements the "Actionable Error Messages" design principle:
//! Every error must include root cause, conflicting constraints, and suggested fixes.

use std::fmt;
use std::path::PathBuf;

use miette::{Diagnostic as MietteDiagnostic, NamedSource, SourceSpan};
use thiserror::Error;

/// Common suggestion messages for consistent error handling.
pub mod suggestions {
    /// Suggestion when no manifest file is found.
    pub const NO_MANIFEST: &str = "help: Run `harbour init` to create a new project";

    /// Suggestion when the lockfile is stale or missing.
    pub const STALE_LOCK: &str = "help: Run `harbour update` to refresh dependencies";

    /// Suggestion when a target is not found.
    pub const TARGET_NOT_FOUND: &str = "help: Run `harbour tree` to see available targets";

    /// Suggestion when a package is not found.
    pub const PACKAGE_NOT_FOUND: &str = "help: Run `harbour tree` to see all dependencies";

    /// Suggestion when a dependency is missing.
    pub const MISSING_DEPENDENCY: &str =
        "help: Run `harbour add <package>` to add it as a dependency";

    /// Suggestion when build fails.
    pub const BUILD_FAILED: &str = "help: Run `harbour build --verbose` for more details";

    /// Suggestion when there are no test targets.
    pub const NO_TEST_TARGETS: &str =
        "help: Add a test target with kind = \"exe\" and name ending in _test";

    /// Suggestion for fetch failures.
    pub const FETCH_FAILED: &str = "help: Check your network connection and try `harbour update`";
}

/// Severity level for diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Note,
    Help,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Severity::Error => write!(f, "error"),
            Severity::Warning => write!(f, "warning"),
            Severity::Note => write!(f, "note"),
            Severity::Help => write!(f, "help"),
        }
    }
}

/// A diagnostic message with optional suggestions.
#[derive(Debug, Clone)]
pub struct Diagnostic {
    /// Primary message
    pub message: String,
    /// Severity level
    pub severity: Severity,
    /// Additional context lines
    pub context: Vec<String>,
    /// Suggested fixes
    pub suggestions: Vec<String>,
    /// Related location (file path)
    pub location: Option<PathBuf>,
}

impl Diagnostic {
    /// Create a new error diagnostic.
    pub fn error(message: impl Into<String>) -> Self {
        Diagnostic {
            message: message.into(),
            severity: Severity::Error,
            context: Vec::new(),
            suggestions: Vec::new(),
            location: None,
        }
    }

    /// Create a new warning diagnostic.
    pub fn warning(message: impl Into<String>) -> Self {
        Diagnostic {
            message: message.into(),
            severity: Severity::Warning,
            context: Vec::new(),
            suggestions: Vec::new(),
            location: None,
        }
    }

    /// Add context to the diagnostic.
    pub fn with_context(mut self, context: impl Into<String>) -> Self {
        self.context.push(context.into());
        self
    }

    /// Add a suggestion for fixing the issue.
    pub fn with_suggestion(mut self, suggestion: impl Into<String>) -> Self {
        self.suggestions.push(suggestion.into());
        self
    }

    /// Add a file location.
    pub fn with_location(mut self, path: impl Into<PathBuf>) -> Self {
        self.location = Some(path.into());
        self
    }

    /// Format the diagnostic for terminal output.
    pub fn format(&self, color: bool) -> String {
        let mut output = String::new();

        // Severity prefix with optional color
        let severity_str = if color {
            match self.severity {
                Severity::Error => "\x1b[1;31merror\x1b[0m",
                Severity::Warning => "\x1b[1;33mwarning\x1b[0m",
                Severity::Note => "\x1b[1;36mnote\x1b[0m",
                Severity::Help => "\x1b[1;32mhelp\x1b[0m",
            }
        } else {
            match self.severity {
                Severity::Error => "error",
                Severity::Warning => "warning",
                Severity::Note => "note",
                Severity::Help => "help",
            }
        };

        // Main message
        output.push_str(&format!("{}: {}\n", severity_str, self.message));

        // Location if present
        if let Some(ref path) = self.location {
            output.push_str(&format!("  --> {}\n", path.display()));
        }

        // Context lines
        for ctx in &self.context {
            output.push_str(&format!("  → {}\n", ctx));
        }

        // Suggestions
        if !self.suggestions.is_empty() {
            output.push('\n');
            let help_prefix = if color {
                "\x1b[1;32mhelp\x1b[0m"
            } else {
                "help"
            };
            output.push_str(&format!("{}: consider:\n", help_prefix));
            for (i, suggestion) in self.suggestions.iter().enumerate() {
                output.push_str(&format!("  {}. {}\n", i + 1, suggestion));
            }
        }

        output
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.format(false))
    }
}

/// Version conflict error with detailed diagnostics.
#[derive(Debug, Error, MietteDiagnostic)]
#[error("version conflict for `{package}`")]
#[diagnostic(
    code(harbour::resolve::version_conflict),
    help("Consider upgrading the conflicting package or vendoring one copy")
)]
pub struct VersionConflictError {
    pub package: String,
    #[source_code]
    pub src: Option<NamedSource<String>>,
    #[label("required here")]
    pub span: Option<SourceSpan>,
    pub requirements: Vec<String>,
}

/// Feature conflict error.
#[derive(Debug, Error, MietteDiagnostic)]
#[error("conflicting features for `{package}`")]
#[diagnostic(
    code(harbour::resolve::feature_conflict),
    help("Align feature selection or vendor one copy with: `harbour vendor {package}`")
)]
pub struct FeatureConflictError {
    pub package: String,
    pub conflicts: Vec<(String, String)>, // (package requiring, feature)
}

/// Missing dependency error.
#[derive(Debug, Error, MietteDiagnostic)]
#[error("could not find `{package}` in any source")]
#[diagnostic(code(harbour::resolve::not_found))]
pub struct PackageNotFoundError {
    pub package: String,
    #[help]
    pub suggestions: Option<String>,
}

/// ABI mismatch error.
#[derive(Debug, Error, MietteDiagnostic)]
#[error("ABI mismatch for `{package}`")]
#[diagnostic(
    code(harbour::build::abi_mismatch),
    help("Rebuild with consistent ABI settings or vendor the dependency")
)]
pub struct AbiMismatchError {
    pub package: String,
    pub expected: String,
    pub found: String,
}

/// Print a diagnostic to stderr.
pub fn emit(diagnostic: &Diagnostic, color: bool) {
    eprint!("{}", diagnostic.format(color));
}

/// Print an error message with context and suggestions.
pub fn emit_error(message: &str, context: &[&str], suggestions: &[&str], color: bool) {
    let mut diag = Diagnostic::error(message);
    for ctx in context {
        diag = diag.with_context(*ctx);
    }
    for sug in suggestions {
        diag = diag.with_suggestion(*sug);
    }
    emit(&diag, color);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_diagnostic_formatting() {
        let diag = Diagnostic::error("version conflict for `openssl`")
            .with_context("myapp requires openssl ^3.0")
            .with_context("legacy-lib requires openssl ^1.1 (no compatible version exists)")
            .with_suggestion("Upgrade legacy-lib to a version supporting openssl 3.x")
            .with_suggestion("Vendor openssl 1.1 for legacy-lib: `harbour vendor legacy-lib`");

        let output = diag.format(false);
        assert!(output.contains("error: version conflict"));
        assert!(output.contains("myapp requires openssl"));
        assert!(output.contains("help: consider:"));
        assert!(output.contains("1. Upgrade legacy-lib"));
    }
}
