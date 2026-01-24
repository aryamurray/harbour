//! Dependency resolution.
//!
//! This module implements PubGrub-based version resolution for Harbour packages.
//! The resolver is pure and deterministic - all I/O happens before resolution.

pub mod cpp_constraints;
pub mod encode;
pub mod errors;
pub mod resolve;
pub mod version;

pub use cpp_constraints::CppConstraints;
pub use resolve::{Resolve, ResolveError};

use std::collections::HashMap;
use std::error::Error as StdError;
use std::fmt;

use anyhow::{bail, Result};
use pubgrub::{Dependencies, DependencyProvider, Range, DefaultStringReporter, Reporter, PackageResolutionStatistics};
use semver::Version;

use crate::core::{SourceId, Summary};
use crate::util::InternedString;

/// A package identifier for PubGrub resolution.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PubGrubPackage {
    pub name: InternedString,
    pub source_id: SourceId,
}

impl fmt::Display for PubGrubPackage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)
    }
}

/// Custom error type for the resolver that implements std::error::Error.
#[derive(Debug)]
pub struct ResolverError(String);

impl fmt::Display for ResolverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl StdError for ResolverError {}

impl From<anyhow::Error> for ResolverError {
    fn from(e: anyhow::Error) -> Self {
        ResolverError(e.to_string())
    }
}

/// Dependency provider for PubGrub resolution.
pub struct HarbourResolver {
    /// Available package summaries by name
    summaries: HashMap<InternedString, Vec<Summary>>,

    /// Root package
    root: Summary,
}

impl HarbourResolver {
    /// Create a new resolver with the root package.
    pub fn new(root: Summary) -> Self {
        HarbourResolver {
            summaries: HashMap::new(),
            root,
        }
    }

    /// Add available summaries for resolution.
    pub fn add_summaries(&mut self, summaries: Vec<Summary>) {
        for summary in summaries {
            self.summaries
                .entry(summary.name())
                .or_default()
                .push(summary);
        }
    }

    /// Get the root package.
    pub fn root(&self) -> &Summary {
        &self.root
    }

    /// Resolve dependencies and return the result.
    pub fn resolve(self) -> Result<Resolve> {
        let root_pkg = PubGrubPackage {
            name: self.root.name(),
            source_id: self.root.source_id(),
        };

        let root_version = self.root.version().clone();

        match pubgrub::resolve(&self, root_pkg.clone(), root_version.clone()) {
            Ok(solution) => {
                // Convert PubGrub solution to Resolve
                let mut resolve = Resolve::new();

                for (pkg, version) in solution {
                    // Find the summary for this version
                    if let Some(summaries) = self.summaries.get(&pkg.name) {
                        if let Some(summary) = summaries.iter().find(|s| s.version() == &version) {
                            resolve.add_package(summary.package_id(), summary.clone());
                        }
                    } else if pkg.name == self.root.name() {
                        // Root package
                        resolve.add_package(self.root.package_id(), self.root.clone());
                    }
                }

                // Add dependency edges
                let packages: Vec<_> = resolve.packages().map(|(id, s)| (*id, s.clone())).collect();
                for (pkg_id, summary) in packages {
                    for dep in summary.dependencies() {
                        if let Some(dep_id) = resolve.get_package_by_name(dep.name()) {
                            resolve.add_edge(pkg_id, dep_id);
                        }
                    }
                }

                Ok(resolve)
            }
            Err(pubgrub::PubGrubError::NoSolution(tree)) => {
                let report = DefaultStringReporter::report(&tree);
                bail!("dependency resolution failed:\n{}", report);
            }
            Err(e) => {
                bail!("dependency resolution error: {:?}", e);
            }
        }
    }
}

impl DependencyProvider for HarbourResolver {
    type P = PubGrubPackage;
    type V = Version;
    type VS = Range<Version>;
    type M = String;
    type Err = ResolverError;
    type Priority = u32;

    fn prioritize(
        &self,
        package: &Self::P,
        _range: &Self::VS,
        _package_conflicts_counts: &PackageResolutionStatistics,
    ) -> Self::Priority {
        // Higher priority = resolved first
        // Prioritize packages with fewer available versions
        if let Some(summaries) = self.summaries.get(&package.name) {
            (1000 - summaries.len().min(1000)) as u32
        } else {
            1000
        }
    }

    fn choose_version(
        &self,
        package: &Self::P,
        range: &Self::VS,
    ) -> Result<Option<Self::V>, Self::Err> {
        // For root package
        if package.name == self.root.name() {
            let version = self.root.version().clone();
            if range.contains(&version) {
                return Ok(Some(version));
            }
            return Ok(None);
        }

        // Find the highest matching version
        if let Some(summaries) = self.summaries.get(&package.name) {
            let mut matching: Vec<_> = summaries
                .iter()
                .filter(|s| range.contains(s.version()))
                .collect();

            // Sort by version descending
            matching.sort_by(|a, b| b.version().cmp(a.version()));

            if let Some(best) = matching.first() {
                return Ok(Some(best.version().clone()));
            }
        }

        Ok(None)
    }

    fn get_dependencies(
        &self,
        package: &Self::P,
        version: &Self::V,
    ) -> Result<Dependencies<Self::P, Self::VS, Self::M>, Self::Err> {
        // For root package
        if package.name == self.root.name() && version == self.root.version() {
            let deps = self
                .root
                .dependencies()
                .iter()
                .map(|dep| {
                    let pkg = PubGrubPackage {
                        name: dep.name(),
                        source_id: dep.source_id(),
                    };
                    let range = version::version_req_to_range(dep.version_req());
                    (pkg, range)
                })
                .collect();

            return Ok(Dependencies::Available(deps));
        }

        // Find the summary for this version
        if let Some(summaries) = self.summaries.get(&package.name) {
            if let Some(summary) = summaries.iter().find(|s| s.version() == version) {
                let deps = summary
                    .dependencies()
                    .iter()
                    .map(|dep| {
                        let pkg = PubGrubPackage {
                            name: dep.name(),
                            source_id: dep.source_id(),
                        };
                        let range = version::version_req_to_range(dep.version_req());
                        (pkg, range)
                    })
                    .collect();

                return Ok(Dependencies::Available(deps));
            }
        }

        Ok(Dependencies::Unavailable("package not found".to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::PackageId;
    use tempfile::TempDir;

    #[test]
    fn test_resolver_simple() {
        let tmp = TempDir::new().unwrap();
        let source = SourceId::for_path(tmp.path()).unwrap();

        let root_id = PackageId::new("root", Version::new(1, 0, 0), source);
        let root = Summary::new(root_id, vec![], None);

        let resolver = HarbourResolver::new(root);
        let resolve = resolver.resolve().unwrap();

        assert_eq!(resolve.packages().count(), 1);
    }
}
