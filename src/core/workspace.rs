//! Workspace - central configuration hub.
//!
//! A Workspace represents the root package and its configuration,
//! providing centralized access to paths and settings.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::core::{Manifest, Package, PackageId};
use crate::util::GlobalContext;

/// Canonical manifest filename (preferred).
pub const MANIFEST_NAME: &str = "Harbour.toml";

/// Alias manifest filename (for compatibility).
pub const MANIFEST_ALIAS: &str = "Harbor.toml";

/// Canonical lockfile filename.
pub const LOCKFILE_NAME: &str = "Harbour.lock";

/// Alias lockfile filename (for compatibility).
pub const LOCKFILE_ALIAS: &str = "Harbor.lock";

/// Find the manifest file in a directory.
///
/// Checks for `Harbour.toml` first, then falls back to `Harbor.toml`.
/// Returns the path to the found manifest, or None if neither exists.
pub fn find_manifest(dir: &Path) -> Option<PathBuf> {
    let primary = dir.join(MANIFEST_NAME);
    if primary.exists() {
        return Some(primary);
    }

    let alias = dir.join(MANIFEST_ALIAS);
    if alias.exists() {
        return Some(alias);
    }

    None
}

/// Find the lockfile in a directory.
///
/// Checks for `Harbour.lock` first, then falls back to `Harbor.lock`.
/// Returns the path to the found lockfile, or None if neither exists.
pub fn find_lockfile(dir: &Path) -> Option<PathBuf> {
    let primary = dir.join(LOCKFILE_NAME);
    if primary.exists() {
        return Some(primary);
    }

    let alias = dir.join(LOCKFILE_ALIAS);
    if alias.exists() {
        return Some(alias);
    }

    None
}

/// A workspace containing the root package and configuration.
#[derive(Debug)]
pub struct Workspace {
    /// The root package
    root_package: Package,

    /// Target directory for build outputs
    target_dir: PathBuf,

    /// Current build profile
    profile: String,
}

impl Workspace {
    /// Create a new workspace from a manifest path.
    pub fn new(manifest_path: &Path, ctx: &GlobalContext) -> Result<Self> {
        let manifest = Manifest::load(manifest_path)?;
        let root = manifest_path
            .parent()
            .unwrap_or(Path::new("."))
            .to_path_buf();

        let root_package = Package::new(manifest, root.clone())?;

        // Default target directory is .harbour/target in the workspace root
        let target_dir = root.join(".harbour").join("target");

        Ok(Workspace {
            root_package,
            target_dir,
            profile: "debug".to_string(),
        })
    }

    /// Create a workspace with a custom target directory.
    pub fn with_target_dir(mut self, target_dir: PathBuf) -> Self {
        self.target_dir = target_dir;
        self
    }

    /// Set the build profile.
    pub fn with_profile(mut self, profile: impl Into<String>) -> Self {
        self.profile = profile.into();
        self
    }

    /// Get the root package.
    pub fn root_package(&self) -> &Package {
        &self.root_package
    }

    /// Get the root package ID.
    pub fn root_package_id(&self) -> PackageId {
        self.root_package.package_id()
    }

    /// Get the workspace root directory.
    pub fn root(&self) -> &Path {
        self.root_package.root()
    }

    /// Get the manifest.
    pub fn manifest(&self) -> &Manifest {
        self.root_package.manifest()
    }

    /// Get the target directory.
    pub fn target_dir(&self) -> &Path {
        &self.target_dir
    }

    /// Get the profile-specific output directory.
    pub fn output_dir(&self) -> PathBuf {
        self.target_dir.join(&self.profile)
    }

    /// Get the deps output directory.
    pub fn deps_dir(&self) -> PathBuf {
        self.output_dir().join("deps")
    }

    /// Get the build directory for a specific package.
    pub fn package_build_dir(&self, pkg_id: PackageId) -> PathBuf {
        self.deps_dir()
            .join(format!("{}-{}", pkg_id.name(), pkg_id.version()))
    }

    /// Get the current profile name.
    pub fn profile(&self) -> &str {
        &self.profile
    }

    /// Check if building in release mode.
    pub fn is_release(&self) -> bool {
        self.profile == "release"
    }

    /// Get the lockfile path.
    ///
    /// Returns existing lockfile if found (checking both Harbour.lock and Harbor.lock),
    /// otherwise returns the canonical path (Harbour.lock).
    pub fn lockfile_path(&self) -> PathBuf {
        find_lockfile(self.root()).unwrap_or_else(|| self.root().join(LOCKFILE_NAME))
    }

    /// Get the manifest path.
    pub fn manifest_path(&self) -> PathBuf {
        find_manifest(self.root()).unwrap_or_else(|| self.root().join(MANIFEST_NAME))
    }

    /// Get the .harbour directory.
    pub fn harbour_dir(&self) -> PathBuf {
        self.root().join(".harbour")
    }

    /// Get the cache directory.
    pub fn cache_dir(&self) -> PathBuf {
        self.harbour_dir().join("cache")
    }

    /// Ensure the target directory exists.
    pub fn ensure_target_dir(&self) -> Result<()> {
        std::fs::create_dir_all(&self.target_dir).with_context(|| {
            format!(
                "failed to create target directory: {}",
                self.target_dir.display()
            )
        })?;
        Ok(())
    }

    /// Ensure the output directory exists.
    pub fn ensure_output_dir(&self) -> Result<()> {
        let output_dir = self.output_dir();
        std::fs::create_dir_all(&output_dir).with_context(|| {
            format!(
                "failed to create output directory: {}",
                output_dir.display()
            )
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_workspace(dir: &Path) -> PathBuf {
        let manifest_path = dir.join(MANIFEST_NAME);
        std::fs::write(
            &manifest_path,
            r#"
[package]
name = "testws"
version = "1.0.0"

[targets.testws]
kind = "exe"
sources = ["src/**/*.c"]
"#,
        )
        .unwrap();
        manifest_path
    }

    fn create_test_workspace_with_alias(dir: &Path) -> PathBuf {
        let manifest_path = dir.join(MANIFEST_ALIAS);
        std::fs::write(
            &manifest_path,
            r#"
[package]
name = "testws"
version = "1.0.0"

[targets.testws]
kind = "exe"
sources = ["src/**/*.c"]
"#,
        )
        .unwrap();
        manifest_path
    }

    #[test]
    fn test_workspace_creation() {
        let tmp = TempDir::new().unwrap();
        let manifest_path = create_test_workspace(tmp.path());
        let ctx = GlobalContext::with_cwd(tmp.path().to_path_buf()).unwrap();

        let ws = Workspace::new(&manifest_path, &ctx).unwrap();
        assert_eq!(ws.root_package().name().as_str(), "testws");
        assert_eq!(ws.profile(), "debug");
    }

    #[test]
    fn test_workspace_with_alias_manifest() {
        let tmp = TempDir::new().unwrap();
        let manifest_path = create_test_workspace_with_alias(tmp.path());
        let ctx = GlobalContext::with_cwd(tmp.path().to_path_buf()).unwrap();

        let ws = Workspace::new(&manifest_path, &ctx).unwrap();
        assert_eq!(ws.root_package().name().as_str(), "testws");
    }

    #[test]
    fn test_find_manifest_prefers_canonical() {
        let tmp = TempDir::new().unwrap();

        // Create both files
        std::fs::write(tmp.path().join(MANIFEST_NAME), "[package]\nname=\"a\"\nversion=\"1.0.0\"").unwrap();
        std::fs::write(tmp.path().join(MANIFEST_ALIAS), "[package]\nname=\"b\"\nversion=\"1.0.0\"").unwrap();

        // Should find the canonical one
        let found = find_manifest(tmp.path()).unwrap();
        assert!(found.ends_with(MANIFEST_NAME));
    }

    #[test]
    fn test_find_manifest_falls_back_to_alias() {
        let tmp = TempDir::new().unwrap();

        // Create only alias
        std::fs::write(tmp.path().join(MANIFEST_ALIAS), "[package]\nname=\"b\"\nversion=\"1.0.0\"").unwrap();

        // Should find the alias
        let found = find_manifest(tmp.path()).unwrap();
        assert!(found.ends_with(MANIFEST_ALIAS));
    }

    #[test]
    fn test_workspace_paths() {
        let tmp = TempDir::new().unwrap();
        let manifest_path = create_test_workspace(tmp.path());
        let ctx = GlobalContext::with_cwd(tmp.path().to_path_buf()).unwrap();

        let ws = Workspace::new(&manifest_path, &ctx)
            .unwrap()
            .with_profile("release");

        assert!(ws.output_dir().ends_with("release"));
        assert!(ws.lockfile_path().ends_with(LOCKFILE_NAME));
    }
}
