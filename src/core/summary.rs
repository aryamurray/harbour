//! Summary - lightweight manifest for resolution.
//!
//! A Summary contains just enough information for dependency resolution:
//! package identity and dependencies. It's designed to be cheap to clone.

use std::sync::Arc;

use semver::Version;

use crate::core::{Dependency, PackageId, SourceId};
use crate::util::InternedString;

/// A lightweight package summary for resolution.
///
/// Summaries are Arc-wrapped internally for cheap cloning.
#[derive(Clone)]
pub struct Summary {
    inner: Arc<SummaryInner>,
}

#[derive(Clone)]
struct SummaryInner {
    package_id: PackageId,
    dependencies: Vec<Dependency>,
    checksum: Option<String>,
    features: Vec<String>,
}

impl Summary {
    /// Create a new summary.
    pub fn new(
        package_id: PackageId,
        dependencies: Vec<Dependency>,
        checksum: Option<String>,
    ) -> Self {
        Summary {
            inner: Arc::new(SummaryInner {
                package_id,
                dependencies,
                checksum,
                features: Vec::new(),
            }),
        }
    }

    /// Create a summary with features.
    pub fn with_features(mut self, features: Vec<String>) -> Self {
        let inner = Arc::make_mut(&mut self.inner);
        inner.features = features;
        self
    }

    /// Get the package ID.
    pub fn package_id(&self) -> PackageId {
        self.inner.package_id
    }

    /// Get the package name.
    pub fn name(&self) -> InternedString {
        self.inner.package_id.name()
    }

    /// Get the package version.
    pub fn version(&self) -> &Version {
        self.inner.package_id.version()
    }

    /// Get the source ID.
    pub fn source_id(&self) -> SourceId {
        self.inner.package_id.source_id()
    }

    /// Get the dependencies.
    pub fn dependencies(&self) -> &[Dependency] {
        &self.inner.dependencies
    }

    /// Get the checksum.
    pub fn checksum(&self) -> Option<&str> {
        self.inner.checksum.as_deref()
    }

    /// Get the features.
    pub fn features(&self) -> &[String] {
        &self.inner.features
    }

    /// Map dependencies to a new list.
    pub fn map_dependencies<F>(mut self, f: F) -> Self
    where
        F: FnMut(Dependency) -> Dependency,
    {
        let inner = Arc::make_mut(&mut self.inner);
        inner.dependencies = inner.dependencies.drain(..).map(f).collect();
        self
    }
}

impl std::fmt::Debug for Summary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Summary")
            .field("package_id", &self.inner.package_id)
            .field("dependencies", &self.inner.dependencies.len())
            .field("checksum", &self.inner.checksum)
            .finish()
    }
}

impl std::fmt::Display for Summary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.inner.package_id)
    }
}

impl PartialEq for Summary {
    fn eq(&self, other: &Self) -> bool {
        self.inner.package_id == other.inner.package_id
    }
}

impl Eq for Summary {}

impl std::hash::Hash for Summary {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.inner.package_id.hash(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_summary_creation() {
        let tmp = TempDir::new().unwrap();
        let source = SourceId::for_path(tmp.path()).unwrap();
        let pkg_id = PackageId::new("test", Version::new(1, 0, 0), source);

        let summary = Summary::new(pkg_id, vec![], Some("abc123".into()));

        assert_eq!(summary.name().as_str(), "test");
        assert_eq!(summary.version(), &Version::new(1, 0, 0));
        assert_eq!(summary.checksum(), Some("abc123"));
    }

    #[test]
    fn test_summary_cheap_clone() {
        let tmp = TempDir::new().unwrap();
        let source = SourceId::for_path(tmp.path()).unwrap();
        let pkg_id = PackageId::new("test", Version::new(1, 0, 0), source);

        let summary1 = Summary::new(pkg_id, vec![], None);
        let summary2 = summary1.clone();

        // Should share the same Arc
        assert!(Arc::ptr_eq(&summary1.inner, &summary2.inner));
    }
}
