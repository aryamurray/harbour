//! Registry source - packages from a package registry, over a pluggable transport.
//!
//! A registry is a collection of shim files that reference actual package
//! sources, plus a tier-1 index that carries just enough metadata to
//! resolve dependencies without fetching any of those sources. This
//! enables centralized package discovery while keeping source code
//! distributed - and, since resolution never needs to fetch a source, lets
//! the same index be served over a CDN with no directory listing.
//!
//! # Two tiers
//!
//! - **Tier 1** (`index.rs`): one record per version, one file per package
//!   (`index/<shard>/<name>.idx`), carrying name/version/`yanked`/deps/
//!   checksum/shim-pointer - everything [`Source::query`] needs.
//! - **Tier 2** (`shim.rs`): the existing per-version shim
//!   (`index/<shard>/<name>/<version>.toml`) - the full build recipe.
//!   Fetched only once a version is actually selected.
//!
//! # Transport
//!
//! How those bytes are actually fetched is abstracted by
//! [`transport::RegistryTransport`] (`transport.rs`). Today the only
//! implementation is [`transport::GitTransport`], which reads them out of
//! a local git clone; a future sparse-HTTP transport implements the same
//! trait with no changes needed here.
//!
//! # Registry Structure
//!
//! ```text
//! registry/
//! ├── config.toml                # Registry metadata
//! └── index/
//!     ├── z/
//!     │   ├── zlib.idx           # Tier-1 index (all versions of zlib)
//!     │   └── zlib/
//!     │       ├── 1.3.1.toml     # Tier-2 shim
//!     │       └── patches/
//!     │           └── fix-cmake.patch
//!     └── s/
//!         ├── sqlite.idx
//!         └── sqlite/
//!             └── 3.45.0.toml
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
pub mod generate;
pub mod index;
pub mod shim;
pub mod transport;

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use url::Url;

use crate::core::workspace::{find_manifest, ManifestError};
use crate::core::{Dependency, Manifest, Package, PackageId, SourceId, Summary};
use crate::sources::Source;
use crate::util::hash::sha256_file;

pub use config::RegistryConfig;
pub use generate::generate_index;
pub use index::{IndexDependency, IndexDependencyKind, IndexRecord};
pub use shim::{shim_path, validate_package_name, Shim, ShimPatch};
pub use transport::{GitTransport, RegistryTransport};

/// A source for registry dependencies.
///
/// `RegistrySource` drives resolution and package loading purely in terms
/// of [`RegistryTransport`]; it holds no transport-specific state itself.
pub struct RegistrySource {
    /// How index/shim/artifact bytes are actually fetched.
    transport: Box<dyn RegistryTransport>,

    /// Source ID for this registry.
    source_id: SourceId,

    /// Loaded registry config (lazy).
    config: Option<RegistryConfig>,

    /// Cache of loaded packages by (name, version).
    packages: std::collections::HashMap<(String, String), Package>,
}

impl RegistrySource {
    /// Create a new registry source that fetches via git.
    pub fn new(registry_url: Url, cache_dir: &Path, source_id: SourceId) -> Self {
        RegistrySource {
            transport: Box::new(transport::GitTransport::new(registry_url, cache_dir)),
            source_id,
            config: None,
            packages: std::collections::HashMap::new(),
        }
    }

    /// Create a registry source backed by an arbitrary transport.
    ///
    /// This is the extension point a second transport (e.g. sparse HTTP)
    /// plugs into: build whatever `impl RegistryTransport` it needs and
    /// hand it here.
    pub fn with_transport(transport: Box<dyn RegistryTransport>, source_id: SourceId) -> Self {
        RegistrySource {
            transport,
            source_id,
            config: None,
            packages: std::collections::HashMap::new(),
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

        let transport = transport::GitTransport::from_local_path(registry_path, cache_dir)?;
        let source_id = SourceId::for_registry(transport.registry_url())?;

        let mut source = RegistrySource {
            transport: Box::new(transport),
            source_id,
            config: None,
            packages: std::collections::HashMap::new(),
        };

        source.load_config()?;

        Ok(source)
    }

    /// Load the registry configuration.
    fn load_config(&mut self) -> Result<()> {
        let bytes = self
            .transport
            .fetch_index_path("config.toml")?
            .ok_or_else(|| anyhow::anyhow!("registry index missing config.toml"))?;

        let content = String::from_utf8(bytes).context("registry config.toml is not UTF-8")?;
        self.config = Some(RegistryConfig::parse(&content)?);
        Ok(())
    }

    /// Get the registry configuration.
    ///
    /// Returns `None` if the config hasn't been loaded yet.
    pub fn config(&self) -> Option<&RegistryConfig> {
        self.config.as_ref()
    }

    /// Get the index path (local clone of registry), if the transport in
    /// use has one.
    ///
    /// Only meaningful for the git transport; kept for callers (e.g. the
    /// verification tooling) that walk the clone directly.
    pub fn index_path(&self) -> Option<&Path> {
        self.transport
            .as_any()
            .downcast_ref::<transport::GitTransport>()
            .map(transport::GitTransport::index_path)
    }

    /// Load a shim file for a specific package version.
    ///
    /// Returns `Ok(Some(shim))` if the shim exists and is valid,
    /// `Ok(None)` if the shim file doesn't exist,
    /// or an error if the shim exists but is invalid.
    pub fn load_shim(&mut self, name: &str, version: &str) -> Result<Option<Shim>> {
        let relative = format!("index/{}", shim_path(name, version)?);

        let Some(bytes) = self.transport.fetch_index_path(&relative)? else {
            return Ok(None);
        };

        let content = String::from_utf8(bytes)
            .with_context(|| format!("shim file is not UTF-8: {relative}"))?;
        let shim = Shim::parse(&content, Path::new(&relative))?;

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

    /// Load a package's tier-1 index, if the package has one.
    ///
    /// Returns `Ok(None)` if the package has no tier-1 index at all (i.e.
    /// it does not exist in this registry).
    fn load_index(&mut self, name: &str) -> Result<Option<Vec<IndexRecord>>> {
        let relative = format!("index/{}", index::index_path(name)?);

        let Some(bytes) = self.transport.fetch_index_path(&relative)? else {
            return Ok(None);
        };

        index::parse_index(&bytes, &relative).map(Some)
    }

    /// Fetch the actual package source based on a shim.
    fn fetch_package_source(&mut self, shim: &Shim) -> Result<PathBuf> {
        let relative_shim_path = format!(
            "index/{}",
            shim_path(&shim.package.name, &shim.package.version)?
        );
        self.transport.fetch_artifact(shim, &relative_shim_path)
    }

    /// Load a package from a fetched source.
    fn load_package_from_source(&self, shim: &Shim, source_dir: &Path) -> Result<Package> {
        // Check for manifest
        let manifest_path = match find_manifest(source_dir) {
            Ok(path) => Some(path),
            Err(ManifestError::NotFound { .. }) => None,
            Err(err) => return Err(err.into()),
        };

        let manifest = if let Some(manifest_path) = manifest_path {
            // Warn if shim has surface overrides and source has manifest
            if shim.effective_surface_override().is_some() {
                tracing::warn!(
                    "package '{}' has both shim surface overrides and Harbour.toml; \
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
                "package '{}' has no Harbour.toml and no shim surface override",
                shim.package.name
            );
        };

        // Create package with registry source ID
        let _version: semver::Version = shim.package.version.parse()?;
        let precise_source = self.source_id.with_precise(shim.source_hash());

        Package::with_source_id(manifest, source_dir.to_path_buf(), precise_source)
    }

    /// Create a synthetic manifest for bootstrap packages without Harbour.toml.
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
                    defines: public.defines.iter().map(Define::flag).collect(),
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
            features: crate::core::features::FeatureMap::new(),
            manifest_dir: std::path::PathBuf::new(),
        })
    }

    /// Compute the shim file hash for lockfile provenance.
    #[allow(dead_code)] // Will be used when lockfile provenance is implemented
    fn compute_shim_hash(&self, name: &str, version: &str) -> Result<String> {
        let relative = format!("index/{}", shim_path(name, version)?);
        let index_path = self
            .index_path()
            .ok_or_else(|| anyhow::anyhow!("compute_shim_hash requires a local clone"))?;
        sha256_file(&index_path.join(relative))
    }

    /// Build a [`Summary`] directly from a tier-1 [`IndexRecord`], with no
    /// source fetch: this is what makes resolution metadata-only.
    fn summary_from_record(&self, record: &IndexRecord) -> Result<Summary> {
        let version: semver::Version = record.version.parse().with_context(|| {
            format!(
                "invalid version '{}' in tier-1 index for '{}'",
                record.version, record.name
            )
        })?;

        let deps = record
            .deps
            .iter()
            .map(|dep| self.dependency_from_index(dep))
            .collect::<Result<Vec<_>>>()?;

        let pkg_id = PackageId::new(record.name.as_str(), version, self.source_id);
        Ok(Summary::new(pkg_id, deps, record.checksum.clone()))
    }

    /// Reconstruct a full [`Dependency`] from a tier-1 [`IndexDependency`].
    fn dependency_from_index(&self, dep: &IndexDependency) -> Result<Dependency> {
        validate_package_name(&dep.name)?;

        let source_id = match &dep.registry {
            Some(url) => SourceId::for_registry(&Url::parse(url)?)?,
            None => self.source_id,
        };

        let version_req: semver::VersionReq = dep.version_req.parse().with_context(|| {
            format!(
                "invalid version requirement '{}' for dependency '{}'",
                dep.version_req, dep.name
            )
        })?;

        Ok(Dependency::new(dep.name.as_str(), source_id)
            .with_version_req(version_req)
            .optional(dep.optional)
            .with_default_features(dep.default_features))
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

        self.ensure_ready()?;

        let name = dep.name().as_str().to_string();

        // Metadata-only: a version range or an exact version both come
        // from the same single tier-1 file read. No source is ever
        // fetched here - see the module docs and `RegistryTransport`.
        let Some(records) = self.load_index(&name)? else {
            return Ok(vec![]);
        };

        let mut summaries = Vec::new();
        for record in &records {
            if record.name != name {
                continue;
            }
            if !record.is_available() {
                continue; // yanked - excluded from new resolutions
            }

            let version: semver::Version = match record.version.parse() {
                Ok(v) => v,
                Err(_) => continue,
            };

            if !dep.matches_version(&version) {
                continue;
            }

            summaries.push(self.summary_from_record(record)?);
        }

        Ok(summaries)
    }

    fn ensure_ready(&mut self) -> Result<()> {
        self.transport.ensure_ready()?;
        if self.config.is_none() {
            self.load_config()?;
        }
        Ok(())
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
        self.ensure_ready()?;

        // Load shim (tier 2) - the build recipe. Yanked versions are not
        // filtered here: a version already selected (e.g. from a
        // lockfile) must remain loadable even after being yanked.
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
        self.packages
            .contains_key(&(name.to_string(), version.clone()))
    }
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
                    if let Ok(Some(target)) = entry.link_name() {
                        std::os::unix::fs::symlink(target.as_ref(), &output_path).with_context(
                            || format!("failed to create symlink: {}", output_path.display()),
                        )?;
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
    use crate::util::fs::sanitize_url_for_path;
    use tempfile::TempDir;

    #[test]
    fn sanitize_url_keeps_https_registry_names_readable() {
        let url = Url::parse("https://github.com/aryamurray/harbour-registry").unwrap();
        assert_eq!(
            sanitize_url_for_path(&url),
            "github.com-aryamurray-harbour-registry"
        );
    }

    #[test]
    fn sanitize_url_strips_git_suffix() {
        let url = Url::parse("https://github.com/aryamurray/harbour-registry.git").unwrap();
        assert_eq!(
            sanitize_url_for_path(&url),
            "github.com-aryamurray-harbour-registry"
        );
    }

    #[test]
    fn test_registry_source_paths() {
        let tmp = TempDir::new().unwrap();
        let url = Url::parse("https://github.com/harbour-project/registry").unwrap();
        let source_id = SourceId::for_registry(&url).unwrap();

        let source = RegistrySource::new(url, tmp.path(), source_id);

        let index_path = source
            .index_path()
            .expect("git transport has an index path");
        assert!(index_path.to_string_lossy().contains("registry"));
    }

    /// Commit a small real git repo at `dir`, containing a `pkg`
    /// staticlib manifest whose declared version is `version`, and return
    /// the commit SHA.
    fn commit_pkg_source(dir: &Path, version: &str) -> String {
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("Harbour.toml"),
            format!(
                "[package]\nname = \"pkg\"\nversion = \"{version}\"\n\n\
                 [targets.pkg]\nkind = \"staticlib\"\nsources = [\"src/lib.c\"]\n"
            ),
        )
        .unwrap();
        std::fs::write(dir.join("src/lib.c"), "int x;\n").unwrap();

        let repo = git2::Repository::init(dir).unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("Test", "test@example.invalid").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap()
            .to_string()
    }

    /// Build a minimal local registry directory (config.toml + tier-1 +
    /// tier-2 files) for one package with two versions - each backed by
    /// its own real git repository, matching the shim's declared version -
    /// with the newer version marked yanked in the tier-1 index.
    fn build_yank_test_registry(root: &Path) -> PathBuf {
        let registry_dir = root.join("registry");
        std::fs::create_dir_all(&registry_dir).unwrap();
        std::fs::write(
            registry_dir.join("config.toml"),
            "[registry]\nname = \"yank-test\"\nregistry_version = 1\n\
             layout = \"letter/name/version\"\n",
        )
        .unwrap();

        let shim_dir = registry_dir.join("index/p/pkg");
        std::fs::create_dir_all(&shim_dir).unwrap();

        for version in ["0.1.0", "0.2.0"] {
            let src_dir = root.join(format!("src-pkg-{version}"));
            let rev = commit_pkg_source(&src_dir, version);
            let src_url = Url::from_file_path(&src_dir).unwrap().to_string();

            std::fs::write(
                shim_dir.join(format!("{version}.toml")),
                format!(
                    "[package]\nname = \"pkg\"\nversion = \"{version}\"\n\n\
                     [source.git]\nurl = \"{src_url}\"\nrev = \"{rev}\"\n"
                ),
            )
            .unwrap();
        }

        let records = vec![
            IndexRecord {
                format_version: index::CURRENT_FORMAT_VERSION,
                name: "pkg".to_string(),
                version: "0.1.0".to_string(),
                yanked: false,
                deps: vec![],
                checksum: None,
                shim: "p/pkg/0.1.0.toml".to_string(),
            },
            IndexRecord {
                format_version: index::CURRENT_FORMAT_VERSION,
                name: "pkg".to_string(),
                version: "0.2.0".to_string(),
                yanked: true,
                deps: vec![],
                checksum: None,
                shim: "p/pkg/0.2.0.toml".to_string(),
            },
        ];
        index::write_index_file(&registry_dir.join("index/p/pkg.idx"), &records).unwrap();

        registry_dir
    }

    #[test]
    fn yanked_version_is_excluded_from_query_but_still_loadable() {
        let tmp = TempDir::new().unwrap();
        let registry_dir = build_yank_test_registry(tmp.path());

        let mut source =
            RegistrySource::from_path(&registry_dir, &tmp.path().join("cache")).unwrap();

        let dep = Dependency::new("pkg", source.source_id);
        let summaries = source.query(&dep).unwrap();

        assert_eq!(
            summaries.len(),
            1,
            "a fresh resolution must not be offered the yanked version"
        );
        assert_eq!(summaries[0].version(), &semver::Version::new(0, 1, 0));

        // But a version already pinned (e.g. by a lockfile predating the
        // yank) must still load successfully.
        let yanked_id = PackageId::new("pkg", semver::Version::new(0, 2, 0), source.source_id);
        let package = source
            .load_package(yanked_id)
            .expect("a yanked version must remain fetchable by exact version");
        assert_eq!(package.version(), &semver::Version::new(0, 2, 0));
    }

    #[test]
    fn test_extract_tarball_basic() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
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
