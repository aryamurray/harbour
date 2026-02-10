//! Environment and toolchain health checks.
//!
//! The `doctor` command performs fast environment checks to verify
//! that all required tools are available and properly configured.
//!
//! ## Usage
//!
//! ```bash
//! harbour doctor           # Quick check
//! harbour doctor --verbose # Detailed output
//! ```
//!
//! ## Checks Performed
//!
//! - C/C++ compiler availability (cc, c++, clang, gcc, cl)
//! - Archiver availability (ar, lib)
//! - Build tools (cmake, ninja, make)
//! - Package tools (pkg-config, pkgconf)
//! - Git availability
//! - Network connectivity (optional)

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::core::abi::TargetTriple;
use crate::util::config::load_config;
use crate::util::{GlobalContext, VcpkgIntegration};

/// Result of a single health check.
#[derive(Debug, Clone)]
pub struct CheckResult {
    /// Name of the check
    pub name: String,

    /// Whether the check passed
    pub passed: bool,

    /// Human-readable status message
    pub message: String,

    /// Path to the tool (if applicable)
    pub path: Option<PathBuf>,

    /// Version string (if applicable)
    pub version: Option<String>,

    /// How long the check took
    pub duration: Duration,

    /// Whether this check is required or optional
    pub required: bool,
}

impl CheckResult {
    /// Create a passing check result.
    pub fn pass(name: impl Into<String>, message: impl Into<String>) -> Self {
        CheckResult {
            name: name.into(),
            passed: true,
            message: message.into(),
            path: None,
            version: None,
            duration: Duration::ZERO,
            required: true,
        }
    }

    /// Create a failing check result.
    pub fn fail(name: impl Into<String>, message: impl Into<String>) -> Self {
        CheckResult {
            name: name.into(),
            passed: false,
            message: message.into(),
            path: None,
            version: None,
            duration: Duration::ZERO,
            required: true,
        }
    }

    /// Mark this check as optional.
    pub fn optional(mut self) -> Self {
        self.required = false;
        self
    }

    /// Set the tool path.
    pub fn with_path(mut self, path: PathBuf) -> Self {
        self.path = Some(path);
        self
    }

    /// Set the version.
    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = Some(version.into());
        self
    }

    /// Set the duration.
    pub fn with_duration(mut self, duration: Duration) -> Self {
        self.duration = duration;
        self
    }
}

/// Summary of all health checks.
#[derive(Debug, Clone)]
pub struct DoctorReport {
    /// Individual check results
    pub checks: Vec<CheckResult>,

    /// Total time taken
    pub total_duration: Duration,

    /// Environment information
    pub environment: HashMap<String, String>,
}

impl DoctorReport {
    /// Create a new empty report.
    pub fn new() -> Self {
        DoctorReport {
            checks: Vec::new(),
            total_duration: Duration::ZERO,
            environment: HashMap::new(),
        }
    }

    /// Add a check result.
    pub fn add(&mut self, check: CheckResult) {
        self.checks.push(check);
    }

    /// Check if all required checks passed.
    pub fn all_required_passed(&self) -> bool {
        self.checks.iter().filter(|c| c.required).all(|c| c.passed)
    }

    /// Get the count of passed checks.
    pub fn passed_count(&self) -> usize {
        self.checks.iter().filter(|c| c.passed).count()
    }

    /// Get the count of failed checks.
    pub fn failed_count(&self) -> usize {
        self.checks.iter().filter(|c| !c.passed).count()
    }

    /// Get the count of required failed checks.
    pub fn required_failed_count(&self) -> usize {
        self.checks
            .iter()
            .filter(|c| c.required && !c.passed)
            .count()
    }
}

impl Default for DoctorReport {
    fn default() -> Self {
        Self::new()
    }
}

/// Options for the doctor command.
#[derive(Debug, Clone, Default)]
pub struct DoctorOptions {
    /// Include verbose output
    pub verbose: bool,

    /// Skip network checks
    pub offline: bool,
}

/// Run the doctor command.
pub fn doctor(_options: DoctorOptions) -> Result<DoctorReport> {
    let start = Instant::now();
    let mut report = DoctorReport::new();

    let ctx = GlobalContext::new()?;
    let config = load_config(
        &ctx.config_path(),
        &ctx.project_harbour_dir().join("config.toml"),
    );
    let target = TargetTriple::host();

    // Collect environment info
    report
        .environment
        .insert("os".to_string(), std::env::consts::OS.to_string());
    report
        .environment
        .insert("arch".to_string(), std::env::consts::ARCH.to_string());

    // Check C compiler
    report.add(check_c_compiler());

    // Check C++ compiler
    report.add(check_cxx_compiler());

    // Check archiver
    report.add(check_archiver());

    // Check CMake
    report.add(check_cmake());

    // Check build tool (ninja or make)
    report.add(check_build_tool());

    // Check pkg-config
    report.add(check_pkg_config());

    // Check git
    report.add(check_git());

    // Check vcpkg integration
    report.add(check_vcpkg(&config.vcpkg, &target));

    report.total_duration = start.elapsed();
    Ok(report)
}

/// Check for C compiler.
fn check_c_compiler() -> CheckResult {
    let start = Instant::now();

    // Try different compilers in order
    let compilers = if cfg!(windows) {
        vec!["cl", "clang", "gcc", "cc"]
    } else {
        vec!["cc", "clang", "gcc"]
    };

    for compiler in compilers {
        if let Some(result) = try_compiler(compiler, &["-v", "--version"]) {
            return CheckResult::pass("C Compiler", format!("Found {}", compiler))
                .with_path(result.0)
                .with_version(result.1)
                .with_duration(start.elapsed());
        }
    }

    CheckResult::fail("C Compiler", "No C compiler found (tried cc, clang, gcc)")
        .with_duration(start.elapsed())
}

/// Check for C++ compiler.
fn check_cxx_compiler() -> CheckResult {
    let start = Instant::now();

    let compilers = if cfg!(windows) {
        vec!["cl", "clang++", "g++", "c++"]
    } else {
        vec!["c++", "clang++", "g++"]
    };

    for compiler in compilers {
        if let Some(result) = try_compiler(compiler, &["-v", "--version"]) {
            return CheckResult::pass("C++ Compiler", format!("Found {}", compiler))
                .with_path(result.0)
                .with_version(result.1)
                .with_duration(start.elapsed());
        }
    }

    CheckResult::fail(
        "C++ Compiler",
        "No C++ compiler found (tried c++, clang++, g++)",
    )
    .with_duration(start.elapsed())
}

/// Check for archiver.
fn check_archiver() -> CheckResult {
    let start = Instant::now();

    let archivers = if cfg!(windows) {
        vec!["lib", "ar", "llvm-ar"]
    } else {
        vec!["ar", "llvm-ar"]
    };

    for archiver in archivers {
        if let Ok(output) = Command::new(archiver).arg("--version").output() {
            if output.status.success() || !output.stderr.is_empty() {
                let version = String::from_utf8_lossy(&output.stdout);
                return CheckResult::pass("Archiver", format!("Found {}", archiver))
                    .with_version(version.lines().next().unwrap_or("").trim().to_string())
                    .with_duration(start.elapsed());
            }
        }
        // ar without --version just returns usage, check if it exists
        if let Ok(path) = which::which(archiver) {
            return CheckResult::pass("Archiver", format!("Found {}", archiver))
                .with_path(path)
                .with_duration(start.elapsed());
        }
    }

    CheckResult::fail("Archiver", "No archiver found (tried ar, lib)")
        .with_duration(start.elapsed())
}

/// Check for CMake.
fn check_cmake() -> CheckResult {
    let start = Instant::now();

    if let Ok(output) = Command::new("cmake").arg("--version").output() {
        if output.status.success() {
            let version_output = String::from_utf8_lossy(&output.stdout);
            let version = version_output
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string();

            if let Ok(path) = which::which("cmake") {
                return CheckResult::pass("CMake", "CMake is available")
                    .with_path(path)
                    .with_version(version)
                    .with_duration(start.elapsed())
                    .optional();
            }
        }
    }

    CheckResult::fail(
        "CMake",
        "CMake not found (optional, needed for some packages)",
    )
    .with_duration(start.elapsed())
    .optional()
}

/// Check for build tool (ninja or make).
fn check_build_tool() -> CheckResult {
    let start = Instant::now();

    // Prefer ninja
    if let Ok(output) = Command::new("ninja").arg("--version").output() {
        if output.status.success() {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if let Ok(path) = which::which("ninja") {
                return CheckResult::pass("Build Tool", "Ninja is available")
                    .with_path(path)
                    .with_version(version)
                    .with_duration(start.elapsed())
                    .optional();
            }
        }
    }

    // Fall back to make
    let make_cmd = if cfg!(windows) { "nmake" } else { "make" };
    if let Ok(output) = Command::new(make_cmd).arg("--version").output() {
        if output.status.success() {
            let version = String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            if let Ok(path) = which::which(make_cmd) {
                return CheckResult::pass("Build Tool", format!("{} is available", make_cmd))
                    .with_path(path)
                    .with_version(version)
                    .with_duration(start.elapsed())
                    .optional();
            }
        }
    }

    CheckResult::fail(
        "Build Tool",
        "No build tool found (ninja or make recommended)",
    )
    .with_duration(start.elapsed())
    .optional()
}

/// Check for pkg-config.
fn check_pkg_config() -> CheckResult {
    let start = Instant::now();

    let tools = vec!["pkg-config", "pkgconf"];

    for tool in tools {
        if let Ok(output) = Command::new(tool).arg("--version").output() {
            if output.status.success() {
                let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if let Ok(path) = which::which(tool) {
                    return CheckResult::pass("pkg-config", format!("{} is available", tool))
                        .with_path(path)
                        .with_version(version)
                        .with_duration(start.elapsed())
                        .optional();
                }
            }
        }
    }

    CheckResult::fail(
        "pkg-config",
        "pkg-config not found (optional, helps with system libraries)",
    )
    .with_duration(start.elapsed())
    .optional()
}

/// Check for git.
fn check_git() -> CheckResult {
    let start = Instant::now();

    if let Ok(output) = Command::new("git").arg("--version").output() {
        if output.status.success() {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if let Ok(path) = which::which("git") {
                return CheckResult::pass("Git", "Git is available")
                    .with_path(path)
                    .with_version(version)
                    .with_duration(start.elapsed());
            }
        }
    }

    CheckResult::fail("Git", "Git not found (required for fetching packages)")
        .with_duration(start.elapsed())
}

/// Check for vcpkg integration.
fn check_vcpkg(config: &crate::util::config::VcpkgConfig, target: &TargetTriple) -> CheckResult {
    let start = Instant::now();

    let enabled = match config.enabled {
        Some(value) => value,
        None => std::env::var_os("VCPKG_ROOT").is_some(),
    };

    if !enabled {
        return CheckResult::pass("vcpkg", "vcpkg integration disabled")
            .with_duration(start.elapsed())
            .optional();
    }

    match VcpkgIntegration::from_config(config, target, true) {
        Some(integration) => {
            let binary_exists = integration.vcpkg_binary().exists();
            let installed_ports = integration.list_installed_ports();
            let port_count = installed_ports.len();

            let mut details = format!(
                "Found at {} (triplet: {}, {} installed packages)",
                integration.root.display(),
                integration.triplet,
                port_count
            );

            if let Some(ref baseline) = integration.baseline {
                let short_baseline = if baseline.len() > 8 {
                    &baseline[..8]
                } else {
                    baseline
                };
                details.push_str(&format!(", baseline: {}", short_baseline));
            }

            if integration.has_custom_registries {
                details.push_str(", custom registries configured");
            }

            if !binary_exists {
                return CheckResult::fail(
                    "vcpkg",
                    format!(
                        "vcpkg binary not found at {}. {}",
                        integration.vcpkg_binary().display(),
                        diagnose_vcpkg_missing()
                    ),
                )
                .with_path(integration.root)
                .with_duration(start.elapsed())
                .optional();
            }

            CheckResult::pass("vcpkg", details)
                .with_path(integration.root)
                .with_duration(start.elapsed())
                .optional()
        }
        None => CheckResult::fail(
            "vcpkg",
            format!(
                "vcpkg enabled but configuration failed. {}",
                diagnose_vcpkg_config_error(config)
            ),
        )
        .with_duration(start.elapsed())
        .optional(),
    }
}

/// Provide diagnostic message for missing vcpkg binary.
fn diagnose_vcpkg_missing() -> &'static str {
    "Run 'git clone https://github.com/microsoft/vcpkg' and './vcpkg/bootstrap-vcpkg.sh' (or .bat on Windows)"
}

/// Provide diagnostic message for vcpkg configuration errors.
fn diagnose_vcpkg_config_error(config: &crate::util::config::VcpkgConfig) -> String {
    let mut hints = Vec::new();

    if config.root.is_none() && std::env::var_os("VCPKG_ROOT").is_none() {
        hints.push("Set VCPKG_ROOT environment variable or [vcpkg.root] in config.toml");
    }

    if config.triplet.is_none()
        && std::env::var("VCPKG_DEFAULT_TRIPLET").is_err()
        && std::env::var("VCPKG_TARGET_TRIPLET").is_err()
    {
        hints.push("Set [vcpkg.triplet] in config.toml or VCPKG_DEFAULT_TRIPLET environment variable");
    }

    if hints.is_empty() {
        "Check that vcpkg root exists and contains 'installed' directory".to_string()
    } else {
        hints.join(". ")
    }
}

/// Try to run a compiler and get its version.
fn try_compiler(name: &str, version_flags: &[&str]) -> Option<(PathBuf, String)> {
    // First check if it exists
    let path = which::which(name).ok()?;

    // Try each version flag
    for flag in version_flags {
        if let Ok(output) = Command::new(name).arg(flag).output() {
            // Some compilers output version to stderr (e.g., gcc -v)
            let text = if output.stdout.is_empty() {
                String::from_utf8_lossy(&output.stderr)
            } else {
                String::from_utf8_lossy(&output.stdout)
            };

            // Extract first meaningful line
            for line in text.lines() {
                let line = line.trim();
                if !line.is_empty()
                    && (line.contains("version")
                        || line.contains("clang")
                        || line.contains("gcc")
                        || line.contains("Microsoft"))
                {
                    return Some((path, line.to_string()));
                }
            }
        }
    }

    // Compiler exists but couldn't get version
    Some((path, "unknown version".to_string()))
}

/// Format the doctor report for display.
pub fn format_report(report: &DoctorReport, verbose: bool) -> String {
    use std::fmt::Write;

    let mut output = String::new();

    writeln!(output, "Harbour Doctor").unwrap();
    writeln!(output, "==============\n").unwrap();

    // Environment
    if verbose {
        writeln!(output, "Environment:").unwrap();
        writeln!(
            output,
            "  OS: {} ({})",
            report
                .environment
                .get("os")
                .unwrap_or(&"unknown".to_string()),
            report
                .environment
                .get("arch")
                .unwrap_or(&"unknown".to_string())
        )
        .unwrap();
        writeln!(output).unwrap();
    }

    // Checks
    writeln!(output, "Checks:").unwrap();
    for check in &report.checks {
        let status = if check.passed { "[OK]" } else { "[!!]" };
        let required = if check.required { "" } else { " (optional)" };

        writeln!(output, "  {} {}{}", status, check.name, required).unwrap();

        if verbose {
            writeln!(output, "      {}", check.message).unwrap();
            if let Some(path) = &check.path {
                writeln!(output, "      Path: {}", path.display()).unwrap();
            }
            if let Some(version) = &check.version {
                writeln!(output, "      Version: {}", version).unwrap();
            }
        }
    }

    writeln!(output).unwrap();

    // Summary
    let passed = report.passed_count();
    let failed = report.failed_count();
    let required_failed = report.required_failed_count();

    writeln!(output, "Summary: {} passed, {} failed", passed, failed).unwrap();

    if required_failed > 0 {
        writeln!(
            output,
            "\nWarning: {} required check(s) failed. Some features may not work.",
            required_failed
        )
        .unwrap();
    } else if failed > 0 {
        writeln!(
            output,
            "\nAll required checks passed. {} optional check(s) failed.",
            failed
        )
        .unwrap();
    } else {
        writeln!(output, "\nAll checks passed. Harbour is ready to use.").unwrap();
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_check_result_pass() {
        let result = CheckResult::pass("test", "passed");
        assert!(result.passed);
        assert!(result.required);
    }

    #[test]
    fn test_check_result_optional() {
        let result = CheckResult::pass("test", "passed").optional();
        assert!(result.passed);
        assert!(!result.required);
    }

    #[test]
    fn test_doctor_report_all_passed() {
        let mut report = DoctorReport::new();
        report.add(CheckResult::pass("check1", "ok"));
        report.add(CheckResult::pass("check2", "ok"));

        assert!(report.all_required_passed());
        assert_eq!(report.passed_count(), 2);
        assert_eq!(report.failed_count(), 0);
    }

    #[test]
    fn test_doctor_report_optional_failed() {
        let mut report = DoctorReport::new();
        report.add(CheckResult::pass("required", "ok"));
        report.add(CheckResult::fail("optional", "missing").optional());

        assert!(report.all_required_passed());
        assert_eq!(report.passed_count(), 1);
        assert_eq!(report.failed_count(), 1);
        assert_eq!(report.required_failed_count(), 0);
    }

    #[test]
    fn test_doctor_report_required_failed() {
        let mut report = DoctorReport::new();
        report.add(CheckResult::pass("check1", "ok"));
        report.add(CheckResult::fail("check2", "missing"));

        assert!(!report.all_required_passed());
        assert_eq!(report.required_failed_count(), 1);
    }
}
