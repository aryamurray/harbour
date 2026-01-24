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

        Repository::clone(self.registry_url.as_str(), &self.index_path)
            .with_context(|| format!("failed to clone registry index from {}", self.registry_url))?;

        Ok(())
    }

    /// Update the registry index.
    fn update_index(&self) -> Result<()> {
        tracing::info!("Updating registry index from {}", self.registry_url);

        let repo = Repository::open(&self.index_path)
            .with_context(|| "failed to open registry index")?;

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

    /// Get the path to a shim file for a package.
    ///
    /// Uses computed path algorithm: `<first_letter>/<name>/<version>.toml`
    /// NO directory scanning - O(1) lookup.
    fn get_shim_path(&self, name: &str, version: &str) -> Result<PathBuf> {
        let relative = shim_path(name, version)?;
        Ok(self.index_path.join(relative))
    }

    /// Load a shim file for a specific package version.
    fn load_shim(&self, name: &str, version: &str) -> Result<Option<Shim>> {
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

        if source_dir.exists() && source_dir.join("Harbor.toml").exists() {
            // Already fetched
            return Ok(source_dir);
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
    fn fetch_tarball_source(&self, tarball: &shim::TarballSource, _dest: &Path) -> Result<()> {
        // For now, tarball sources are not implemented
        // This would require HTTP download, hash verification, and extraction
        bail!(
            "tarball sources not yet implemented (url: {}); use git source instead",
            tarball.url
        );
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
    fn load_package_from_source(
        &self,
        shim: &Shim,
        source_dir: &Path,
    ) -> Result<Package> {
        // Check for manifest
        let manifest_path = source_dir.join("Harbor.toml");

        let manifest = if manifest_path.exists() {
            // Warn if shim has surface overrides and source has manifest
            if shim.surface.is_some() {
                tracing::warn!(
                    "package '{}' has both shim surface overrides and Harbor.toml; \
                     shim surface will override upstream",
                    shim.package.name
                );
            }
            Manifest::load(&manifest_path)?
        } else if let Some(ref surface) = shim.surface {
            // Create synthetic manifest from shim surface
            self.create_synthetic_manifest(shim, surface)?
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
        _surface: &shim::ShimSurface,
    ) -> Result<Manifest> {
        // This creates a minimal manifest with the surface from the shim
        // For now, we create a minimal stub - full implementation would need
        // to properly construct targets and surface fields
        bail!(
            "synthetic manifest creation not yet implemented for package '{}'; \
             the upstream package needs a Harbor.toml",
            shim.package.name
        );
    }

    /// Compute the shim file hash for lockfile provenance.
    #[allow(dead_code)] // Will be used when lockfile provenance is implemented
    fn compute_shim_hash(&self, name: &str, version: &str) -> Result<String> {
        let shim_path = self.get_shim_path(name, version)?;
        sha256_file(&shim_path)
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

        // We need to find versions that match the dependency's version requirement.
        // Since we don't scan directories, we need a specific version.
        // For registry deps with version ranges, the resolver should have
        // already determined the specific version to use.
        //
        // If version_req is "*", we can't know which version to load.
        // In practice, the resolver should provide a specific version.

        // Try to extract a specific version from the version requirement
        let version_str = extract_specific_version(dep.version_req());

        if let Some(version) = version_str {
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
        let shim = self
            .load_shim(name, &version)?
            .ok_or_else(|| {
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
        if self.packages.contains_key(&(name.to_string(), version.clone())) {
            return true;
        }

        // Check if the shim path exists and source is fetched
        if let Ok(shim_path) = self.get_shim_path(name, &version) {
            if shim_path.exists() {
                // Try to load shim and check source cache
                if let Ok(Some(shim)) = self.load_shim(name, &version) {
                    let source_dir = self.get_source_cache_path(&shim);
                    return source_dir.exists() && source_dir.join("Harbor.toml").exists();
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
        assert!(source.src_cache_path.to_string_lossy().contains("registry-src"));
    }
}
