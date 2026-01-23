//! Package - full package with manifest and targets.
//!
//! A Package combines the manifest with resolved source locations.

use std::path::{Path, PathBuf};

use anyhow::Result;
use semver::Version;

use crate::core::{Manifest, PackageId, SourceId, Summary, Target};
use crate::util::InternedString;

/// A complete package with its manifest and location.
#[derive(Debug, Clone)]
pub struct Package {
    /// The package ID
    package_id: PackageId,

    /// The parsed manifest
    manifest: Manifest,

    /// Root directory of the package
    root: PathBuf,
}

impl Package {
    /// Create a new package from a manifest and root directory.
    pub fn new(manifest: Manifest, root: PathBuf) -> Result<Self> {
        let version = manifest.version()?;
        let source_id = SourceId::for_path(&root)?;
        let package_id = PackageId::new(&manifest.package.name, version, source_id);

        Ok(Package {
            package_id,
            manifest,
            root,
        })
    }

    /// Create a package with a specific source ID (for git/registry sources).
    pub fn with_source_id(manifest: Manifest, root: PathBuf, source_id: SourceId) -> Result<Self> {
        let version = manifest.version()?;
        let package_id = PackageId::new(&manifest.package.name, version, source_id);

        Ok(Package {
            package_id,
            manifest,
            root,
        })
    }

    /// Load a package from a manifest file.
    pub fn load(manifest_path: &Path) -> Result<Self> {
        let manifest = Manifest::load(manifest_path)?;
        let root = manifest_path.parent().unwrap_or(Path::new(".")).to_path_buf();
        Self::new(manifest, root)
    }

    /// Get the package ID.
    pub fn package_id(&self) -> PackageId {
        self.package_id
    }

    /// Get the package name.
    pub fn name(&self) -> InternedString {
        self.package_id.name()
    }

    /// Get the package version.
    pub fn version(&self) -> &Version {
        self.package_id.version()
    }

    /// Get the source ID.
    pub fn source_id(&self) -> SourceId {
        self.package_id.source_id()
    }

    /// Get the manifest.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Get the package root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Get the manifest file path.
    pub fn manifest_path(&self) -> PathBuf {
        self.root.join("Harbor.toml")
    }

    /// Get all targets.
    pub fn targets(&self) -> &[Target] {
        &self.manifest.targets
    }

    /// Get a target by name.
    pub fn target(&self, name: &str) -> Option<&Target> {
        self.manifest.target(name)
    }

    /// Get the default target.
    pub fn default_target(&self) -> Option<&Target> {
        self.manifest.default_target()
    }

    /// Create a summary for this package.
    pub fn summary(&self) -> Result<Summary> {
        let deps = self
            .manifest
            .dependencies
            .iter()
            .map(|(name, spec)| spec.to_dependency(name, &self.root))
            .collect::<Result<Vec<_>>>()?;

        Ok(Summary::new(self.package_id, deps, None))
    }

    /// Get the source directory (typically src/).
    pub fn src_dir(&self) -> PathBuf {
        self.root.join("src")
    }

    /// Get the include directory (typically include/).
    pub fn include_dir(&self) -> PathBuf {
        self.root.join("include")
    }
}

impl std::fmt::Display for Package {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.package_id)
    }
}

impl PartialEq for Package {
    fn eq(&self, other: &Self) -> bool {
        self.package_id == other.package_id
    }
}

impl Eq for Package {}

impl std::hash::Hash for Package {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.package_id.hash(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_manifest(dir: &Path) -> PathBuf {
        let manifest_path = dir.join("Harbor.toml");
        std::fs::write(
            &manifest_path,
            r#"
[package]
name = "testpkg"
version = "1.0.0"

[targets.testpkg]
kind = "staticlib"
sources = ["src/**/*.c"]
"#,
        )
        .unwrap();
        manifest_path
    }

    #[test]
    fn test_package_load() {
        let tmp = TempDir::new().unwrap();
        let manifest_path = create_test_manifest(tmp.path());

        let pkg = Package::load(&manifest_path).unwrap();
        assert_eq!(pkg.name().as_str(), "testpkg");
        assert_eq!(pkg.version(), &Version::new(1, 0, 0));
    }

    #[test]
    fn test_package_summary() {
        let tmp = TempDir::new().unwrap();
        let manifest_path = create_test_manifest(tmp.path());

        let pkg = Package::load(&manifest_path).unwrap();
        let summary = pkg.summary().unwrap();

        assert_eq!(summary.name().as_str(), "testpkg");
        assert!(summary.dependencies().is_empty());
    }
}
