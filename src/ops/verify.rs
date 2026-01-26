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
//! 5. Build with specified backend (via standard harbour_build::build())
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
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::Serialize;
use toml::Value;
use url::Url;

use crate::builder::shim::intent::TargetTriple;
use crate::core::Workspace;
use crate::sources::registry::shim::HarnessConfig;
use crate::sources::registry::{RegistrySource, Shim};
use crate::sources::source::Source;
use crate::sources::SourceCache;
use crate::util::context::GlobalContext;

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
struct VerifyContext {
    /// Temporary directory for verification
    temp_dir: tempfile::TempDir,
    /// Cache directory for sources
    cache_dir: PathBuf,
    /// The loaded shim
    shim: Shim,
}

/// Verify a package according to the options.
///
/// The GlobalContext provides access to configuration, cache directories, and
/// registry defaults.
pub fn verify(options: VerifyOptions, ctx: &GlobalContext) -> Result<VerifyResult> {
    let start = Instant::now();
    let version_str = options
        .version
        .clone()
        .unwrap_or_else(|| "latest".to_string());
    let mut result = VerifyResult::new(&options.package, &version_str);
    result.linkage = format!("{:?}", options.linkage).to_lowercase();
    result.platform = options.platform.clone();
    result.target_triple = options.target_triple.clone();

    // Validate inputs
    if options.package.is_empty() {
        result.add_step(VerifyStep::fail(
            "Resolve",
            "No package specified",
            Duration::ZERO,
        ));
        result.total_duration = start.elapsed();
        return Ok(result);
    }

    // Step 1: Resolve package from registry
    let resolve_start = Instant::now();
    let verify_ctx = match resolve_package(&options, ctx) {
        Ok(vctx) => {
            result.version = vctx.shim.package.version.clone();
            result.add_step(VerifyStep::pass(
                "Resolve",
                format!(
                    "Package {} v{} found in registry",
                    vctx.shim.package.name, vctx.shim.package.version
                ),
                resolve_start.elapsed(),
            ));
            vctx
        }
        Err(e) => {
            result.add_step(VerifyStep::fail(
                "Resolve",
                format!("Failed to resolve package: {}", e),
                resolve_start.elapsed(),
            ));
            result.total_duration = start.elapsed();
            return Ok(result);
        }
    };

    // Step 2: Fetch source
    let fetch_start = Instant::now();
    let source_dir = match fetch_source(&verify_ctx) {
        Ok(dir) => {
            result.add_step(VerifyStep::pass(
                "Fetch",
                format!("Source fetched to {}", dir.display()),
                fetch_start.elapsed(),
            ));
            dir
        }
        Err(e) => {
            result.add_step(VerifyStep::fail(
                "Fetch",
                format!("Failed to fetch source: {}", e),
                fetch_start.elapsed(),
            ));
            result.total_duration = start.elapsed();
            return Ok(result);
        }
    };

    // Step 3: Apply patches (if any)
    let patch_start = Instant::now();
    if !verify_ctx.shim.patches.is_empty() {
        match apply_patches(&verify_ctx, &source_dir, &options) {
            Ok(_) => {
                result.add_step(VerifyStep::pass(
                    "Patches",
                    format!("Applied {} patches", verify_ctx.shim.patches.len()),
                    patch_start.elapsed(),
                ));
            }
            Err(e) => {
                result.add_step(VerifyStep::fail(
                    "Patches",
                    format!("Failed to apply patches: {}", e),
                    patch_start.elapsed(),
                ));
                result.total_duration = start.elapsed();
                return Ok(result);
            }
        }
    } else {
        result.add_step(VerifyStep::pass(
            "Patches",
            "No patches to apply",
            patch_start.elapsed(),
        ));
    }

    // Step 4: Build
    let build_start = Instant::now();
    match build_package(&verify_ctx, &source_dir, &options, ctx) {
        Ok(artifacts) => {
            result.artifacts = artifacts.clone();
            let artifact_list: Vec<String> = artifacts
                .iter()
                .map(|p| {
                    p.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string()
                })
                .collect();
            result.add_step(VerifyStep::pass(
                "Build",
                format!(
                    "Build completed ({:?} linkage). Artifacts: {}",
                    options.linkage,
                    artifact_list.join(", ")
                ),
                build_start.elapsed(),
            ));
        }
        Err(e) => {
            result.add_step(VerifyStep::fail(
                "Build",
                format!("Build failed: {}", e),
                build_start.elapsed(),
            ));
            result.total_duration = start.elapsed();
            return Ok(result);
        }
    }

    // Step 5: Verify artifacts
    let verify_start = Instant::now();
    match verify_artifacts(&result.artifacts) {
        Ok(_) => {
            result.add_step(VerifyStep::pass(
                "Artifacts",
                format!("Verified {} artifacts exist", result.artifacts.len()),
                verify_start.elapsed(),
            ));
        }
        Err(e) => {
            result.add_step(VerifyStep::fail(
                "Artifacts",
                format!("Artifact verification failed: {}", e),
                verify_start.elapsed(),
            ));
            result.total_duration = start.elapsed();
            return Ok(result);
        }
    }

    // Step 6: Harness test (if configured and not skipped)
    if !options.skip_harness {
        let harness_start = Instant::now();
        if let Some(harness_config) = verify_ctx.shim.harness() {
            match run_harness_test(
                harness_config,
                &verify_ctx,
                &source_dir,
                &result.artifacts,
                options.target_triple.as_deref(),
            ) {
                Ok(_) => {
                    result.add_step(VerifyStep::pass(
                        "Harness",
                        format!("Consumer test passed (tested {})", harness_config.header),
                        harness_start.elapsed(),
                    ));
                }
                Err(e) => {
                    result.add_step(VerifyStep::fail(
                        "Harness",
                        format!("Harness test failed: {}", e),
                        harness_start.elapsed(),
                    ));
                }
            }
        } else {
            result.add_step(
                VerifyStep::pass("Harness", "No harness configured", harness_start.elapsed())
                    .with_warning("Package has no harness test defined"),
            );
        }
    }

    result.total_duration = start.elapsed();
    Ok(result)
}

/// Resolve package from registry and set up verification context.
fn resolve_package(options: &VerifyOptions, ctx: &GlobalContext) -> Result<VerifyContext> {
    // Create temporary directory for verification
    let temp_dir = tempfile::tempdir().context("failed to create temp directory")?;

    // Use GlobalContext's cache directory if available, otherwise use temp dir
    let cache_dir = ctx.cache_dir().join("verify");
    std::fs::create_dir_all(&cache_dir)?;

    // Determine version to use
    let version = options.version.as_deref().unwrap_or("latest");

    // Load shim from registry
    let shim = if let Some(registry_path) = &options.registry_path {
        // Local registry (CI mode)
        load_shim_from_local_registry(registry_path, &options.package, version)?
    } else {
        // Remote registry (uses default registry from GlobalContext)
        let registry_url = ctx.default_registry_url();
        load_shim_from_remote_registry(&options.package, version, &cache_dir, &registry_url)?
    };

    Ok(VerifyContext {
        temp_dir,
        cache_dir,
        shim,
    })
}

/// Load shim from a local registry directory.
fn load_shim_from_local_registry(
    registry_path: &Path,
    package: &str,
    version: &str,
) -> Result<Shim> {
    // For local registry, directly load the shim file
    let first_char = package
        .chars()
        .next()
        .ok_or_else(|| anyhow::anyhow!("invalid empty package name"))?;

    let shim_path = if version == "latest" {
        // Find the latest version by scanning the directory
        let pkg_dir = registry_path
            .join("index")
            .join(first_char.to_string())
            .join(package);

        if !pkg_dir.exists() {
            bail!(
                "package '{}' not found in registry at {}",
                package,
                registry_path.display()
            );
        }

        find_latest_version(&pkg_dir)?
    } else {
        registry_path
            .join("index")
            .join(first_char.to_string())
            .join(package)
            .join(format!("{}.toml", version))
    };

    if !shim_path.exists() {
        bail!(
            "shim not found: {} (looked at {})",
            package,
            shim_path.display()
        );
    }

    Shim::load(&shim_path).context("failed to load shim")
}

/// Find the latest version in a package directory.
fn find_latest_version(pkg_dir: &Path) -> Result<PathBuf> {
    let mut versions: Vec<(semver::Version, PathBuf)> = Vec::new();

    for entry in std::fs::read_dir(pkg_dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.extension().map_or(false, |ext| ext == "toml") {
            if let Some(stem) = path.file_stem() {
                let version_str = stem.to_string_lossy();
                if let Ok(version) = semver::Version::parse(&version_str) {
                    versions.push((version, path));
                }
            }
        }
    }

    versions.sort_by(|a, b| b.0.cmp(&a.0)); // Sort descending

    versions
        .into_iter()
        .next()
        .map(|(_, path)| path)
        .ok_or_else(|| anyhow::anyhow!("no versions found in {}", pkg_dir.display()))
}

/// Load shim from the remote registry.
fn load_shim_from_remote_registry(
    package: &str,
    version: &str,
    cache_dir: &Path,
    registry_url: &Url,
) -> Result<Shim> {
    let source_id = crate::core::SourceId::for_registry(registry_url)?;
    let mut registry = RegistrySource::new(registry_url.clone(), cache_dir, source_id);

    // Ensure the index is fetched
    registry.ensure_ready()?;

    // Load the shim
    let version_to_load = if version == "latest" {
        // TODO: Implement latest version discovery for remote registry
        bail!("'latest' version not supported for remote registry yet; specify an exact version");
    } else {
        version.to_string()
    };

    registry
        .load_shim(package, &version_to_load)?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "package '{}' version '{}' not found",
                package,
                version_to_load
            )
        })
}

/// Fetch the package source.
fn fetch_source(ctx: &VerifyContext) -> Result<PathBuf> {
    let source_dir = ctx.cache_dir.join("src").join(&ctx.shim.package.name);

    if let Some(git) = &ctx.shim.source.git {
        fetch_git_source(git, &source_dir)?;
    } else if let Some(_tarball) = &ctx.shim.source.tarball {
        bail!("tarball sources not yet supported");
    } else {
        bail!("no source specified in shim");
    }

    Ok(source_dir)
}

/// Get a short (8-char) version of a SHA for display.
fn short_sha(sha: &str) -> &str {
    &sha[..8.min(sha.len())]
}

/// Fetch a git source.
fn fetch_git_source(git: &crate::sources::registry::shim::GitSource, dest: &Path) -> Result<()> {
    use git2::{Repository, ResetType};

    let target_oid = git2::Oid::from_str(&git.rev)
        .with_context(|| format!("invalid commit SHA: {}", git.rev))?;

    // Check if we already have the right commit checked out
    if dest.exists() {
        if let Ok(repo) = Repository::open(dest) {
            if let Ok(head) = repo.head() {
                if let Some(oid) = head.target() {
                    if oid == target_oid {
                        tracing::info!("Using cached source at {}", short_sha(&git.rev));
                        return Ok(());
                    }
                }
            }
            // Wrong commit or dirty state - try to reset to correct commit
            if let Ok(commit) = repo.find_commit(target_oid) {
                tracing::info!("Resetting to {} (cached)", short_sha(&git.rev));
                repo.reset(commit.as_object(), ResetType::Hard, None)
                    .context("failed to reset cached repository to target commit")?;
                return Ok(());
            }
        }
        // Invalid or incompatible repo - remove and re-clone
        tracing::debug!("Removing stale source directory");
        std::fs::remove_dir_all(dest)?;
    }

    tracing::info!("Cloning {} at {}", git.url, short_sha(&git.rev));

    // Clone the repository
    std::fs::create_dir_all(dest)?;
    let repo = Repository::clone(&git.url, dest)
        .with_context(|| format!("failed to clone {}", git.url))?;

    // Checkout specific commit
    let commit = repo
        .find_commit(target_oid)
        .with_context(|| format!("commit {} not found", git.rev))?;
    repo.reset(commit.as_object(), ResetType::Hard, None)
        .context("failed to reset repository to target commit")?;

    Ok(())
}

/// Apply patches to the source.
fn apply_patches(ctx: &VerifyContext, source_dir: &Path, options: &VerifyOptions) -> Result<()> {
    let registry_path = options
        .registry_path
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("patches require local registry path"))?;

    let first_char = ctx.shim.package.name.chars().next().unwrap();
    let shim_dir = registry_path
        .join("index")
        .join(first_char.to_string())
        .join(&ctx.shim.package.name);

    for patch in &ctx.shim.patches {
        let patch_path = shim_dir.join(&patch.file);

        if !patch_path.exists() {
            bail!("patch file not found: {}", patch_path.display());
        }

        // Verify patch hash
        crate::sources::registry::shim::verify_patch_hash(&patch_path, &patch.sha256)?;

        // Apply patch using git apply
        let output = Command::new("git")
            .args(["apply", "--check"])
            .arg(&patch_path)
            .current_dir(source_dir)
            .output()
            .context("failed to run git apply --check")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("patch '{}' will not apply cleanly: {}", patch.file, stderr);
        }

        let output = Command::new("git")
            .arg("apply")
            .arg(&patch_path)
            .current_dir(source_dir)
            .output()
            .context("failed to run git apply")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("failed to apply patch '{}': {}", patch.file, stderr);
        }

        tracing::info!("Applied patch: {}", patch.file);
    }

    Ok(())
}

/// Build the package.
///
/// For CMake projects, uses CMakeBuilder directly since harbour_build::build()
/// doesn't yet dispatch to CMake. For native projects, uses the standard build.
fn build_package(
    verify_ctx: &VerifyContext,
    source_dir: &Path,
    options: &VerifyOptions,
    global_ctx: &GlobalContext,
) -> Result<Vec<PathBuf>> {
    // Check if this is a CMake project
    let is_cmake = verify_ctx
        .shim
        .build
        .as_ref()
        .and_then(|b| b.backend.as_ref())
        .map(|b| b == "cmake")
        .unwrap_or(false);

    if is_cmake {
        return build_with_cmake(verify_ctx, source_dir, options);
    }

    // Native backend - generate Harbor.toml and use standard build
    let manifest_content = generate_manifest_toml(&verify_ctx.shim)?;

    // Write to Harbor.toml (the build system expects this exact name)
    let manifest_path = source_dir.join("Harbor.toml");
    let backup_path = source_dir.join("Harbor.toml.bak");

    // Back up existing manifest if present
    if manifest_path.exists() {
        tracing::warn!("Source has existing Harbor.toml - backing up to Harbor.toml.bak");
        std::fs::rename(&manifest_path, &backup_path)
            .context("failed to back up existing manifest")?;
    }

    tracing::debug!("Generated manifest:\n{}", manifest_content);
    std::fs::write(&manifest_path, &manifest_content)
        .context("failed to write verification manifest")?;

    // Set up build context with cwd pointing to source directory
    let build_global_ctx = GlobalContext::with_cwd(source_dir.to_path_buf())?;
    let profile = "release";
    let ws = Workspace::new(&manifest_path, &build_global_ctx)?.with_profile(profile);

    let mut source_cache = SourceCache::new(verify_ctx.cache_dir.clone());

    // Determine linkage - warn if Both is used since we only test one at a time
    let linkage = match options.linkage {
        VerifyLinkage::Auto | VerifyLinkage::Static => {
            crate::builder::shim::LinkagePreference::static_()
        }
        VerifyLinkage::Shared => crate::builder::shim::LinkagePreference::shared(),
        VerifyLinkage::Both => {
            tracing::warn!(
                "VerifyLinkage::Both currently only tests static linkage. \
                 Run separately with --linkage=shared to test shared linkage."
            );
            crate::builder::shim::LinkagePreference::static_()
        }
    };

    // Build options for native backend
    let build_opts = crate::ops::harbour_build::BuildOptions {
        release: true,
        packages: vec![verify_ctx.shim.package.name.clone()],
        targets: vec![],
        emit_compile_commands: false,
        emit_plan: false,
        jobs: None,
        verbose: options.verbose || global_ctx.is_verbose(),
        cpp_std: None,
        backend: None, // Native
        linkage,
        ffi: false,
        target_triple: options.target_triple.as_ref().map(|s| TargetTriple::new(s)),
        locked: false,
    };

    // Run the build using standard infrastructure
    let build_result = crate::ops::harbour_build::build(&ws, &mut source_cache, &build_opts)
        .map_err(|e| {
            tracing::error!("Native build error: {:?}", e);
            e
        })
        .context("native build failed")?;

    // Collect artifact paths from build result
    let artifacts: Vec<PathBuf> = build_result
        .artifacts
        .iter()
        .map(|a| a.path.clone())
        .collect();

    if artifacts.is_empty() {
        bail!("build produced no artifacts");
    }

    Ok(artifacts)
}

/// Build a CMake project directly.
///
/// This directly invokes CMake since harbour_build::build() doesn't yet
/// dispatch to the CMake backend.
fn build_with_cmake(
    verify_ctx: &VerifyContext,
    source_dir: &Path,
    options: &VerifyOptions,
) -> Result<Vec<PathBuf>> {
    tracing::info!("Building with CMake backend");

    let build_dir = verify_ctx.temp_dir.path().join("cmake-build");
    std::fs::create_dir_all(&build_dir)?;

    // Collect CMake arguments from shim
    let mut cmake_args: Vec<String> = vec![
        "-S".to_string(),
        source_dir.to_string_lossy().to_string(),
        "-B".to_string(),
        build_dir.to_string_lossy().to_string(),
        "-DCMAKE_BUILD_TYPE=Release".to_string(),
        "-DCMAKE_POSITION_INDEPENDENT_CODE=ON".to_string(),
    ];

    // Add shim-specified options
    if let Some(build) = &verify_ctx.shim.build {
        if let Some(cmake) = &build.cmake {
            for opt in &cmake.options {
                cmake_args.push(opt.clone());
            }
        }
    }

    // Configure
    tracing::info!("Configuring CMake project");
    if options.verbose {
        tracing::debug!("CMake args: {:?}", cmake_args);
    }

    let output = Command::new("cmake")
        .args(&cmake_args)
        .current_dir(source_dir)
        .output()
        .context("failed to run cmake configure")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        bail!(
            "CMake configure failed:\nstdout:\n{}\nstderr:\n{}",
            stdout,
            stderr
        );
    }

    // Build
    tracing::info!("Building CMake project");
    let output = Command::new("cmake")
        .arg("--build")
        .arg(&build_dir)
        .arg("--config")
        .arg("Release")
        .arg("--parallel")
        .current_dir(source_dir)
        .output()
        .context("failed to run cmake build")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        bail!(
            "CMake build failed:\nstdout:\n{}\nstderr:\n{}",
            stdout,
            stderr
        );
    }

    // Find built artifacts
    let mut artifacts = Vec::new();

    // Determine library extensions based on OS
    #[cfg(target_os = "windows")]
    let lib_extensions: &[&str] = &["lib", "dll"];
    #[cfg(target_os = "macos")]
    let lib_extensions: &[&str] = &["a", "dylib"];
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let lib_extensions: &[&str] = &["a", "so"];

    // Search for libraries in build directory
    for entry in walkdir::WalkDir::new(&build_dir)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            if lib_extensions.contains(&ext) {
                // Skip CMake internal files
                let filename = path.file_name().unwrap_or_default().to_string_lossy();
                if !filename.starts_with("cmake") && !filename.contains("CMake") {
                    artifacts.push(path.to_path_buf());
                }
            }
        }
    }

    if artifacts.is_empty() {
        bail!(
            "CMake build produced no library artifacts in {}",
            build_dir.display()
        );
    }

    if options.verbose {
        for artifact in &artifacts {
            tracing::info!("Built artifact: {}", artifact.display());
        }
    }

    Ok(artifacts)
}

/// Generate a Harbor.toml manifest from a shim.
///
/// Uses the `toml` crate for safe serialization, ensuring proper escaping
/// and formatting of all values.
fn generate_manifest_toml(shim: &Shim) -> Result<String> {
    let mut doc = toml::Table::new();
    let pkg_name = &shim.package.name;

    // [package]
    let mut package = toml::Table::new();
    package.insert("name".into(), Value::String(pkg_name.clone()));
    package.insert(
        "version".into(),
        Value::String(shim.package.version.clone()),
    );
    doc.insert("package".into(), Value::Table(package));

    // [targets.NAME]
    let mut targets = toml::Table::new();
    let mut target = toml::Table::new();
    target.insert("kind".into(), Value::String("staticlib".into()));

    // Check for build configuration (cmake, meson, etc.)
    let backend_name = shim
        .build
        .as_ref()
        .and_then(|b| b.backend.as_ref())
        .cloned();

    let is_native = backend_name.as_ref().map_or(true, |b| b == "native");

    // Backend configuration
    if let Some(ref backend) = backend_name {
        if backend != "native" {
            let mut backend_table = toml::Table::new();
            backend_table.insert("backend".into(), Value::String(backend.clone()));

            // Parse and convert CMake options
            if backend == "cmake" {
                if let Some(build) = &shim.build {
                    if let Some(cmake) = &build.cmake {
                        let options = parse_cmake_options(&cmake.options)?;
                        if !options.is_empty() {
                            backend_table.insert("options".into(), Value::Table(options));
                        }
                    }
                }
            }

            target.insert("backend".into(), Value::Table(backend_table));
        }
    }

    // Sources (native backend only)
    if is_native {
        let sources = shim
            .effective_surface_override()
            .and_then(|s| {
                if s.sources.is_empty() {
                    None
                } else {
                    Some(s.sources.clone())
                }
            })
            .unwrap_or_else(|| vec!["*.c".into(), "src/*.c".into()]);

        target.insert(
            "sources".into(),
            Value::Array(sources.iter().map(|s| Value::String(s.clone())).collect()),
        );
    }

    // Surface configuration
    if let Some(surface) = shim.effective_surface_override() {
        if let Some(compile) = &surface.compile {
            if let Some(public) = &compile.public {
                if !public.include_dirs.is_empty() || !public.defines.is_empty() {
                    let mut surface_table = toml::Table::new();
                    let mut compile_table = toml::Table::new();
                    let mut public_table = toml::Table::new();

                    if !public.include_dirs.is_empty() {
                        public_table.insert(
                            "include_dirs".into(),
                            Value::Array(
                                public
                                    .include_dirs
                                    .iter()
                                    .map(|d| Value::String(d.clone()))
                                    .collect(),
                            ),
                        );
                    }

                    if !public.defines.is_empty() {
                        public_table.insert(
                            "defines".into(),
                            Value::Array(
                                public
                                    .defines
                                    .iter()
                                    .map(|d| Value::String(d.clone()))
                                    .collect(),
                            ),
                        );
                    }

                    compile_table.insert("public".into(), Value::Table(public_table));
                    surface_table.insert("compile".into(), Value::Table(compile_table));
                    target.insert("surface".into(), Value::Table(surface_table));
                }
            }
        }
    }

    targets.insert(pkg_name.clone(), Value::Table(target));
    doc.insert("targets".into(), Value::Table(targets));

    toml::to_string_pretty(&doc).context("failed to serialize manifest")
}

/// Parse CMake options from shim format (-DKEY=VALUE) into a TOML table.
///
/// Handles:
/// - `-DKEY=VALUE` -> key = value
/// - `-DKEY:TYPE=VALUE` -> key = value (type annotation stripped)
/// - `-G Generator` -> CMAKE_GENERATOR = "Generator"
/// - Boolean values: ON/OFF/TRUE/FALSE/YES/NO/1/0
///
/// Warns about malformed options that cannot be parsed.
fn parse_cmake_options(opts: &[String]) -> Result<toml::Table> {
    let mut table = toml::Table::new();

    for opt in opts {
        if let Some(stripped) = opt.strip_prefix("-D") {
            // Handle -DKEY:TYPE=VALUE or -DKEY=VALUE
            if let Some((key_part, value)) = stripped.split_once('=') {
                // Strip type annotation if present: KEY:BOOL -> KEY
                let key = key_part.split(':').next().unwrap_or(key_part);
                let parsed_value = parse_cmake_value(value);
                table.insert(key.to_string(), parsed_value);
            } else {
                // Malformed: -DKEY without value
                tracing::warn!(
                    "Skipping malformed CMake option '{}': expected -DKEY=VALUE format",
                    opt
                );
            }
        } else if let Some(generator) = opt.strip_prefix("-G") {
            // -G Generator or -GGenerator
            let gen = generator.trim();
            if !gen.is_empty() {
                table.insert("CMAKE_GENERATOR".into(), Value::String(gen.to_string()));
            } else {
                tracing::warn!(
                    "Skipping malformed CMake option '{}': -G requires a generator name",
                    opt
                );
            }
        } else if opt.starts_with('-') {
            // Unknown flag - warn but don't fail
            tracing::debug!(
                "Ignoring unrecognized CMake option '{}': only -D and -G flags are converted",
                opt
            );
        }
        // Non-flag options are silently ignored (shouldn't happen in well-formed shims)
    }

    Ok(table)
}

/// Parse a CMake value string into an appropriate TOML value.
///
/// - ON/TRUE/YES/1 -> true
/// - OFF/FALSE/NO/0 -> false
/// - Integer strings -> integer
/// - Everything else -> string
fn parse_cmake_value(v: &str) -> Value {
    match v.to_uppercase().as_str() {
        "ON" | "TRUE" | "YES" | "1" => Value::Boolean(true),
        "OFF" | "FALSE" | "NO" | "0" => Value::Boolean(false),
        _ => {
            // Try integer
            if let Ok(i) = v.parse::<i64>() {
                Value::Integer(i)
            } else {
                Value::String(v.to_string())
            }
        }
    }
}

/// Verify that artifacts exist.
fn verify_artifacts(artifacts: &[PathBuf]) -> Result<()> {
    for artifact in artifacts {
        if !artifact.exists() {
            bail!("artifact does not exist: {}", artifact.display());
        }

        let metadata = std::fs::metadata(artifact)?;
        if metadata.len() == 0 {
            bail!("artifact is empty: {}", artifact.display());
        }
    }

    Ok(())
}

/// Run the harness test.
fn run_harness_test(
    config: &HarnessConfig,
    ctx: &VerifyContext,
    source_dir: &Path,
    artifacts: &[PathBuf],
    target_triple: Option<&str>,
) -> Result<()> {
    // Create harness test directory
    let harness_dir = ctx.temp_dir.path().join("harness");
    std::fs::create_dir_all(&harness_dir)?;

    // Generate harness source file
    let harness_path = generate_harness(config, &harness_dir)?;

    // Find library and include paths
    let lib_path = artifacts
        .iter()
        .find(|p| {
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
            ext == "a" || ext == "lib"
        })
        .ok_or_else(|| anyhow::anyhow!("no library artifact found for harness test"))?;

    // Get include directory from surface override
    let include_dir = if let Some(surface) = ctx.shim.effective_surface_override() {
        surface
            .compile
            .as_ref()
            .and_then(|c| c.public.as_ref())
            .and_then(|p| p.include_dirs.first())
            .map(|d| source_dir.join(d))
            .unwrap_or_else(|| source_dir.to_path_buf())
    } else {
        source_dir.to_path_buf()
    };

    // Compile harness
    let output_path = harness_dir.join(if cfg!(windows) {
        "harness.exe"
    } else {
        "harness"
    });

    #[cfg(windows)]
    {
        // On Windows, use detected MSVC toolchain (supports auto-detection)
        use crate::builder::toolchain::detect_toolchain;

        let toolchain = detect_toolchain()
            .context("failed to detect MSVC toolchain for harness compilation")?;

        // Get compiler path and generate compile command
        let compiler = toolchain.compiler_path();

        let mut cmd = Command::new(compiler);
        cmd.arg("/nologo")
            .arg(format!("/I{}", include_dir.display()))
            .arg(&harness_path)
            .arg(lib_path)
            .arg(format!("/Fe:{}", output_path.display()));

        // Add C++ flag if needed
        if config.lang == "cxx" || config.lang == "c++" {
            cmd.arg("/TP"); // Treat source as C++
        }

        cmd.arg("/link");

        // Apply environment variables from toolchain (for auto-detected MSVC)
        // Generate a dummy compile command to get the environment
        let dummy_input = crate::builder::toolchain::CompileInput {
            source: harness_path.clone(),
            output: output_path.clone(),
            include_dirs: vec![],
            defines: vec![],
            cflags: vec![],
        };
        let spec = toolchain.compile_command(&dummy_input, crate::core::target::Language::C, None);
        for (key, value) in &spec.env {
            cmd.env(key, value);
        }

        tracing::debug!("Running harness compile: {:?}", cmd);

        let output = cmd.output().context("failed to compile harness")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            bail!("harness compilation failed:\n{}\n{}", stdout, stderr);
        }
    }

    #[cfg(not(windows))]
    {
        let compiler = if config.lang == "cxx" || config.lang == "c++" {
            std::env::var("CXX").unwrap_or_else(|_| "c++".to_string())
        } else {
            std::env::var("CC").unwrap_or_else(|_| "cc".to_string())
        };

        let mut cmd = Command::new(&compiler);
        cmd.arg(&harness_path)
            .arg("-o")
            .arg(&output_path)
            .arg(format!("-I{}", include_dir.display()))
            .arg(lib_path);

        // Add platform-specific link flags
        #[cfg(target_os = "linux")]
        {
            cmd.args(["-lpthread", "-ldl", "-lm"]);
        }

        #[cfg(target_os = "macos")]
        {
            // Common macOS frameworks that C libraries may depend on
            cmd.args(["-framework", "CoreFoundation"]);
        }

        tracing::debug!("Running harness compile: {:?}", cmd);

        let output = cmd.output().context("failed to compile harness")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            bail!("harness compilation failed:\n{}\n{}", stdout, stderr);
        }
    }

    // Run harness (skip for cross-compilation)
    let is_cross_compile = target_triple.map_or(false, |triple| {
        // Check if target triple differs from host
        let host_triple = get_host_triple();
        triple != host_triple
    });

    if is_cross_compile {
        tracing::info!("Harness compiled successfully (execution skipped for cross-compilation)");
        return Ok(());
    }

    // Execute the harness
    tracing::info!("Running harness test");
    let output = Command::new(&output_path)
        .output()
        .context("failed to execute harness")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        bail!(
            "harness execution failed (exit code {:?}):\n{}\n{}",
            output.status.code(),
            stdout,
            stderr
        );
    }

    tracing::info!("Harness test passed");
    Ok(())
}

/// Get the host target triple for the current platform.
fn get_host_triple() -> &'static str {
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        "x86_64-pc-windows-msvc"
    }
    #[cfg(all(target_os = "windows", target_arch = "x86"))]
    {
        "i686-pc-windows-msvc"
    }
    #[cfg(all(target_os = "windows", target_arch = "aarch64"))]
    {
        "aarch64-pc-windows-msvc"
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        "x86_64-unknown-linux-gnu"
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        "aarch64-unknown-linux-gnu"
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        "x86_64-apple-darwin"
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        "aarch64-apple-darwin"
    }
    #[cfg(not(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "windows", target_arch = "x86"),
        all(target_os = "windows", target_arch = "aarch64"),
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64"),
    )))]
    {
        "unknown-unknown-unknown"
    }
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

        if verbose || !step.passed {
            writeln!(output, "      {}", step.message).unwrap();
        }
        for warning in &step.warnings {
            writeln!(output, "      Warning: {}", warning).unwrap();
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

    // Artifacts
    if !result.artifacts.is_empty() {
        writeln!(output, "\nArtifacts:").unwrap();
        for artifact in &result.artifacts {
            writeln!(output, "  - {}", artifact.display()).unwrap();
        }
    }

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
    serde_json::to_string_pretty(result)
        .unwrap_or_else(|e| format!(r#"{{"error": "Failed to serialize result: {}"}}"#, e))
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
    let overall_emoji = if result.passed {
        ":heavy_check_mark:"
    } else {
        ":x:"
    };
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

    // Artifacts
    if !result.artifacts.is_empty() {
        writeln!(output).unwrap();
        writeln!(output, "### Artifacts").unwrap();
        for artifact in &result.artifacts {
            if let Some(name) = artifact.file_name() {
                writeln!(output, "- `{}`", name.to_string_lossy()).unwrap();
            }
        }
    }

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

    #[test]
    fn test_verify_empty_package() {
        let options = VerifyOptions::default();
        let ctx = GlobalContext::new().unwrap();
        let result = verify(options, &ctx).unwrap();
        assert!(!result.passed);
    }

    #[test]
    fn test_cmake_option_parsing() {
        let opts = vec![
            "-DFOO=ON".into(),
            "-DBAR:BOOL=OFF".into(),
            "-DBAZ=some_string".into(),
            "-DCOUNT=42".into(),
            "-G Ninja".into(),
        ];
        let table = parse_cmake_options(&opts).unwrap();

        assert_eq!(table.get("FOO"), Some(&Value::Boolean(true)));
        assert_eq!(table.get("BAR"), Some(&Value::Boolean(false)));
        assert_eq!(table.get("BAZ"), Some(&Value::String("some_string".into())));
        assert_eq!(table.get("COUNT"), Some(&Value::Integer(42)));
        assert_eq!(
            table.get("CMAKE_GENERATOR"),
            Some(&Value::String("Ninja".into()))
        );
    }

    #[test]
    fn test_cmake_option_parsing_malformed() {
        // Malformed options should be skipped (with warnings logged)
        let opts = vec![
            "-DFOO=ON".into(),    // Valid
            "-DMALFORMED".into(), // Invalid: no value
            "-G".into(),          // Invalid: no generator name
            "-DBAR=OFF".into(),   // Valid
            "-WUNKNOWN".into(),   // Unknown flag (ignored)
        ];
        let table = parse_cmake_options(&opts).unwrap();

        // Only valid options should be in the table
        assert_eq!(table.len(), 2);
        assert_eq!(table.get("FOO"), Some(&Value::Boolean(true)));
        assert_eq!(table.get("BAR"), Some(&Value::Boolean(false)));

        // Malformed options should NOT be in the table
        assert!(table.get("MALFORMED").is_none());
        assert!(table.get("CMAKE_GENERATOR").is_none());
    }

    #[test]
    fn test_cmake_value_parsing() {
        // Boolean true values
        assert_eq!(parse_cmake_value("ON"), Value::Boolean(true));
        assert_eq!(parse_cmake_value("TRUE"), Value::Boolean(true));
        assert_eq!(parse_cmake_value("YES"), Value::Boolean(true));
        assert_eq!(parse_cmake_value("1"), Value::Boolean(true));

        // Boolean false values
        assert_eq!(parse_cmake_value("OFF"), Value::Boolean(false));
        assert_eq!(parse_cmake_value("FALSE"), Value::Boolean(false));
        assert_eq!(parse_cmake_value("NO"), Value::Boolean(false));
        assert_eq!(parse_cmake_value("0"), Value::Boolean(false));

        // Integer values
        assert_eq!(parse_cmake_value("42"), Value::Integer(42));
        assert_eq!(parse_cmake_value("-10"), Value::Integer(-10));

        // String values
        assert_eq!(
            parse_cmake_value("some_value"),
            Value::String("some_value".into())
        );
    }

    #[test]
    fn test_generate_manifest_native() {
        use crate::sources::registry::shim::{
            CompileSurfacePublic, Shim, ShimPackage, ShimSource, ShimSurfaceOverride,
            SurfaceOverrideCompile,
        };

        let shim = Shim {
            package: ShimPackage {
                name: "mylib".to_string(),
                version: "1.0.0".to_string(),
            },
            source: ShimSource {
                git: Some(crate::sources::registry::shim::GitSource {
                    url: "https://example.com/repo".to_string(),
                    rev: "a".repeat(40),
                    checksum: None,
                }),
                tarball: None,
            },
            patches: vec![],
            metadata: None,
            features: None,
            surface_override: Some(ShimSurfaceOverride {
                compile: Some(SurfaceOverrideCompile {
                    public: Some(CompileSurfacePublic {
                        include_dirs: vec!["include".to_string()],
                        defines: vec!["MYLIB_API".to_string()],
                    }),
                    private: None,
                }),
                link: None,
                sources: vec!["src/*.c".to_string()],
            }),
            surface: None,
            build: None,
        };

        let manifest = generate_manifest_toml(&shim).unwrap();

        // Verify it parses correctly
        let parsed: toml::Table = toml::from_str(&manifest).unwrap();
        assert!(parsed.contains_key("package"));
        assert!(parsed.contains_key("targets"));

        let targets = parsed.get("targets").unwrap().as_table().unwrap();
        let target = targets.get("mylib").unwrap().as_table().unwrap();
        assert_eq!(target.get("kind").unwrap().as_str().unwrap(), "staticlib");

        // Check sources are included for native backend
        let sources = target.get("sources").unwrap().as_array().unwrap();
        assert!(!sources.is_empty());
    }

    #[test]
    fn test_generate_manifest_cmake() {
        use crate::sources::registry::shim::{
            CompileSurfacePublic, Shim, ShimBuildConfig, ShimCMakeConfig, ShimPackage, ShimSource,
            ShimSurfaceOverride, SurfaceOverrideCompile,
        };

        let shim = Shim {
            package: ShimPackage {
                name: "libuv".to_string(),
                version: "1.51.0".to_string(),
            },
            source: ShimSource {
                git: Some(crate::sources::registry::shim::GitSource {
                    url: "https://github.com/libuv/libuv".to_string(),
                    rev: "a".repeat(40),
                    checksum: None,
                }),
                tarball: None,
            },
            patches: vec![],
            metadata: None,
            features: None,
            surface_override: Some(ShimSurfaceOverride {
                compile: Some(SurfaceOverrideCompile {
                    public: Some(CompileSurfacePublic {
                        include_dirs: vec!["include".to_string()],
                        defines: vec![],
                    }),
                    private: None,
                }),
                link: None,
                sources: vec![], // CMake doesn't need sources
            }),
            surface: None,
            build: Some(ShimBuildConfig {
                backend: Some("cmake".to_string()),
                cmake: Some(ShimCMakeConfig {
                    options: vec![
                        "-DLIBUV_BUILD_TESTS=OFF".to_string(),
                        "-DLIBUV_BUILD_BENCH=OFF".to_string(),
                        "-G Ninja".to_string(),
                    ],
                }),
            }),
        };

        let manifest = generate_manifest_toml(&shim).unwrap();

        // Verify it parses correctly
        let parsed: toml::Table = toml::from_str(&manifest).unwrap();
        assert!(parsed.contains_key("package"));
        assert!(parsed.contains_key("targets"));

        let targets = parsed.get("targets").unwrap().as_table().unwrap();
        let target = targets.get("libuv").unwrap().as_table().unwrap();

        // Check backend is present
        let backend = target.get("backend").unwrap().as_table().unwrap();
        assert_eq!(backend.get("backend").unwrap().as_str().unwrap(), "cmake");

        // Check options are converted
        let options = backend.get("options").unwrap().as_table().unwrap();
        assert_eq!(
            options.get("LIBUV_BUILD_TESTS"),
            Some(&Value::Boolean(false))
        );
        assert_eq!(
            options.get("LIBUV_BUILD_BENCH"),
            Some(&Value::Boolean(false))
        );
        assert_eq!(
            options.get("CMAKE_GENERATOR"),
            Some(&Value::String("Ninja".into()))
        );

        // CMake packages should NOT have sources in the target
        assert!(target.get("sources").is_none());
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
