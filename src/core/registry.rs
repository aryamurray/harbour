//! Registry trait - source abstraction.
//!
//! The Registry trait provides a uniform interface for querying packages
//! from different sources (paths, git, future registries).

use anyhow::Result;

use crate::core::{Dependency, PackageId, Summary};

/// A source of packages.
///
/// Registries provide access to package metadata and source files.
/// Different registry implementations handle different source types
/// (local paths, git repos, package registries).
pub trait Registry {
    /// Query available versions matching a dependency.
    ///
    /// Returns summaries for all versions that could satisfy the dependency.
    fn query(&mut self, dep: &Dependency) -> Result<Vec<Summary>>;

    /// Query a specific package by ID.
    fn query_exact(&mut self, pkg_id: PackageId) -> Result<Option<Summary>>;

    /// Ensure the source is ready for use.
    ///
    /// For git sources, this clones the repository.
    /// For path sources, this verifies the path exists.
    fn block_until_ready(&mut self) -> Result<()>;

    /// Check if this registry contains a package with the given name.
    fn contains(&self, name: &str) -> bool;

    /// Get the source name for display.
    fn source_name(&self) -> &str;
}

/// A collection of registries for resolution.
pub struct RegistrySet {
    registries: Vec<Box<dyn Registry>>,
}

impl RegistrySet {
    /// Create a new empty registry set.
    pub fn new() -> Self {
        RegistrySet {
            registries: Vec::new(),
        }
    }

    /// Add a registry to the set.
    pub fn add(&mut self, registry: Box<dyn Registry>) {
        self.registries.push(registry);
    }

    /// Query all registries for versions matching a dependency.
    pub fn query(&mut self, dep: &Dependency) -> Result<Vec<Summary>> {
        let mut results = Vec::new();

        for registry in &mut self.registries {
            match registry.query(dep) {
                Ok(summaries) => results.extend(summaries),
                Err(e) => {
                    // Log but continue - other registries might have it
                    tracing::debug!(
                        "Registry {} failed to query {}: {}",
                        registry.source_name(),
                        dep.name(),
                        e
                    );
                }
            }
        }

        Ok(results)
    }

    /// Query a specific package by ID.
    pub fn query_exact(&mut self, pkg_id: PackageId) -> Result<Option<Summary>> {
        for registry in &mut self.registries {
            if let Ok(Some(summary)) = registry.query_exact(pkg_id) {
                return Ok(Some(summary));
            }
        }
        Ok(None)
    }

    /// Ensure all registries are ready.
    pub fn block_until_ready(&mut self) -> Result<()> {
        for registry in &mut self.registries {
            registry.block_until_ready()?;
        }
        Ok(())
    }
}

impl Default for RegistrySet {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::SourceId;
    use semver::Version;
    use tempfile::TempDir;

    struct MockRegistry {
        name: String,
        summaries: Vec<Summary>,
    }

    impl MockRegistry {
        fn new(name: &str) -> Self {
            MockRegistry {
                name: name.to_string(),
                summaries: Vec::new(),
            }
        }

        fn with_summary(mut self, summary: Summary) -> Self {
            self.summaries.push(summary);
            self
        }
    }

    impl Registry for MockRegistry {
        fn query(&mut self, dep: &Dependency) -> Result<Vec<Summary>> {
            Ok(self
                .summaries
                .iter()
                .filter(|s| s.name() == dep.name())
                .cloned()
                .collect())
        }

        fn query_exact(&mut self, pkg_id: PackageId) -> Result<Option<Summary>> {
            Ok(self
                .summaries
                .iter()
                .find(|s| s.package_id() == pkg_id)
                .cloned())
        }

        fn block_until_ready(&mut self) -> Result<()> {
            Ok(())
        }

        fn contains(&self, name: &str) -> bool {
            self.summaries.iter().any(|s| s.name().as_str() == name)
        }

        fn source_name(&self) -> &str {
            &self.name
        }
    }

    #[test]
    fn test_registry_set() {
        let tmp = TempDir::new().unwrap();
        let source = SourceId::for_path(tmp.path()).unwrap();
        let pkg_id = PackageId::new("testpkg", Version::new(1, 0, 0), source);
        let summary = Summary::new(pkg_id, vec![], None);

        let registry = MockRegistry::new("mock").with_summary(summary);

        let mut set = RegistrySet::new();
        set.add(Box::new(registry));

        let dep = Dependency::new("testpkg", source);
        let results = set.query(&dep).unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name().as_str(), "testpkg");
    }
}
