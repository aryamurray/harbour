//! Public types and enums for the verify module.

use std::path::PathBuf;
use std::time::Duration;

use serde::Serialize;

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

    /// Whether the step passed (a skipped step has not failed, so it counts)
    pub passed: bool,

    /// `passed`, `failed` or `skipped`: what a CI system keys on, since
    /// `passed` alone cannot say that a stage did not run.
    pub status: &'static str,

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
            status: "passed",
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
            status: "failed",
            message: message.into(),
            duration,
            warnings: Vec::new(),
        }
    }

    /// Create a step that did not run, and says why.
    pub fn skipped(name: impl Into<String>, message: impl Into<String>) -> Self {
        VerifyStep {
            name: name.into(),
            passed: true,
            status: "skipped",
            message: message.into(),
            duration: Duration::ZERO,
            warnings: Vec::new(),
        }
    }

    /// Add a warning to the step.
    pub fn with_warning(mut self, warning: impl Into<String>) -> Self {
        self.warnings.push(warning.into());
        self
    }
}

/// The first stage that failed, and why.
#[derive(Debug, Clone, Serialize)]
pub struct Failure {
    /// Stage name (`Resolve`, `Fetch`, `Build`, `Artifacts` or `Harness`)
    pub stage: String,
    /// The stage's message: the compiler's or linker's own words where there are any
    pub reason: String,
}

/// The translation units a package compiled.
#[derive(Debug, Clone, Default, Serialize)]
pub struct TranslationUnits {
    /// How many there are
    pub count: usize,
    /// Their paths, relative to the package's source tree, as the lockfile
    /// records them
    pub files: Vec<String>,
}

/// What a build compiled, as `harbour build` counts it.
#[derive(Debug, Clone, Default, Serialize)]
pub struct BuildStats {
    /// Source files compiled, across the whole graph
    pub compiled: usize,
    /// Source files that were already up to date
    pub up_to_date: usize,
}

/// A public header the default harness could not use, and the compiler's
/// first complaint about it.
#[derive(Debug, Clone, Serialize)]
pub struct RejectedHeader {
    pub header: String,
    /// `umbrella` (the header itself says to include another one instead, so
    /// it was not tried alone) or `compile` (it was tried and did not compile)
    pub kind: &'static str,
    pub error: String,
}

/// Which harness verification ran, and what it covered.
#[derive(Debug, Clone, Serialize)]
pub struct HarnessReport {
    /// `declared` (the shim's `[metadata.harness]`), `default` (public headers
    /// plus a link) or `none` (it did not run)
    pub level: &'static str,
    /// Language the harness was compiled as
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
    /// Headers the harness included
    pub headers: Vec<String>,
    /// Public headers that could not be included on their own
    pub rejected_headers: Vec<RejectedHeader>,
    /// A symbol from the library that the harness referenced, so that linking
    /// had to pull the archive in
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link_symbol: Option<String>,
    /// Whether the harness was executed (it is only linked when cross-compiling)
    pub executed: bool,
}

impl Default for HarnessReport {
    fn default() -> Self {
        HarnessReport {
            level: "none",
            lang: None,
            headers: Vec::new(),
            rejected_headers: Vec::new(),
            link_symbol: None,
            executed: false,
        }
    }
}

impl HarnessReport {
    /// How the report names the level to a person.
    pub fn describe(&self) -> &'static str {
        match self.level {
            "declared" => "declared",
            "default" => "default (headers + link)",
            _ => "none",
        }
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

    /// The triple the package was built for: `--target-triple`, else the
    /// configured or host one. Always present, so a CI log says what was
    /// verified even when nothing was asked for.
    pub triple: String,

    /// Individual step results
    pub steps: Vec<VerifyStep>,

    /// The translation units the package itself compiled (its dependencies'
    /// are not counted here).
    pub translation_units: TranslationUnits,

    /// What the build as a whole compiled, dependencies included.
    pub build: BuildStats,

    /// Which harness ran and what it covered.
    pub harness: HarnessReport,

    /// Why verification failed, if it did: the first failing stage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<Failure>,

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
            triple: String::new(),
            steps: Vec::new(),
            translation_units: TranslationUnits::default(),
            build: BuildStats::default(),
            harness: HarnessReport::default(),
            failure: None,
            total_duration: Duration::ZERO,
            artifacts: Vec::new(),
            passed: true,
        }
    }

    /// Add a step result.
    pub fn add_step(&mut self, step: VerifyStep) {
        if !step.passed {
            self.passed = false;
            if self.failure.is_none() {
                self.failure = Some(Failure {
                    stage: step.name.clone(),
                    reason: step.message.clone(),
                });
            }
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

    /// Require every source pattern to match a file (`--strict-sources`)
    pub strict_sources: bool,
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
            strict_sources: false,
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
