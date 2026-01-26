//! Build event types for JSON output.
//!
//! This module defines the stable JSON schema for machine-readable build output.
//! These events are emitted when using `--message-format=json`.
//!
//! # Event Types
//!
//! - `compiler-artifact`: A compiled file was produced
//! - `build-finished`: Build completed (success or failure)
//! - `compiler-warning`: A compiler warning was emitted
//! - `compiler-error`: A compiler error was emitted
//! - `build-progress`: Progress update during build
//!
//! # Stability
//!
//! The JSON schema is versioned and should remain backwards compatible.
//! New fields may be added, but existing fields should not be removed or renamed.

use std::path::PathBuf;

use serde::Serialize;

/// A build event emitted during the build process.
///
/// Each event is serialized as a single JSON object per line.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "reason")]
pub enum BuildEvent {
    /// A compiled artifact (object file, library, executable) was produced.
    #[serde(rename = "compiler-artifact")]
    CompilerArtifact {
        /// Package identifier (e.g., "zlib v1.3.1")
        package_id: String,
        /// Target name
        target: String,
        /// Output filenames
        filenames: Vec<PathBuf>,
        /// Whether this is a fresh build (vs cached)
        #[serde(skip_serializing_if = "Option::is_none")]
        fresh: Option<bool>,
    },

    /// Build completed (success or failure).
    #[serde(rename = "build-finished")]
    BuildFinished {
        /// Whether the build succeeded
        success: bool,
        /// Total build duration in milliseconds
        duration_ms: u64,
        /// Number of targets built
        #[serde(skip_serializing_if = "Option::is_none")]
        targets_built: Option<u64>,
    },

    /// A compiler warning was emitted.
    #[serde(rename = "compiler-warning")]
    CompilerWarning {
        /// Package that produced the warning
        package_id: String,
        /// Warning message text
        message: String,
        /// Source file (if available)
        #[serde(skip_serializing_if = "Option::is_none")]
        file: Option<PathBuf>,
        /// Line number (if available)
        #[serde(skip_serializing_if = "Option::is_none")]
        line: Option<u32>,
        /// Column number (if available)
        #[serde(skip_serializing_if = "Option::is_none")]
        column: Option<u32>,
    },

    /// A compiler error was emitted.
    #[serde(rename = "compiler-error")]
    CompilerError {
        /// Package that produced the error
        package_id: String,
        /// Error message text
        message: String,
        /// Source file (if available)
        #[serde(skip_serializing_if = "Option::is_none")]
        file: Option<PathBuf>,
        /// Line number (if available)
        #[serde(skip_serializing_if = "Option::is_none")]
        line: Option<u32>,
        /// Column number (if available)
        #[serde(skip_serializing_if = "Option::is_none")]
        column: Option<u32>,
    },

    /// Build progress update.
    #[serde(rename = "build-progress")]
    Progress {
        /// Current progress count
        current: u64,
        /// Total count
        total: u64,
        /// Unit type (e.g., "files", "targets", "packages")
        unit: String,
        /// Optional phase name (e.g., "compiling", "linking")
        #[serde(skip_serializing_if = "Option::is_none")]
        phase: Option<String>,
    },

    /// A generic diagnostic message.
    #[serde(rename = "diagnostic")]
    Diagnostic {
        /// Severity level ("error", "warning", "note", "help")
        level: String,
        /// Message text
        message: String,
    },

    /// Build started event with metadata.
    #[serde(rename = "build-started")]
    BuildStarted {
        /// Profile being built (e.g., "debug", "release")
        profile: String,
        /// Target triple (e.g., "x86_64-pc-windows-msvc")
        target_triple: String,
        /// Number of packages to build
        #[serde(skip_serializing_if = "Option::is_none")]
        package_count: Option<u64>,
    },
}

impl BuildEvent {
    /// Create a compiler artifact event.
    pub fn artifact(
        package_id: impl Into<String>,
        target: impl Into<String>,
        filenames: Vec<PathBuf>,
    ) -> Self {
        BuildEvent::CompilerArtifact {
            package_id: package_id.into(),
            target: target.into(),
            filenames,
            fresh: None,
        }
    }

    /// Create a build finished event.
    pub fn finished(success: bool, duration_ms: u64) -> Self {
        BuildEvent::BuildFinished {
            success,
            duration_ms,
            targets_built: None,
        }
    }

    /// Create a compiler warning event.
    pub fn warning(package_id: impl Into<String>, message: impl Into<String>) -> Self {
        BuildEvent::CompilerWarning {
            package_id: package_id.into(),
            message: message.into(),
            file: None,
            line: None,
            column: None,
        }
    }

    /// Create a compiler error event.
    pub fn error(package_id: impl Into<String>, message: impl Into<String>) -> Self {
        BuildEvent::CompilerError {
            package_id: package_id.into(),
            message: message.into(),
            file: None,
            line: None,
            column: None,
        }
    }

    /// Create a progress event.
    pub fn progress(current: u64, total: u64, unit: impl Into<String>) -> Self {
        BuildEvent::Progress {
            current,
            total,
            unit: unit.into(),
            phase: None,
        }
    }

    /// Create a build started event.
    pub fn started(profile: impl Into<String>, target_triple: impl Into<String>) -> Self {
        BuildEvent::BuildStarted {
            profile: profile.into(),
            target_triple: target_triple.into(),
            package_count: None,
        }
    }

    /// Serialize this event to a JSON string.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Serialize this event to a pretty JSON string.
    pub fn to_json_pretty(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_artifact_serialization() {
        let event = BuildEvent::artifact(
            "zlib v1.3.1",
            "zlib",
            vec![PathBuf::from("target/debug/libzlib.a")],
        );
        let json = event.to_json();
        assert!(json.contains("\"reason\":\"compiler-artifact\""));
        assert!(json.contains("\"package_id\":\"zlib v1.3.1\""));
        assert!(json.contains("libzlib.a"));
    }

    #[test]
    fn test_finished_serialization() {
        let event = BuildEvent::finished(true, 2340);
        let json = event.to_json();
        assert!(json.contains("\"reason\":\"build-finished\""));
        assert!(json.contains("\"success\":true"));
        assert!(json.contains("\"duration_ms\":2340"));
    }

    #[test]
    fn test_progress_serialization() {
        let event = BuildEvent::progress(5, 10, "files");
        let json = event.to_json();
        assert!(json.contains("\"reason\":\"build-progress\""));
        assert!(json.contains("\"current\":5"));
        assert!(json.contains("\"total\":10"));
        assert!(json.contains("\"unit\":\"files\""));
    }

    #[test]
    fn test_error_with_location() {
        let event = BuildEvent::CompilerError {
            package_id: "myapp v1.0.0".to_string(),
            message: "undefined reference to 'foo'".to_string(),
            file: Some(PathBuf::from("src/main.c")),
            line: Some(42),
            column: Some(10),
        };
        let json = event.to_json();
        assert!(json.contains("\"reason\":\"compiler-error\""));
        assert!(json.contains("\"line\":42"));
        assert!(json.contains("\"column\":10"));
    }
}
