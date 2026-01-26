//! Source cache management.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

use crate::core::{Dependency, Package, PackageId, SourceId, Summary};
use crate::sources::{GitSource, PathSource, RegistrySource, Source};

/// Manages all package sources and caching.
pub struct SourceCache {
    /// Cache directory for downloaded sources
    cache_dir: PathBuf,

    /// Active sources by source ID
    sources: HashMap<SourceId, Box<dyn Source>>,
}

impl SourceCache {
    /// Create a new source cache.
    pub fn new(cache_dir: PathBuf) -> Self {
        SourceCache {
            cache_dir,
            sources: HashMap::new(),
        }
    }

    /// Get or create a source for a dependency.
    pub fn get_or_create(&mut self, dep: &Dependency) -> Result<&mut dyn Source> {
        let source_id = dep.source_id();

        // Create source first if needed (avoids borrow checker issues with entry API)
        if !self.sources.contains_key(&source_id) {
            let source = self.create_source(source_id)?;
            self.sources.insert(source_id, source);
        }

        // Unwrap is safe because we just ensured the key exists
        Ok(self.sources.get_mut(&source_id).unwrap().as_mut())
    }

    /// Create a source for the given source ID.
    fn create_source(&self, source_id: SourceId) -> Result<Box<dyn Source>> {
        if source_id.is_path() {
            let path = source_id
                .path()
                .ok_or_else(|| anyhow::anyhow!("path source missing path"))?;
            Ok(Box::new(PathSource::new(path.to_path_buf(), source_id)))
        } else if source_id.is_git() {
            let reference = source_id.git_reference().cloned().unwrap_or_default();
            Ok(Box::new(GitSource::new(
                source_id.url().clone(),
                reference,
                &self.cache_dir,
                source_id,
            )))
        } else if source_id.is_registry() {
            Ok(Box::new(RegistrySource::new(
                source_id.url().clone(),
                &self.cache_dir,
                source_id,
            )))
        } else {
            bail!("unsupported source kind")
        }
    }

    /// Query all sources for versions matching a dependency.
    pub fn query(&mut self, dep: &Dependency) -> Result<Vec<Summary>> {
        let source = self.get_or_create(dep)?;
        source.query(dep)
    }

    /// Load a package from its source.
    pub fn load_package(&mut self, pkg_id: PackageId) -> Result<Package> {
        let source_id = pkg_id.source_id();

        if !self.sources.contains_key(&source_id) {
            let source = self.create_source(source_id)?;
            self.sources.insert(source_id, source);
        }

        let source = self.sources.get_mut(&source_id).unwrap();
        source.load_package(pkg_id)
    }

    /// Get the local path for a package.
    pub fn package_path(&mut self, pkg_id: PackageId) -> Result<PathBuf> {
        let source_id = pkg_id.source_id();

        if !self.sources.contains_key(&source_id) {
            let source = self.create_source(source_id)?;
            self.sources.insert(source_id, source);
        }

        let source = self.sources.get_mut(&source_id).unwrap();
        source.ensure_ready()?;
        Ok(source.get_package_path(pkg_id)?.to_path_buf())
    }

    /// Ensure all sources for the given dependencies are ready.
    pub fn ensure_ready(&mut self, deps: &[Dependency]) -> Result<()> {
        for dep in deps {
            let source = self.get_or_create(dep)?;
            source.ensure_ready()?;
        }
        Ok(())
    }

    /// Get the cache directory.
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_source_cache_path() {
        let tmp = TempDir::new().unwrap();
        let cache_dir = tmp.path().join("cache");
        let pkg_dir = tmp.path().join("pkg");

        // Create a test package
        std::fs::create_dir_all(&pkg_dir).unwrap();
        std::fs::write(
            pkg_dir.join("Harbor.toml"),
            "[package]\nname = \"test\"\nversion = \"1.0.0\"\n",
        )
        .unwrap();

        let mut cache = SourceCache::new(cache_dir);
        let source_id = SourceId::for_path(&pkg_dir).unwrap();
        let dep = Dependency::new("test", source_id);

        let summaries = cache.query(&dep).unwrap();
        assert_eq!(summaries.len(), 1);
    }
}
