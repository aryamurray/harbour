//! Source trait - common interface for all package sources.

use std::path::Path;

use anyhow::Result;

use crate::core::{Dependency, Package, PackageId, Summary};

/// A source of packages.
pub trait Source {
    /// Get the source name for display.
    fn name(&self) -> &str;

    /// Check if this source supports the given dependency.
    fn supports(&self, dep: &Dependency) -> bool;

    /// Query available versions matching a dependency.
    fn query(&mut self, dep: &Dependency) -> Result<Vec<Summary>>;

    /// Ensure the source is ready (e.g., git is cloned).
    fn ensure_ready(&mut self) -> Result<()>;

    /// Get the local path for a package.
    fn get_package_path(&self, pkg_id: PackageId) -> Result<&Path>;

    /// Load a full package from this source.
    fn load_package(&mut self, pkg_id: PackageId) -> Result<Package>;

    /// Check if a package is cached locally.
    fn is_cached(&self, pkg_id: PackageId) -> bool;
}
