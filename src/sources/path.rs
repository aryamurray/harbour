//! Path source - local filesystem dependencies.

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

use crate::core::workspace::find_manifest;
use crate::core::{Dependency, Package, PackageId, SourceId, Summary};
use crate::sources::Source;

/// A source for local path dependencies.
pub struct PathSource {
    /// The root path
    path: PathBuf,

    /// Cached package
    package: Option<Package>,

    /// Source ID
    source_id: SourceId,
}

impl PathSource {
    /// Create a new path source.
    pub fn new(path: PathBuf, source_id: SourceId) -> Self {
        PathSource {
            path,
            package: None,
            source_id,
        }
    }

    /// Create a path source from a path.
    pub fn for_path(path: &Path) -> Result<Self> {
        let source_id = SourceId::for_path(path)?;
        Ok(PathSource::new(path.to_path_buf(), source_id))
    }

    /// Load the package from the path.
    fn load(&mut self) -> Result<&Package> {
        if self.package.is_none() {
            let manifest_path = find_manifest(&self.path)?;

            let package = Package::load(&manifest_path)?;
            self.package = Some(package);
        }

        Ok(self.package.as_ref().unwrap())
    }
}

impl Source for PathSource {
    fn name(&self) -> &str {
        "path"
    }

    fn supports(&self, dep: &Dependency) -> bool {
        dep.source_id() == self.source_id
    }

    fn query(&mut self, dep: &Dependency) -> Result<Vec<Summary>> {
        if !self.supports(dep) {
            return Ok(vec![]);
        }

        let package = self.load()?;

        // Check if the package name matches
        if package.name() != dep.name() {
            return Ok(vec![]);
        }

        // Check if version matches
        if !dep.matches_version(package.version()) {
            return Ok(vec![]);
        }

        let summary = package.summary()?;
        Ok(vec![summary])
    }

    fn ensure_ready(&mut self) -> Result<()> {
        if !self.path.exists() {
            bail!("path does not exist: {}", self.path.display());
        }

        find_manifest(&self.path)?;

        Ok(())
    }

    fn get_package_path(&self, _pkg_id: PackageId) -> Result<&Path> {
        Ok(&self.path)
    }

    fn load_package(&mut self, pkg_id: PackageId) -> Result<Package> {
        let package = self.load()?;

        if package.package_id() != pkg_id {
            bail!(
                "package ID mismatch: expected {}, found {}",
                pkg_id,
                package.package_id()
            );
        }

        Ok(package.clone())
    }

    fn is_cached(&self, _pkg_id: PackageId) -> bool {
        // Path sources are always "cached" (they're local)
        self.path.exists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use semver::VersionReq;
    use tempfile::TempDir;

    fn create_test_package(dir: &Path, name: &str, version: &str) {
        let manifest = format!(
            r#"
[package]
name = "{}"
version = "{}"

[targets.{}]
kind = "staticlib"
sources = ["src/**/*.c"]
"#,
            name, version, name
        );

        std::fs::write(dir.join("Harbor.toml"), manifest).unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/lib.c"), "void init() {}").unwrap();
    }

    #[test]
    fn test_path_source_query() {
        let tmp = TempDir::new().unwrap();
        create_test_package(tmp.path(), "testlib", "1.0.0");

        let mut source = PathSource::for_path(tmp.path()).unwrap();
        source.ensure_ready().unwrap();

        let dep = Dependency::new("testlib", source.source_id);
        let summaries = source.query(&dep).unwrap();

        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].name().as_str(), "testlib");
    }

    #[test]
    fn test_path_source_version_filter() {
        let tmp = TempDir::new().unwrap();
        create_test_package(tmp.path(), "testlib", "2.0.0");

        let mut source = PathSource::for_path(tmp.path()).unwrap();

        // Query for version that doesn't match
        let dep = Dependency::new("testlib", source.source_id)
            .with_version_req(VersionReq::parse("^1.0").unwrap());

        let summaries = source.query(&dep).unwrap();
        assert!(summaries.is_empty());
    }
}
