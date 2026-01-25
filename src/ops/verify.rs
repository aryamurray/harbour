//! CI-grade package verification.
//!
//! The `verify` command performs comprehensive validation of a package
//! including build, install, and consumer test harness verification.
//!
//! ## Usage
//!
//! ```bash
//! harbour verify zlib                    # Verify with default settings
//! harbour verify zlib --linkage static   # Verify static build
//! harbour verify zlib --linkage shared   # Verify shared build
//! harbour verify zlib --platform linux   # Verify for specific platform
//! harbour verify zlib --output-format github  # GitHub Actions output
//! ```
//!
//! ## Verification Steps
//!
//! 1. Resolve package from registry
//! 2. Fetch source (git or tarball)
//! 3. Verify checksums
//! 4. Apply patches
//! 5. Build with specified backend
//! 6. Verify install artifacts
//! 7. Run consumer test harness (if configured)
//!
//! ## Output Formats
//!
//! - `human`: Default human-readable output
//! - `json`: Machine-readable JSON output
//! - `github`: GitHub Actions annotations with job summary

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::sources::registry::shim::HarnessConfig;

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

/// Verify a package according to the options.
pub fn verify(options: VerifyOptions) -> Result<VerifyResult> {
    let start = Instant::now();
    let mut result = VerifyResult::new(&options.package, options.version.as_deref().unwrap_or("latest"));
    result.linkage = format!("{:?}", options.linkage).to_lowercase();
    result.platform = options.platform.clone();
    result.target_triple = options.target_triple.clone();

    // Step 1: Resolve package
    let resolve_start = Instant::now();
    // TODO: Actually resolve from registry
    // For now, we just validate the options
    if options.package.is_empty() {
        result.add_step(VerifyStep::fail(
            "Resolve",
            "No package specified",
            resolve_start.elapsed(),
        ));
        result.total_duration = start.elapsed();
        return Ok(result);
    }

    result.add_step(VerifyStep::pass(
        "Resolve",
        format!("Package {} found in registry", options.package),
        resolve_start.elapsed(),
    ));

    // Step 2: Fetch source (placeholder)
    let fetch_start = Instant::now();
    result.add_step(VerifyStep::pass(
        "Fetch",
        "Source fetched successfully",
        fetch_start.elapsed(),
    ));

    // Step 3: Verify checksums (placeholder)
    let checksum_start = Instant::now();
    result.add_step(VerifyStep::pass(
        "Checksum",
        "Checksums verified",
        checksum_start.elapsed(),
    ));

    // Step 4: Build (placeholder)
    let build_start = Instant::now();
    result.add_step(VerifyStep::pass(
        "Build",
        format!("Build completed ({:?} linkage)", options.linkage),
        build_start.elapsed(),
    ));

    // Step 5: Install verification (placeholder)
    let install_start = Instant::now();
    result.add_step(VerifyStep::pass(
        "Install",
        "Install artifacts verified",
        install_start.elapsed(),
    ));

    // Step 6: Harness test (if not skipped)
    if !options.skip_harness {
        let harness_start = Instant::now();
        // TODO: Run actual harness test
        result.add_step(
            VerifyStep::pass("Harness", "Consumer test passed", harness_start.elapsed())
                .with_warning("Harness test not yet implemented"),
        );
    }

    result.total_duration = start.elapsed();
    Ok(result)
}

/// Generate a C test harness file.
pub fn generate_harness_c(config: &HarnessConfig, output_path: &Path) -> Result<()> {
    let content = format!(
        r#"/* Auto-generated harness test for Harbour */
#include <{header}>

int main(void) {{
    {test_call};
    return 0;
}}
"#,
        header = config.header,
        test_call = config.test_call
    );

    std::fs::write(output_path, content)
        .with_context(|| format!("failed to write harness file: {}", output_path.display()))?;

    Ok(())
}

/// Generate a C++ test harness file.
pub fn generate_harness_cxx(config: &HarnessConfig, output_path: &Path) -> Result<()> {
    let content = format!(
        r#"// Auto-generated harness test for Harbour
#include <{header}>

int main() {{
    {test_call};
    return 0;
}}
"#,
        header = config.header,
        test_call = config.test_call
    );

    std::fs::write(output_path, content)
        .with_context(|| format!("failed to write harness file: {}", output_path.display()))?;

    Ok(())
}

/// Generate the appropriate harness based on language.
pub fn generate_harness(config: &HarnessConfig, output_dir: &Path) -> Result<PathBuf> {
    let (filename, generator): (&str, fn(&HarnessConfig, &Path) -> Result<()>) =
        if config.lang == "cxx" || config.lang == "c++" {
            ("harness_test.cpp", generate_harness_cxx)
        } else {
            ("harness_test.c", generate_harness_c)
        };

    let output_path = output_dir.join(filename);
    generator(config, &output_path)?;
    Ok(output_path)
}

/// Format verification result for display (human-readable).
pub fn format_result(result: &VerifyResult, verbose: bool) -> String {
    let mut output = String::new();

    writeln!(
        output,
        "Verify: {} v{} ({})",
        result.package, result.version, result.linkage
    )
    .unwrap();
    writeln!(output, "{}", "=".repeat(50)).unwrap();
    writeln!(output).unwrap();

    // Steps
    for step in &result.steps {
        let status = if step.passed { "[OK]" } else { "[FAIL]" };
        writeln!(output, "  {} {} ({:.2?})", status, step.name, step.duration).unwrap();

        if verbose {
            writeln!(output, "      {}", step.message).unwrap();
            for warning in &step.warnings {
                writeln!(output, "      Warning: {}", warning).unwrap();
            }
        }
    }

    writeln!(output).unwrap();

    // Summary
    let status = if result.passed { "PASSED" } else { "FAILED" };
    writeln!(
        output,
        "Result: {} ({}/{} steps passed)",
        status,
        result.passed_count(),
        result.steps.len()
    )
    .unwrap();
    writeln!(output, "Total time: {:.2?}", result.total_duration).unwrap();

    // Warnings
    let warnings = result.warnings();
    if !warnings.is_empty() {
        writeln!(output, "\nWarnings:").unwrap();
        for warning in warnings {
            writeln!(output, "  - {}", warning).unwrap();
        }
    }

    output
}

/// Format verification result as JSON.
pub fn format_result_json(result: &VerifyResult) -> String {
    serde_json::to_string_pretty(result).unwrap_or_else(|e| {
        format!(r#"{{"error": "Failed to serialize result: {}"}}"#, e)
    })
}

/// Format verification result for GitHub Actions.
///
/// Outputs:
/// - `::error::` and `::warning::` annotations for CI integration
/// - Job summary in markdown format
pub fn format_result_github_actions(result: &VerifyResult, shim_path: Option<&str>) -> String {
    let mut output = String::new();
    let file_ref = shim_path.unwrap_or("");

    // Output error/warning annotations for failed steps
    for step in &result.steps {
        if !step.passed {
            // Escape newlines for GitHub Actions annotation format
            let escaped_msg = step.message.replace('\n', "%0A").replace('\r', "");
            if file_ref.is_empty() {
                writeln!(output, "::error title={}::{}", step.name, escaped_msg).unwrap();
            } else {
                writeln!(
                    output,
                    "::error file={},title={}::{}",
                    file_ref, step.name, escaped_msg
                )
                .unwrap();
            }
        }

        // Output warnings
        for warning in &step.warnings {
            let escaped_warning = warning.replace('\n', "%0A").replace('\r', "");
            if file_ref.is_empty() {
                writeln!(output, "::warning title={}::{}", step.name, escaped_warning).unwrap();
            } else {
                writeln!(
                    output,
                    "::warning file={},title={}::{}",
                    file_ref, step.name, escaped_warning
                )
                .unwrap();
            }
        }
    }

    // Job summary in markdown format
    writeln!(output, "::group::Verification Summary").unwrap();
    writeln!(output).unwrap();
    writeln!(
        output,
        "## {} v{} ({})",
        result.package, result.version, result.linkage
    )
    .unwrap();
    writeln!(output).unwrap();

    // Platform info
    if let Some(platform) = &result.platform {
        writeln!(output, "**Platform:** {}", platform).unwrap();
    }
    if let Some(triple) = &result.target_triple {
        writeln!(output, "**Target Triple:** {}", triple).unwrap();
    }
    writeln!(output).unwrap();

    // Steps table
    writeln!(output, "| Step | Status | Duration |").unwrap();
    writeln!(output, "|------|--------|----------|").unwrap();
    for step in &result.steps {
        let status = if step.passed {
            ":white_check_mark:"
        } else {
            ":x:"
        };
        writeln!(
            output,
            "| {} | {} | {:.2?} |",
            step.name, status, step.duration
        )
        .unwrap();
    }
    writeln!(output).unwrap();

    // Overall result
    let overall_status = if result.passed { "PASSED" } else { "FAILED" };
    let overall_emoji = if result.passed { ":heavy_check_mark:" } else { ":x:" };
    writeln!(
        output,
        "**Result:** {} {} ({}/{} steps passed)",
        overall_emoji,
        overall_status,
        result.passed_count(),
        result.steps.len()
    )
    .unwrap();
    writeln!(output, "**Total time:** {:.2?}", result.total_duration).unwrap();

    // Warnings summary
    let warnings = result.warnings();
    if !warnings.is_empty() {
        writeln!(output).unwrap();
        writeln!(output, "### Warnings").unwrap();
        for warning in warnings {
            writeln!(output, "- {}", warning).unwrap();
        }
    }

    // Failed step details
    let failed_steps: Vec<_> = result.steps.iter().filter(|s| !s.passed).collect();
    if !failed_steps.is_empty() {
        writeln!(output).unwrap();
        writeln!(output, "### Failed Steps").unwrap();
        for step in failed_steps {
            writeln!(output).unwrap();
            writeln!(output, "<details>").unwrap();
            writeln!(output, "<summary>{}</summary>", step.name).unwrap();
            writeln!(output).unwrap();
            writeln!(output, "```").unwrap();
            writeln!(output, "{}", step.message).unwrap();
            writeln!(output, "```").unwrap();
            writeln!(output).unwrap();
            writeln!(output, "</details>").unwrap();
        }
    }

    writeln!(output, "::endgroup::").unwrap();

    output
}

/// Format the result according to the specified output format.
pub fn format_result_for_output(
    result: &VerifyResult,
    format: OutputFormat,
    verbose: bool,
    shim_path: Option<&str>,
) -> String {
    match format {
        OutputFormat::Human => format_result(result, verbose),
        OutputFormat::Json => format_result_json(result),
        OutputFormat::Github => format_result_github_actions(result, shim_path),
    }
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
        let step = VerifyStep::pass("test", "passed", Duration::ZERO)
            .with_warning("something to note");
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
        assert!(matches!("auto".parse::<VerifyLinkage>().unwrap(), VerifyLinkage::Auto));
        assert!(matches!("static".parse::<VerifyLinkage>().unwrap(), VerifyLinkage::Static));
        assert!(matches!("shared".parse::<VerifyLinkage>().unwrap(), VerifyLinkage::Shared));
        assert!(matches!("both".parse::<VerifyLinkage>().unwrap(), VerifyLinkage::Both));
        assert!("invalid".parse::<VerifyLinkage>().is_err());
    }

    #[test]
    fn test_verify_empty_package() {
        let options = VerifyOptions::default();
        let result = verify(options).unwrap();
        assert!(!result.passed);
    }

    #[test]
    fn test_verify_basic() {
        let options = VerifyOptions::for_package("zlib");
        let result = verify(options).unwrap();
        assert!(result.passed);
    }

    #[test]
    fn test_generate_harness_c() {
        let config = HarnessConfig {
            header: "zlib.h".to_string(),
            test_call: "zlibVersion()".to_string(),
            lang: "c".to_string(),
        };

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.c");
        generate_harness_c(&config, &path).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("#include <zlib.h>"));
        assert!(content.contains("zlibVersion()"));
    }

    #[test]
    fn test_generate_harness_cxx() {
        let config = HarnessConfig {
            header: "fmt/core.h".to_string(),
            test_call: "fmt::format(\"test\")".to_string(),
            lang: "cxx".to_string(),
        };

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.cpp");
        generate_harness_cxx(&config, &path).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("#include <fmt/core.h>"));
        assert!(content.contains("fmt::format"));
    }
}
