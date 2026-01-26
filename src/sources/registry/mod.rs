//! Registry source - packages from a git-based package registry.
//!
//! A registry is a git repository containing shim files that reference
//! actual package sources. This enables centralized package discovery
//! while keeping source code distributed.
//!
//! # Registry Structure
//!
//! ```text
//! registry/
//! ├── config.toml                # Registry metadata
//! ├── z/
//! │   └── zlib/
//! │       ├── 1.3.1.toml         # Shim files
//! │       └── patches/           # Optional patches
//! │           └── fix-cmake.patch
//! └── s/
//!     └── sqlite/
//!         └── 3.45.0.toml
//! ```
//!
//! # Shim Format
//!
//! Each shim is a TOML file that references the actual source:
//!
//! ```toml
//! [package]
//! name = "zlib"
//! version = "1.3.1"
//!
//! [source.git]
//! url = "https://github.com/madler/zlib"
//! rev = "04f42ceca40f73e2978b50e93806c2a18c1281fc"
//! ```

pub mod config;
pub mod shim;

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use git2::{Repository, ResetType};
use url::Url;

use crate::core::{Dependency, Manifest, Package, PackageId, SourceId, Summary};
use crate::sources::Source;
use crate::util::hash::sha256_file;

pub use config::RegistryConfig;
pub use shim::{shim_path, validate_package_name, Shim, ShimPatch};

/// A source for registry dependencies.
///
/// RegistrySource manages a cached git clone of the registry index
/// and fetches package sources on demand based on shim files.
pub struct RegistrySource {
    /// Registry git URL
    registry_url: Url,

    /// Local path to cloned registry index
    index_path: PathBuf,

    /// Local path for fetched package sources
    src_cache_path: PathBuf,

    /// Source ID for this registry
    source_id: SourceId,

    /// Loaded registry config (lazy)
    config: Option<RegistryConfig>,

    /// Cache of loaded packages by (name, version)
    packages: std::collections::HashMap<(String, String), Package>,

    /// Whether the index has been fetched this session
    index_fetched: bool,
}

impl RegistrySource {
    /// Create a new registry source.
    pub fn new(registry_url: Url, cache_dir: &Path, source_id: SourceId) -> Self {
        // Create unique directory names for this registry
        let registry_dir_name = sanitize_url_for_path(&registry_url);

        let index_path = cache_dir.join("registry").join(&registry_dir_name);
        let src_cache_path = cache_dir.join("registry-src").join(&registry_dir_name);

        RegistrySource {
            registry_url,
            index_path,
            src_cache_path,
            source_id,
            config: None,
            packages: std::collections::HashMap::new(),
            index_fetched: false,
        }
    }

    /// Create a registry source from a local directory path.
    ///
    /// This is used for CI verification where the registry is already cloned
    /// locally (e.g., in a GitHub Actions checkout). The index is not fetched
    /// from a remote URL; instead, the local path is used directly.
    ///
    /// # Arguments
    ///
    /// * `registry_path` - Path to the local registry directory
    /// * `cache_dir` - Cache directory for fetched package sources
    ///
    /// # Example
    ///
    /// ```ignore
    /// let source = RegistrySource::from_path(
    ///     Path::new("/checkout/harbour-registry"),
    ///     Path::new("/tmp/harbour-cache"),
    /// )?;
    /// ```
    pub fn from_path(registry_path: &Path, cache_dir: &Path) -> Result<Self> {
        // Verify the path exists and has a config.toml
        let config_path = registry_path.join("config.toml");
        if !config_path.exists() {
            bail!(
                "not a valid registry directory: {} (missing config.toml)",
                registry_path.display()
            );
        }

        // Create a file:// URL for the registry path
        let registry_url = Url::from_file_path(registry_path)
            .map_err(|_| anyhow::anyhow!("invalid registry path: {}", registry_path.display()))?;

        let source_id = SourceId::for_registry(&registry_url)?;

        let src_cache_path = cache_dir.join("registry-src").join("local");

        let mut source = RegistrySource {
            registry_url,
            index_path: registry_path.to_path_buf(),
            src_cache_path,
            source_id,
            config: None,
            packages: std::collections::HashMap::new(),
            index_fetched: true, // Mark as fetched since we're using a local path
        };

        // Load the config immediately
        source.load_config()?;

        Ok(source)
    }

    /// Fetch or update the registry index.
    fn fetch_index(&mut self) -> Result<()> {
        if self.index_fetched {
            return Ok(());
        }

        if self.index_path.exists() {
            self.update_index()?;
        } else {
            self.clone_index()?;
        }

        self.index_fetched = true;
        self.load_config()?;

        Ok(())
    }

    /// Clone the registry index.
    fn clone_index(&self) -> Result<()> {
        tracing::info!("Cloning registry index from {}", self.registry_url);

        std::fs::create_dir_all(self.index_path.parent().unwrap())?;

        Repository::clone(self.registry_url.as_str(), &self.index_path).with_context(|| {
            format!("failed to clone registry index from {}", self.registry_url)
        })?;

        Ok(())
    }

    /// Update the registry index.
    fn update_index(&self) -> Result<()> {
        tracing::info!("Updating registry index from {}", self.registry_url);

        let repo =
            Repository::open(&self.index_path).with_context(|| "failed to open registry index")?;

        // Fetch from remote
        let mut remote = repo.find_remote("origin")?;
        remote.fetch(&["refs/heads/*:refs/heads/*"], None, None)?;

        // Reset to origin/HEAD or origin/main
        let head = repo.head()?;
        let commit = head.peel_to_commit()?;
        repo.reset(commit.as_object(), ResetType::Hard, None)?;

        Ok(())
    }

    /// Load the registry configuration.
    fn load_config(&mut self) -> Result<()> {
        let config_path = self.index_path.join("config.toml");

        if !config_path.exists() {
            bail!(
                "registry index missing config.toml at {}",
                config_path.display()
            );
        }

        self.config = Some(RegistryConfig::load(&config_path)?);
        Ok(())
    }

    /// Get the registry configuration.
    ///
    /// Returns `None` if the config hasn't been loaded yet.
    pub fn config(&self) -> Option<&RegistryConfig> {
        self.config.as_ref()
    }

    /// Get the index path (local clone of registry).
    pub fn index_path(&self) -> &Path {
        &self.index_path
    }

    /// Get the path to a shim file for a package.
    ///
    /// Uses computed path algorithm: `index/<first_letter>/<name>/<version>.toml`
    /// NO directory scanning - O(1) lookup.
    fn get_shim_path(&self, name: &str, version: &str) -> Result<PathBuf> {
        let relative = shim_path(name, version)?;
        Ok(self.index_path.join("index").join(relative))
    }

    /// Load a shim file for a specific package version.
    ///
    /// Returns `Ok(Some(shim))` if the shim exists and is valid,
    /// `Ok(None)` if the shim file doesn't exist,
    /// or an error if the shim exists but is invalid.
    pub fn load_shim(&self, name: &str, version: &str) -> Result<Option<Shim>> {
        let shim_path = self.get_shim_path(name, version)?;

        if !shim_path.exists() {
            return Ok(None);
        }

        let shim = Shim::load(&shim_path)?;

        // Verify shim matches requested package
        if shim.package.name != name {
            bail!(
                "shim file name mismatch: expected '{}', found '{}'",
                name,
                shim.package.name
            );
        }
        if shim.package.version != version {
            bail!(
                "shim file version mismatch: expected '{}', found '{}'",
                version,
                shim.package.version
            );
        }

        Ok(Some(shim))
    }

    /// Fetch the actual package source based on a shim.
    fn fetch_package_source(&self, shim: &Shim) -> Result<PathBuf> {
        let source_dir = self.get_source_cache_path(shim);

        // Check if source is already cached.
        // For git sources, check for .git directory.
        // For tarball sources, check if directory is non-empty.
        if source_dir.exists() {
            if shim.is_git() && source_dir.join(".git").exists() {
                return Ok(source_dir);
            } else if shim.is_tarball() {
                // For tarballs, check if directory has any content
                if let Ok(mut entries) = std::fs::read_dir(&source_dir) {
                    if entries.next().is_some() {
                        return Ok(source_dir);
                    }
                }
            }
        }

        // Create the source directory
        std::fs::create_dir_all(&source_dir)?;

        if let Some(git) = &shim.source.git {
            self.fetch_git_source(git, &source_dir)?;
        } else if let Some(tarball) = &shim.source.tarball {
            self.fetch_tarball_source(tarball, &source_dir)?;
        } else {
            bail!("shim has no source specified");
        }

        // Apply patches if any
        if !shim.patches.is_empty() {
            self.apply_patches(shim, &source_dir)?;
        }

        Ok(source_dir)
    }

    /// Get the cache path for a package source.
    fn get_source_cache_path(&self, shim: &Shim) -> PathBuf {
        let source_hash = shim.source_hash();
        self.src_cache_path
            .join(&shim.package.name)
            .join(&shim.package.version)
            .join(source_hash)
    }

    /// Fetch a git source.
    fn fetch_git_source(&self, git: &shim::GitSource, dest: &Path) -> Result<()> {
        tracing::info!("Fetching git source from {} at {}", git.url, &git.rev[..8]);

        let repo = Repository::clone(&git.url, dest)
            .with_context(|| format!("failed to clone {}", git.url))?;

        // Checkout specific commit
        let oid = git2::Oid::from_str(&git.rev)?;
        let commit = repo.find_commit(oid)?;
        repo.reset(commit.as_object(), ResetType::Hard, None)?;

        Ok(())
    }

    /// Fetch a tarball source.
    ///
    /// Downloads the tarball from the URL, verifies the SHA256 hash,
    /// and extracts the contents to the destination directory.
    fn fetch_tarball_source(&self, tarball: &shim::TarballSource, dest: &Path) -> Result<()> {
        tracing::info!("Fetching tarball from {}", tarball.url);

        // Download the tarball to a temporary file
        let response = reqwest::blocking::get(&tarball.url)
            .with_context(|| format!("failed to download tarball from {}", tarball.url))?;

        if !response.status().is_success() {
            bail!(
                "failed to download tarball from {}: HTTP {}",
                tarball.url,
                response.status()
            );
        }

        let tarball_bytes = response
            .bytes()
            .with_context(|| "failed to read tarball response body")?;

        // Verify SHA256 hash
        let actual_hash = crate::util::hash::sha256_bytes(&tarball_bytes);
        if actual_hash != tarball.sha256 {
            bail!(
                "tarball hash mismatch for {}:\n  expected: {}\n  actual:   {}",
                tarball.url,
                tarball.sha256,
                actual_hash
            );
        }

        tracing::debug!("Tarball hash verified: {}", &actual_hash[..16]);

        // Extract the tarball
        extract_tarball(&tarball_bytes, dest, tarball.strip_prefix.as_deref())
            .with_context(|| format!("failed to extract tarball from {}", tarball.url))?;

        tracing::info!(
            "Extracted tarball to {} (strip_prefix: {:?})",
            dest.display(),
            tarball.strip_prefix
        );

        Ok(())
    }

    /// Apply patches to a fetched source.
    fn apply_patches(&self, shim: &Shim, source_dir: &Path) -> Result<()> {
        // Patches can only be applied to git sources
        if !shim.is_git() {
            bail!("patches can only be applied to git sources, not tarballs");
        }

        // Get the shim directory to resolve relative patch paths
        let shim_path = self.get_shim_path(&shim.package.name, &shim.package.version)?;
        let shim_dir = shim_path.parent().unwrap();

        for patch in &shim.patches {
            let patch_path = shim_dir.join(&patch.file);

            if !patch_path.exists() {
                bail!(
                    "patch file not found: {} (expected at {})",
                    patch.file,
                    patch_path.display()
                );
            }

            // Verify patch hash
            shim::verify_patch_hash(&patch_path, &patch.sha256)?;

            // Apply patch using git apply
            self.apply_single_patch(&patch_path, source_dir)?;
        }

        Ok(())
    }

    /// Apply a single patch file.
    fn apply_single_patch(&self, patch_path: &Path, source_dir: &Path) -> Result<()> {
        tracing::info!("Applying patch: {}", patch_path.display());

        // First, verify the patch will apply cleanly
        let check_output = std::process::Command::new("git")
            .args(["apply", "--check"])
            .arg(patch_path)
            .current_dir(source_dir)
            .output()
            .with_context(|| "failed to run git apply --check")?;

        if !check_output.status.success() {
            let stderr = String::from_utf8_lossy(&check_output.stderr);
            bail!(
                "patch '{}' will not apply cleanly:\n{}",
                patch_path.display(),
                stderr
            );
        }

        // Apply the patch
        let apply_output = std::process::Command::new("git")
            .arg("apply")
            .arg(patch_path)
            .current_dir(source_dir)
            .output()
            .with_context(|| "failed to run git apply")?;

        if !apply_output.status.success() {
            let stderr = String::from_utf8_lossy(&apply_output.stderr);
            bail!(
                "failed to apply patch '{}':\n{}",
                patch_path.display(),
                stderr
            );
        }

        Ok(())
    }

    /// Load a package from a fetched source.
    fn load_package_from_source(&self, shim: &Shim, source_dir: &Path) -> Result<Package> {
        // Check for manifest
        let manifest_path = source_dir.join("Harbor.toml");

        let manifest = if manifest_path.exists() {
            // Warn if shim has surface overrides and source has manifest
            if shim.effective_surface_override().is_some() {
                tracing::warn!(
                    "package '{}' has both shim surface overrides and Harbor.toml; \
                     shim surface will override upstream",
                    shim.package.name
                );
            }
            Manifest::load(&manifest_path)?
        } else if let Some(surface_override) = shim.effective_surface_override() {
            // Create synthetic manifest from shim surface override
            self.create_synthetic_manifest(shim, &surface_override)?
        } else {
            bail!(
                "package '{}' has no Harbor.toml and no shim surface override",
                shim.package.name
            );
        };

        // Create package with registry source ID
        let _version: semver::Version = shim.package.version.parse()?;
        let precise_source = self.source_id.with_precise(&shim.source_hash());

        Package::with_source_id(manifest, source_dir.to_path_buf(), precise_source)
    }

    /// Create a synthetic manifest for bootstrap packages without Harbor.toml.
    fn create_synthetic_manifest(
        &self,
        shim: &Shim,
        surface_override: &shim::ShimSurfaceOverride,
    ) -> Result<Manifest> {
        use crate::core::manifest::PackageMetadata;
        use crate::core::surface::{
            CompileRequirements, Define, LibRef, LinkRequirements, Surface,
        };
        use crate::core::target::Target;

        // Create package metadata
        let package = PackageMetadata {
            name: shim.package.name.clone(),
            version: shim.package.version.clone(),
            description: shim.metadata().and_then(|m| m.category.clone()),
            authors: Vec::new(),
            license: shim.metadata().and_then(|m| m.license.clone()),
            repository: shim.metadata().and_then(|m| m.upstream_url.clone()),
            homepage: None,
            documentation: None,
            keywords: Vec::new(),
            categories: Vec::new(),
        };

        // Build the surface from override
        let mut surface = Surface::default();

        // Set compile surface
        if let Some(compile) = &surface_override.compile {
            if let Some(public) = &compile.public {
                surface.compile.public = CompileRequirements {
                    include_dirs: public
                        .include_dirs
                        .iter()
                        .map(std::path::PathBuf::from)
                        .collect(),
                    defines: public.defines.iter().map(|d| Define::flag(d)).collect(),
                    cflags: Vec::new(),
                };
            }
        }

        // Set link surface
        if let Some(link) = &surface_override.link {
            if let Some(public) = &link.public {
                let libs: Vec<LibRef> = public
                    .libs
                    .iter()
                    .map(|lib| match lib.kind.as_str() {
                        "framework" => LibRef::framework(&lib.name),
                        _ => LibRef::system(&lib.name),
                    })
                    .collect();

                surface.link.public = LinkRequirements {
                    libs,
                    ldflags: Vec::new(),
                    groups: Vec::new(),
                    frameworks: Vec::new(),
                };
            }
        }

        // Create a synthetic library target
        let mut target = Target::staticlib(&shim.package.name);

        // Determine language from harness config
        let is_cxx = shim
            .harness()
            .map(|h| h.lang == "cxx" || h.lang == "c++")
            .unwrap_or(false);

        if is_cxx {
            target.lang = crate::core::target::Language::Cxx;
        }

        // Use sources from shim if provided, otherwise use conservative defaults
        if !surface_override.sources.is_empty() {
            target.sources = surface_override.sources.clone();
        } else {
            // Default: only root level and src/ files to avoid test/contrib directories
            if is_cxx {
                target.sources = vec![
                    "*.c".to_string(),
                    "*.cpp".to_string(),
                    "src/*.c".to_string(),
                    "src/*.cpp".to_string(),
                ];
            } else {
                target.sources = vec!["*.c".to_string(), "src/*.c".to_string()];
            }
        }
        target.surface = surface;

        Ok(Manifest {
            package: Some(package),
            workspace: None,
            dependencies: std::collections::HashMap::new(),
            targets: vec![target],
            profiles: std::collections::HashMap::new(),
            build: crate::core::manifest::BuildConfig::default(),
            manifest_dir: std::path::PathBuf::new(),
        })
    }

    /// Compute the shim file hash for lockfile provenance.
    #[allow(dead_code)] // Will be used when lockfile provenance is implemented
    fn compute_shim_hash(&self, name: &str, version: &str) -> Result<String> {
        let shim_path = self.get_shim_path(name, version)?;
        sha256_file(&shim_path)
    }

    /// List all available versions for a package by scanning the directory.
    ///
    /// This is used when a version range or wildcard is specified.
    fn list_available_versions(&self, name: &str) -> Result<Vec<String>> {
        let first_char = name
            .chars()
            .next()
            .ok_or_else(|| anyhow::anyhow!("invalid empty package name"))?;

        // Path: index/<first_char>/<name>/
        let package_dir = self
            .index_path
            .join("index")
            .join(first_char.to_string())
            .join(name);

        if !package_dir.exists() {
            return Ok(vec![]);
        }

        let mut versions = Vec::new();

        for entry in std::fs::read_dir(&package_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.extension().map_or(false, |ext| ext == "toml") {
                if let Some(stem) = path.file_stem() {
                    let version_str = stem.to_string_lossy().to_string();
                    // Validate it's a valid semver version
                    if semver::Version::parse(&version_str).is_ok() {
                        versions.push(version_str);
                    }
                }
            }
        }

        // Sort versions (newest first for better resolver behavior)
        versions.sort_by(|a, b| {
            let va: semver::Version = a.parse().unwrap();
            let vb: semver::Version = b.parse().unwrap();
            vb.cmp(&va)
        });

        Ok(versions)
    }
}

impl Source for RegistrySource {
    fn name(&self) -> &str {
        "registry"
    }

    fn supports(&self, dep: &Dependency) -> bool {
        dep.source_id().is_registry() && dep.source_id().url() == self.source_id.url()
    }

    fn query(&mut self, dep: &Dependency) -> Result<Vec<Summary>> {
        if !self.supports(dep) {
            return Ok(vec![]);
        }

        // Ensure index is fetched
        self.fetch_index()?;

        let name = dep.name().as_str();

        // Try to extract a specific version from the version requirement
        let version_str = extract_specific_version(dep.version_req());

        if let Some(version) = version_str {
            // Exact version - use O(1) lookup
            if let Some(shim) = self.load_shim(name, &version)? {
                // Check name match
                if shim.package.name != name {
                    return Ok(vec![]);
                }

                // Check version matches requirement
                let version: semver::Version = shim.package.version.parse()?;
                if !dep.matches_version(&version) {
                    return Ok(vec![]);
                }

                // Fetch the actual source
                let source_dir = self.fetch_package_source(&shim)?;

                // Load the package
                let package = self.load_package_from_source(&shim, &source_dir)?;

                // Cache it
                self.packages.insert(
                    (name.to_string(), shim.package.version.clone()),
                    package.clone(),
                );

                return Ok(vec![package.summary()?]);
            }
        } else {
            // Wildcard or range - scan directory for available versions
            let available_versions = self.list_available_versions(name)?;
            let mut summaries = Vec::new();

            for version_str in available_versions {
                let version: semver::Version = match version_str.parse() {
                    Ok(v) => v,
                    Err(_) => continue, // Skip invalid versions
                };

                if !dep.matches_version(&version) {
                    continue;
                }

                if let Some(shim) = self.load_shim(name, &version_str)? {
                    // Fetch the actual source
                    let source_dir = self.fetch_package_source(&shim)?;

                    // Load the package
                    let package = self.load_package_from_source(&shim, &source_dir)?;

                    // Cache it
                    self.packages.insert(
                        (name.to_string(), shim.package.version.clone()),
                        package.clone(),
                    );

                    summaries.push(package.summary()?);
                }
            }

            return Ok(summaries);
        }

        Ok(vec![])
    }

    fn ensure_ready(&mut self) -> Result<()> {
        self.fetch_index()
    }

    fn get_package_path(&self, pkg_id: PackageId) -> Result<&Path> {
        let name = pkg_id.name().as_str();
        let version = pkg_id.version().to_string();

        // Return cached path if we have the package loaded
        if let Some(pkg) = self.packages.get(&(name.to_string(), version.clone())) {
            return Ok(pkg.root());
        }

        // Otherwise we need to load it first
        bail!(
            "package {} {} not loaded; call load_package first",
            name,
            version
        );
    }

    fn load_package(&mut self, pkg_id: PackageId) -> Result<Package> {
        let name = pkg_id.name().as_str();
        let version = pkg_id.version().to_string();

        // Check cache first
        if let Some(pkg) = self.packages.get(&(name.to_string(), version.clone())) {
            return Ok(pkg.clone());
        }

        // Ensure index is fetched
        self.fetch_index()?;

        // Load shim
        let shim = self.load_shim(name, &version)?.ok_or_else(|| {
            anyhow::anyhow!(
                "package `{}` version `{}` not found in registry\n  \
                     --> shim not found at: {}\n\
                     help: verify package exists; `harbour search` not yet implemented",
                name,
                version,
                shim_path(name, &version).unwrap_or_else(|_| "?".to_string())
            )
        })?;

        // Fetch source
        let source_dir = self.fetch_package_source(&shim)?;

        // Load package
        let package = self.load_package_from_source(&shim, &source_dir)?;

        // Verify name and version match
        if package.name() != pkg_id.name() || package.version() != pkg_id.version() {
            bail!(
                "package mismatch: expected {} {}, found {} {}",
                pkg_id.name(),
                pkg_id.version(),
                package.name(),
                package.version()
            );
        }

        // Cache it
        self.packages
            .insert((name.to_string(), version), package.clone());

        Ok(package)
    }

    fn is_cached(&self, pkg_id: PackageId) -> bool {
        let name = pkg_id.name().as_str();
        let version = pkg_id.version().to_string();

        // Check if we have it in memory
        if self
            .packages
            .contains_key(&(name.to_string(), version.clone()))
        {
            return true;
        }

        // Check if the shim path exists and source is fetched
        if let Ok(shim_path) = self.get_shim_path(name, &version) {
            if shim_path.exists() {
                // Try to load shim and check source cache
                if let Ok(Some(shim)) = self.load_shim(name, &version) {
                    let source_dir = self.get_source_cache_path(&shim);
                    if source_dir.exists() {
                        // For git sources, check for .git directory
                        // For tarball sources, check if directory has content
                        if shim.is_git() {
                            return source_dir.join(".git").exists();
                        } else if shim.is_tarball() {
                            if let Ok(mut entries) = std::fs::read_dir(&source_dir) {
                                return entries.next().is_some();
                            }
                        }
                    }
                }
            }
        }

        false
    }
}

/// Sanitize a URL for use as a directory name.
fn sanitize_url_for_path(url: &Url) -> String {
    let mut name = String::new();

    if let Some(host) = url.host_str() {
        name.push_str(host);
    }

    let path = url.path().trim_matches('/');
    if !path.is_empty() {
        name.push('-');
        name.push_str(&path.replace('/', "-"));
    }

    // Remove .git suffix
    if name.ends_with(".git") {
        name.truncate(name.len() - 4);
    }

    name
}

/// Try to extract a specific version from a version requirement.
///
/// This handles cases like:
/// - `=1.2.3` -> Some("1.2.3")
/// - `1.2.3` (if it's an exact match) -> Some("1.2.3")
/// - `^1.2.3` -> None (would need to scan for compatible versions)
fn extract_specific_version(req: &semver::VersionReq) -> Option<String> {
    // VersionReq doesn't expose its comparators directly in a stable way
    // We'll use string parsing as a workaround

    let req_str = req.to_string();

    // Handle exact version requirements (= prefix or bare version)
    if let Some(stripped) = req_str.strip_prefix('=') {
        let version_str = stripped.trim();
        if semver::Version::parse(version_str).is_ok() {
            return Some(version_str.to_string());
        }
    }

    // Handle bare version string that happens to be exact
    if !req_str.contains('^')
        && !req_str.contains('~')
        && !req_str.contains('>')
        && !req_str.contains('<')
        && !req_str.contains('*')
        && !req_str.contains(',')
    {
        // Might be a bare version or = prefixed
        let version_str = req_str.trim_start_matches('=').trim();
        if semver::Version::parse(version_str).is_ok() {
            return Some(version_str.to_string());
        }
    }

    None
}

/// Extract a gzip-compressed tarball to a destination directory.
///
/// Supports `.tar.gz` and `.tgz` archives. If `strip_prefix` is provided,
/// the specified prefix is stripped from all file paths during extraction.
///
/// # Arguments
///
/// * `data` - The tarball bytes
/// * `dest` - Destination directory for extracted files
/// * `strip_prefix` - Optional prefix to strip from paths (e.g., "zlib-1.3.1")
///
/// # Example
///
/// ```ignore
/// extract_tarball(&tarball_bytes, &dest_dir, Some("zlib-1.3.1"))?;
/// ```
pub fn extract_tarball(data: &[u8], dest: &Path, strip_prefix: Option<&str>) -> Result<()> {
    use flate2::read::GzDecoder;
    use std::io::Cursor;
    use tar::Archive;

    // Create a gzip decoder
    let cursor = Cursor::new(data);
    let decoder = GzDecoder::new(cursor);
    let mut archive = Archive::new(decoder);

    // Ensure destination exists
    std::fs::create_dir_all(dest)
        .with_context(|| format!("failed to create destination directory: {}", dest.display()))?;

    // Extract entries
    for entry in archive
        .entries()
        .context("failed to read tarball entries")?
    {
        let mut entry = entry.context("failed to read tarball entry")?;
        let entry_path = entry.path().context("failed to get entry path")?;
        let entry_path_str = entry_path.to_string_lossy();

        // Determine the output path, stripping prefix if specified
        let output_path = if let Some(prefix) = strip_prefix {
            // Normalize path separators for comparison
            let normalized_path = entry_path_str.replace('\\', "/");
            let normalized_prefix = prefix.trim_end_matches('/');

            // Strip the prefix if it matches
            let stripped = if normalized_path.starts_with(&format!("{}/", normalized_prefix)) {
                normalized_path
                    .strip_prefix(&format!("{}/", normalized_prefix))
                    .unwrap()
                    .to_string()
            } else if normalized_path == normalized_prefix {
                // Entry is the prefix directory itself, skip it
                continue;
            } else {
                // Path doesn't start with prefix, use as-is
                // This handles cases where some files might be outside the prefix
                normalized_path.to_string()
            };

            // Skip empty paths (would result from stripping just the prefix dir)
            if stripped.is_empty() {
                continue;
            }

            dest.join(stripped)
        } else {
            dest.join(entry_path.as_ref())
        };

        // Security check: ensure path is within destination
        let canonical_dest = dest.canonicalize().unwrap_or_else(|_| dest.to_path_buf());
        if let Ok(canonical_output) = output_path.canonicalize() {
            if !canonical_output.starts_with(&canonical_dest) {
                bail!(
                    "tarball entry escapes destination directory: {}",
                    entry_path_str
                );
            }
        }

        // Create parent directories if needed
        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create directory: {}", parent.display()))?;
        }

        // Extract based on entry type
        let entry_type = entry.header().entry_type();
        match entry_type {
            tar::EntryType::Directory => {
                std::fs::create_dir_all(&output_path).with_context(|| {
                    format!("failed to create directory: {}", output_path.display())
                })?;
            }
            tar::EntryType::Regular | tar::EntryType::Continuous => {
                // Extract the file
                entry.unpack(&output_path).with_context(|| {
                    format!("failed to extract file: {}", output_path.display())
                })?;
            }
            tar::EntryType::Symlink => {
                // Handle symlinks (on platforms that support them)
                #[cfg(unix)]
                {
                    if let Ok(link_target) = entry.link_name() {
                        if let Some(target) = link_target {
                            std::os::unix::fs::symlink(target.as_ref(), &output_path)
                                .with_context(|| {
                                    format!("failed to create symlink: {}", output_path.display())
                                })?;
                        }
                    }
                }
                #[cfg(windows)]
                {
                    // On Windows, skip symlinks or copy the target file
                    tracing::debug!("Skipping symlink on Windows: {}", entry_path_str);
                }
            }
            tar::EntryType::Link => {
                // Hard links - extract as regular file
                entry.unpack(&output_path).with_context(|| {
                    format!("failed to extract hard link: {}", output_path.display())
                })?;
            }
            _ => {
                // Skip other entry types (fifos, char devices, etc.)
                tracing::debug!(
                    "Skipping unsupported entry type {:?}: {}",
                    entry_type,
                    entry_path_str
                );
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_sanitize_url() {
        let url = Url::parse("https://github.com/harbour-project/registry.git").unwrap();
        assert_eq!(
            sanitize_url_for_path(&url),
            "github.com-harbour-project-registry"
        );

        let url2 = Url::parse("https://example.com/my/registry").unwrap();
        assert_eq!(sanitize_url_for_path(&url2), "example.com-my-registry");
    }

    #[test]
    fn test_extract_specific_version() {
        // Exact versions (with = prefix)
        assert_eq!(
            extract_specific_version(&"=1.2.3".parse().unwrap()),
            Some("1.2.3".to_string())
        );

        // Note: bare "1.2.3" is parsed by semver as "^1.2.3" (caret semantics),
        // so we can't extract a specific version from it
        assert_eq!(extract_specific_version(&"1.2.3".parse().unwrap()), None);

        // Range versions - can't extract specific
        assert_eq!(extract_specific_version(&"^1.2.3".parse().unwrap()), None);
        assert_eq!(extract_specific_version(&"~1.2.3".parse().unwrap()), None);
        assert_eq!(extract_specific_version(&">=1.0".parse().unwrap()), None);
        assert_eq!(extract_specific_version(&"*".parse().unwrap()), None);
    }

    #[test]
    fn test_registry_source_paths() {
        let tmp = TempDir::new().unwrap();
        let url = Url::parse("https://github.com/harbour-project/registry").unwrap();
        let source_id = SourceId::for_registry(&url).unwrap();

        let source = RegistrySource::new(url, tmp.path(), source_id);

        // Verify path structure
        assert!(source.index_path.to_string_lossy().contains("registry"));
        assert!(source
            .src_cache_path
            .to_string_lossy()
            .contains("registry-src"));
    }

    #[test]
    fn test_extract_tarball_basic() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;
        use tar::Builder;

        // Create a simple tarball in memory
        let mut tar_data = Vec::new();
        {
            let encoder = GzEncoder::new(&mut tar_data, Compression::default());
            let mut builder = Builder::new(encoder);

            // Add a simple file
            let mut header = tar::Header::new_gnu();
            header.set_path("test.txt").unwrap();
            header.set_size(5);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append(&header, std::io::Cursor::new(b"hello"))
                .unwrap();

            builder.finish().unwrap();
        }

        // Extract to temp directory
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("extracted");

        extract_tarball(&tar_data, &dest, None).unwrap();

        // Verify file was extracted
        let content = std::fs::read_to_string(dest.join("test.txt")).unwrap();
        assert_eq!(content, "hello");
    }

    #[test]
    fn test_extract_tarball_with_strip_prefix() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;
        use tar::Builder;

        // Create a tarball with a prefix directory
        let mut tar_data = Vec::new();
        {
            let encoder = GzEncoder::new(&mut tar_data, Compression::default());
            let mut builder = Builder::new(encoder);

            // Add a directory entry
            let mut header = tar::Header::new_gnu();
            header.set_path("mylib-1.0.0/").unwrap();
            header.set_size(0);
            header.set_mode(0o755);
            header.set_entry_type(tar::EntryType::Directory);
            header.set_cksum();
            builder.append(&header, std::io::empty()).unwrap();

            // Add a file inside the directory
            let mut header = tar::Header::new_gnu();
            header.set_path("mylib-1.0.0/src/main.c").unwrap();
            header.set_size(13);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append(&header, std::io::Cursor::new(b"int main() {}"))
                .unwrap();

            builder.finish().unwrap();
        }

        // Extract with strip_prefix
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("extracted");

        extract_tarball(&tar_data, &dest, Some("mylib-1.0.0")).unwrap();

        // Verify file was extracted without prefix
        let content = std::fs::read_to_string(dest.join("src/main.c")).unwrap();
        assert_eq!(content, "int main() {}");

        // The prefix directory itself should not exist
        assert!(!dest.join("mylib-1.0.0").exists());
    }
}
