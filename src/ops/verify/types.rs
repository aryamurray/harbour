//! Public types and enums for the verify module.

use std::path::PathBuf;
use std::time::Duration;

use serde::Serialize;

use crate::sources::registry::Shim;

/// Output format for verification results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputFormat {
    /// Human-readable output (default)
    #[default]
    Human,
    /// Machine-readable JSON output
    Json,
    /// GitHub Actions annotations with job summary
    Github,
}

impl std::str::FromStr for OutputFormat {
    type Err = OutputFormatParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "human" => Ok(OutputFormat::Human),
            "json" => Ok(OutputFormat::Json),
            "github" | "github-actions" | "gha" => Ok(OutputFormat::Github),
            _ => Err(OutputFormatParseError(s.to_string())),
        }
    }
}

/// Error parsing output format option.
#[derive(Debug, Clone)]
pub struct OutputFormatParseError(pub String);

impl std::fmt::Display for OutputFormatParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "invalid output format '{}', valid values: human, json, github",
            self.0
        )
    }
}

impl std::error::Error for OutputFormatParseError {}

/// Result of a verification step.
#[derive(Debug, Clone, Serialize)]
pub struct VerifyStep {
    /// Step name
    pub name: String,

    /// Whether the step passed
    pub passed: bool,

    /// Status message
    pub message: String,

    /// How long the step took (in milliseconds for JSON)
    #[serde(serialize_with = "serialize_duration_ms")]
    pub duration: Duration,

    /// Any warnings (passed but with issues)
    pub warnings: Vec<String>,
}

fn serialize_duration_ms<S>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_u64(duration.as_millis() as u64)
}

impl VerifyStep {
    /// Create a passing step.
    pub fn pass(name: impl Into<String>, message: impl Into<String>, duration: Duration) -> Self {
        VerifyStep {
            name: name.into(),
            passed: true,
            message: message.into(),
            duration,
            warnings: Vec::new(),
        }
    }

    /// Create a failing step.
    pub fn fail(name: impl Into<String>, message: impl Into<String>, duration: Duration) -> Self {
        VerifyStep {
            name: name.into(),
            passed: false,
            message: message.into(),
            duration,
            warnings: Vec::new(),
        }
    }

    /// Add a warning to the step.
    pub fn with_warning(mut self, warning: impl Into<String>) -> Self {
        self.warnings.push(warning.into());
        self
    }
}

/// Complete verification result.
#[derive(Debug, Clone, Serialize)]
pub struct VerifyResult {
    /// Package name
    pub package: String,

    /// Package version
    pub version: String,

    /// Linkage used
    pub linkage: String,

    /// Target platform
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,

    /// Target triple (for cross-compilation)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_triple: Option<String>,

    /// Individual step results
    pub steps: Vec<VerifyStep>,

    /// Total verification time (in milliseconds for JSON)
    #[serde(serialize_with = "serialize_duration_ms")]
    pub total_duration: Duration,

    /// Build artifacts produced
    pub artifacts: Vec<PathBuf>,

    /// Whether verification passed overall
    pub passed: bool,
}

impl VerifyResult {
    /// Create a new verify result.
    pub fn new(package: impl Into<String>, version: impl Into<String>) -> Self {
        VerifyResult {
            package: package.into(),
            version: version.into(),
            linkage: "auto".to_string(),
            platform: None,
            target_triple: None,
            steps: Vec::new(),
            total_duration: Duration::ZERO,
            artifacts: Vec::new(),
            passed: true,
        }
    }

    /// Add a step result.
    pub fn add_step(&mut self, step: VerifyStep) {
        if !step.passed {
            self.passed = false;
        }
        self.steps.push(step);
    }

    /// Get count of passed steps.
    pub fn passed_count(&self) -> usize {
        self.steps.iter().filter(|s| s.passed).count()
    }

    /// Get count of failed steps.
    pub fn failed_count(&self) -> usize {
        self.steps.iter().filter(|s| !s.passed).count()
    }

    /// Get all warnings.
    pub fn warnings(&self) -> Vec<&str> {
        self.steps
            .iter()
            .flat_map(|s| s.warnings.iter().map(|w| w.as_str()))
            .collect()
    }
}

/// Options for the verify command.
#[derive(Debug, Clone)]
pub struct VerifyOptions {
    /// Package name to verify
    pub package: String,

    /// Package version (optional, uses latest if not specified)
    pub version: Option<String>,

    /// Linkage to test (static, shared, or both)
    pub linkage: VerifyLinkage,

    /// Target platform (uses current if not specified)
    pub platform: Option<String>,

    /// Output directory for build artifacts
    pub output_dir: Option<PathBuf>,

    /// Skip harness test
    pub skip_harness: bool,

    /// Verbose output
    pub verbose: bool,

    /// Output format (human, json, github)
    pub output_format: OutputFormat,

    /// Target triple for cross-compilation
    pub target_triple: Option<String>,

    /// Path to local registry (for CI verification)
    pub registry_path: Option<PathBuf>,
}

impl Default for VerifyOptions {
    fn default() -> Self {
        VerifyOptions {
            package: String::new(),
            version: None,
            linkage: VerifyLinkage::Auto,
            platform: None,
            output_dir: None,
            skip_harness: false,
            verbose: false,
            output_format: OutputFormat::Human,
            target_triple: None,
            registry_path: None,
        }
    }
}

impl VerifyOptions {
    /// Create options for a specific package.
    pub fn for_package(package: impl Into<String>) -> Self {
        VerifyOptions {
            package: package.into(),
            ..Default::default()
        }
    }
}

/// Linkage to verify.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyLinkage {
    /// Let the package decide
    Auto,
    /// Verify static build only
    Static,
    /// Verify shared build only
    Shared,
    /// Verify both static and shared
    Both,
}

impl std::str::FromStr for VerifyLinkage {
    type Err = VerifyLinkageParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "auto" => Ok(VerifyLinkage::Auto),
            "static" => Ok(VerifyLinkage::Static),
            "shared" | "dynamic" => Ok(VerifyLinkage::Shared),
            "both" | "all" => Ok(VerifyLinkage::Both),
            _ => Err(VerifyLinkageParseError(s.to_string())),
        }
    }
}

/// Error parsing linkage option.
#[derive(Debug, Clone)]
pub struct VerifyLinkageParseError(pub String);

impl std::fmt::Display for VerifyLinkageParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "invalid verify linkage '{}', valid values: auto, static, shared, both",
            self.0
        )
    }
}

impl std::error::Error for VerifyLinkageParseError {}

/// Internal context for verification.
pub(crate) struct VerifyContext {
    /// Temporary directory for verification
    pub temp_dir: tempfile::TempDir,
    /// Cache directory for sources
    pub cache_dir: PathBuf,
    /// The loaded shim
    pub shim: Shim,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_verify_step_pass() {
        let step = VerifyStep::pass("test", "passed", Duration::from_millis(100));
        assert!(step.passed);
        assert_eq!(step.name, "test");
    }

    #[test]
    fn test_verify_step_with_warning() {
        let step =
            VerifyStep::pass("test", "passed", Duration::ZERO).with_warning("something to note");
        assert!(step.passed);
        assert_eq!(step.warnings.len(), 1);
    }

    #[test]
    fn test_verify_result() {
        let mut result = VerifyResult::new("zlib", "1.3.1");
        result.add_step(VerifyStep::pass("step1", "ok", Duration::ZERO));
        result.add_step(VerifyStep::pass("step2", "ok", Duration::ZERO));

        assert!(result.passed);
        assert_eq!(result.passed_count(), 2);
        assert_eq!(result.failed_count(), 0);
    }

    #[test]
    fn test_verify_result_failed() {
        let mut result = VerifyResult::new("zlib", "1.3.1");
        result.add_step(VerifyStep::pass("step1", "ok", Duration::ZERO));
        result.add_step(VerifyStep::fail("step2", "error", Duration::ZERO));

        assert!(!result.passed);
        assert_eq!(result.passed_count(), 1);
        assert_eq!(result.failed_count(), 1);
    }

    #[test]
    fn test_verify_linkage_parse() {
        assert!(matches!(
            "auto".parse::<VerifyLinkage>().unwrap(),
            VerifyLinkage::Auto
        ));
        assert!(matches!(
            "static".parse::<VerifyLinkage>().unwrap(),
            VerifyLinkage::Static
        ));
        assert!(matches!(
            "shared".parse::<VerifyLinkage>().unwrap(),
            VerifyLinkage::Shared
        ));
        assert!(matches!(
            "both".parse::<VerifyLinkage>().unwrap(),
            VerifyLinkage::Both
        ));
        assert!("invalid".parse::<VerifyLinkage>().is_err());
    }
}
