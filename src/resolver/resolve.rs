//! Resolve - the immutable dependency graph.
//!
//! Once created, a Resolve is read-only. Only `harbour update` can
//! create a new Resolve.

use std::collections::{HashMap, HashSet};

use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::Topo;
use thiserror::Error;

use crate::core::{PackageId, SourceId, Summary};
use crate::resolver::encode::RegistryProvenance;
use crate::util::InternedString;

/// Version of the resolve/lockfile format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveVersion {
    V1,
}

impl Default for ResolveVersion {
    fn default() -> Self {
        ResolveVersion::V1
    }
}

/// Errors that can occur during package lookup.
#[derive(Debug, Error)]
pub enum ResolveError {
    /// Package with given name not found in the dependency graph.
    #[error("package `{name}` not found in dependency graph")]
    PackageNotFound { name: InternedString },

    /// Multiple packages with the same name exist from different sources.
    #[error("ambiguous package `{name}` - found in {count} sources: {sources}")]
    AmbiguousPackage {
        name: InternedString,
        count: usize,
        sources: String,
    },

    /// Multiple versions of the same package from the same source.
    /// MVP only supports one version per (name, source) pair.
    #[error(
        "multiple versions of `{name}` from `{pkg_source}`: {versions:?}\n\
             help: MVP supports one version per (name, source)"
    )]
    MultipleVersions {
        name: InternedString,
        pkg_source: String,
        versions: Vec<String>,
    },
}

/// The resolved dependency graph.
///
/// This is an immutable representation of all packages in the dependency tree
/// along with their exact versions and relationships.
///
/// Supports multi-version packages (same name from different sources) by keying
/// packages on (name, source_id).
#[derive(Debug, Clone)]
pub struct Resolve {
    /// Package graph
    graph: DiGraph<PackageId, ()>,

    /// Map from PackageId to node index
    pkg_to_node: HashMap<PackageId, NodeIndex>,

    /// Index for looking up packages by (name, source_id).
    /// Supports multiple versions from the same source (future use).
    pkg_index: HashMap<(InternedString, SourceId), Vec<PackageId>>,

    /// Summaries for each package
    summaries: HashMap<PackageId, Summary>,

    /// Checksums for verification
    checksums: HashMap<PackageId, Option<String>>,

    /// Registry provenance for reproducibility (only for registry packages)
    registry_provenances: HashMap<PackageId, RegistryProvenance>,

    /// Format version
    version: ResolveVersion,
}

impl Resolve {
    /// Create a new empty Resolve.
    pub fn new() -> Self {
        Resolve {
            graph: DiGraph::new(),
            pkg_to_node: HashMap::new(),
            pkg_index: HashMap::new(),
            summaries: HashMap::new(),
            checksums: HashMap::new(),
            registry_provenances: HashMap::new(),
            version: ResolveVersion::V1,
        }
    }

    /// Add a package to the resolve.
    pub fn add_package(&mut self, pkg_id: PackageId, summary: Summary) {
        if self.pkg_to_node.contains_key(&pkg_id) {
            return;
        }

        let node = self.graph.add_node(pkg_id);
        self.pkg_to_node.insert(pkg_id, node);

        // Index by (name, source_id) for multi-version support
        let key = (pkg_id.name(), pkg_id.source_id());
        self.pkg_index.entry(key).or_default().push(pkg_id);

        if let Some(checksum) = summary.checksum() {
            self.checksums.insert(pkg_id, Some(checksum.to_string()));
        }

        self.summaries.insert(pkg_id, summary);
    }

    /// Add a dependency edge between packages.
    pub fn add_edge(&mut self, from: PackageId, to: PackageId) {
        if let (Some(&from_node), Some(&to_node)) =
            (self.pkg_to_node.get(&from), self.pkg_to_node.get(&to))
        {
            // Check if edge already exists
            if !self.graph.contains_edge(from_node, to_node) {
                self.graph.add_edge(from_node, to_node, ());
            }
        }
    }

    /// Get a package ID by name.
    ///
    /// If multiple packages with the same name exist from different sources,
    /// this returns the first one found. Use `get_package` for explicit source
    /// selection, or `get_package_by_name_strict` for error on ambiguity.
    pub fn get_package_by_name(&self, name: InternedString) -> Option<PackageId> {
        // Find all packages with this name
        let matches: Vec<_> = self
            .pkg_index
            .iter()
            .filter(|((n, _), _)| *n == name)
            .flat_map(|(_, ids)| ids.iter().copied())
            .collect();

        matches.first().copied()
    }

    /// Get a package ID by name, returning an error if ambiguous.
    ///
    /// Use this when you need to ensure unambiguous resolution.
    pub fn get_package_by_name_strict(
        &self,
        name: InternedString,
    ) -> Result<PackageId, ResolveError> {
        // Find all packages with this name
        let matches: Vec<_> = self
            .pkg_index
            .iter()
            .filter(|((n, _), _)| *n == name)
            .flat_map(|(_, ids)| ids.iter().copied())
            .collect();

        match matches.len() {
            0 => Err(ResolveError::PackageNotFound { name }),
            1 => Ok(matches[0]),
            _ => {
                // Collect sources for the error message
                let sources: Vec<_> = matches
                    .iter()
                    .map(|id| format!("{} ({})", id.source_id(), id.version()))
                    .collect();
                Err(ResolveError::AmbiguousPackage {
                    name,
                    count: matches.len(),
                    sources: sources.join(", "),
                })
            }
        }
    }

    /// Get a package ID by name and source.
    ///
    /// This is the unambiguous way to look up a package when you know its source.
    pub fn get_package(&self, name: InternedString, source: SourceId) -> Option<PackageId> {
        self.pkg_index
            .get(&(name, source))
            .and_then(|ids| ids.first().copied())
    }

    /// Get the unique package ID for a (name, source) pair.
    ///
    /// Errors if multiple versions exist (MVP invariant: one version per (name, source)).
    /// This is stricter than `get_package` which just returns the first match.
    pub fn unique_pkg(
        &self,
        name: InternedString,
        source: SourceId,
    ) -> Result<PackageId, ResolveError> {
        match self.pkg_index.get(&(name, source)) {
            None => Err(ResolveError::PackageNotFound { name }),
            Some(ids) if ids.len() == 1 => Ok(ids[0]),
            Some(ids) => Err(ResolveError::MultipleVersions {
                name,
                pkg_source: source.to_string(),
                versions: ids.iter().map(|id| id.version().to_string()).collect(),
            }),
        }
    }

    /// Get all packages with a given name (from any source).
    pub fn get_packages_by_name(&self, name: InternedString) -> Vec<PackageId> {
        self.pkg_index
            .iter()
            .filter(|((n, _), _)| *n == name)
            .flat_map(|(_, ids)| ids.iter().copied())
            .collect()
    }

    /// Get the summary for a package.
    pub fn summary(&self, pkg_id: PackageId) -> Option<&Summary> {
        self.summaries.get(&pkg_id)
    }

    /// Get the checksum for a package.
    pub fn checksum(&self, pkg_id: PackageId) -> Option<&str> {
        self.checksums.get(&pkg_id).and_then(|c| c.as_deref())
    }

    /// Set registry provenance for a package.
    pub fn set_registry_provenance(&mut self, pkg_id: PackageId, provenance: RegistryProvenance) {
        self.registry_provenances.insert(pkg_id, provenance);
    }

    /// Get registry provenance for a package.
    pub fn registry_provenance(&self, pkg_id: PackageId) -> Option<RegistryProvenance> {
        self.registry_provenances.get(&pkg_id).cloned()
    }

    /// Iterate over all packages.
    pub fn packages(&self) -> impl Iterator<Item = (&PackageId, &Summary)> {
        self.summaries.iter()
    }

    /// Get the number of packages.
    pub fn len(&self) -> usize {
        self.summaries.len()
    }

    /// Check if empty.
    pub fn is_empty(&self) -> bool {
        self.summaries.is_empty()
    }

    /// Get direct dependencies of a package.
    pub fn deps(&self, pkg_id: PackageId) -> Vec<PackageId> {
        if let Some(&node) = self.pkg_to_node.get(&pkg_id) {
            self.graph.neighbors(node).map(|n| self.graph[n]).collect()
        } else {
            Vec::new()
        }
    }

    /// Get packages that depend on the given package.
    pub fn dependents(&self, pkg_id: PackageId) -> Vec<PackageId> {
        if let Some(&node) = self.pkg_to_node.get(&pkg_id) {
            self.graph
                .neighbors_directed(node, petgraph::Direction::Incoming)
                .map(|n| self.graph[n])
                .collect()
        } else {
            Vec::new()
        }
    }

    /// Get packages in topological order (dependencies before dependents).
    /// This returns packages that have no dependencies first, then packages that
    /// depend on them, etc. Useful for build order.
    pub fn topological_order(&self) -> Vec<PackageId> {
        let mut topo = Topo::new(&self.graph);
        let mut order = Vec::new();

        while let Some(node) = topo.next(&self.graph) {
            order.push(self.graph[node]);
        }

        // Petgraph's Topo returns nodes in order where a->b means a before b.
        // But add_edge(a, b) means "a depends on b", so b should come before a.
        // Reverse to get dependencies before dependents.
        order.reverse();
        order
    }

    /// Get packages in reverse topological order (dependents before dependencies).
    pub fn reverse_topological_order(&self) -> Vec<PackageId> {
        let mut topo = Topo::new(&self.graph);
        let mut order = Vec::new();

        while let Some(node) = topo.next(&self.graph) {
            order.push(self.graph[node]);
        }

        order
    }

    /// Check if a package is in the resolve.
    pub fn contains(&self, pkg_id: PackageId) -> bool {
        self.pkg_to_node.contains_key(&pkg_id)
    }

    /// Check if a package with the given name is in the resolve.
    pub fn contains_name(&self, name: &str) -> bool {
        self.pkg_index.keys().any(|(n, _)| n.as_str() == name)
    }

    /// Get the resolve version.
    pub fn version(&self) -> ResolveVersion {
        self.version
    }

    /// Get all transitive dependencies of a package.
    pub fn transitive_deps(&self, pkg_id: PackageId) -> HashSet<PackageId> {
        let mut visited = HashSet::new();
        let mut stack = vec![pkg_id];

        while let Some(current) = stack.pop() {
            if visited.insert(current) {
                for dep in self.deps(current) {
                    stack.push(dep);
                }
            }
        }

        visited.remove(&pkg_id);
        visited
    }
}

impl Default for Resolve {
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

    fn create_test_summary(name: &str, version: &str) -> (PackageId, Summary) {
        let tmp = TempDir::new().unwrap();
        let source = SourceId::for_path(tmp.path()).unwrap();
        let pkg_id = PackageId::new(name, version.parse().unwrap(), source);
        let summary = Summary::new(pkg_id, vec![], None);
        (pkg_id, summary)
    }

    fn create_test_summary_with_source(
        name: &str,
        version: &str,
        source: SourceId,
    ) -> (PackageId, Summary) {
        let pkg_id = PackageId::new(name, version.parse().unwrap(), source);
        let summary = Summary::new(pkg_id, vec![], None);
        (pkg_id, summary)
    }

    #[test]
    fn test_resolve_basic() {
        let mut resolve = Resolve::new();

        let (id_a, sum_a) = create_test_summary("a", "1.0.0");
        let (id_b, sum_b) = create_test_summary("b", "1.0.0");

        resolve.add_package(id_a, sum_a);
        resolve.add_package(id_b, sum_b);
        resolve.add_edge(id_a, id_b);

        assert_eq!(resolve.len(), 2);
        assert_eq!(resolve.deps(id_a), vec![id_b]);
        assert_eq!(resolve.dependents(id_b), vec![id_a]);
    }

    #[test]
    fn test_topological_order() {
        let mut resolve = Resolve::new();

        let (id_a, sum_a) = create_test_summary("a", "1.0.0");
        let (id_b, sum_b) = create_test_summary("b", "1.0.0");
        let (id_c, sum_c) = create_test_summary("c", "1.0.0");

        resolve.add_package(id_a, sum_a);
        resolve.add_package(id_b, sum_b);
        resolve.add_package(id_c, sum_c);

        // a -> b -> c
        resolve.add_edge(id_a, id_b);
        resolve.add_edge(id_b, id_c);

        let order = resolve.topological_order();

        // c should come before b, b should come before a
        let pos_a = order.iter().position(|&id| id == id_a).unwrap();
        let pos_b = order.iter().position(|&id| id == id_b).unwrap();
        let pos_c = order.iter().position(|&id| id == id_c).unwrap();

        assert!(pos_c < pos_b);
        assert!(pos_b < pos_a);
    }

    #[test]
    fn test_resolve_empty() {
        let resolve = Resolve::new();

        assert_eq!(resolve.len(), 0);
        assert!(resolve.is_empty());
        assert_eq!(resolve.version(), ResolveVersion::V1);
    }

    #[test]
    fn test_resolve_single_package() {
        let mut resolve = Resolve::new();
        let (id, summary) = create_test_summary("single", "1.0.0");

        resolve.add_package(id, summary);

        assert_eq!(resolve.len(), 1);
        assert!(!resolve.is_empty());
        assert!(resolve.contains(id));
        assert!(resolve.contains_name("single"));
        assert!(!resolve.contains_name("nonexistent"));
    }

    #[test]
    fn test_resolve_add_package_idempotent() {
        let mut resolve = Resolve::new();
        let tmp = TempDir::new().unwrap();
        let source = SourceId::for_path(tmp.path()).unwrap();
        let pkg_id = PackageId::new("test", Version::new(1, 0, 0), source);
        let summary = Summary::new(pkg_id, vec![], None);

        resolve.add_package(pkg_id, summary.clone());
        resolve.add_package(pkg_id, summary); // Add same package again

        // Should still only have one package
        assert_eq!(resolve.len(), 1);
    }

    #[test]
    fn test_resolve_add_edge_idempotent() {
        let mut resolve = Resolve::new();
        let (id_a, sum_a) = create_test_summary("a", "1.0.0");
        let (id_b, sum_b) = create_test_summary("b", "1.0.0");

        resolve.add_package(id_a, sum_a);
        resolve.add_package(id_b, sum_b);

        resolve.add_edge(id_a, id_b);
        resolve.add_edge(id_a, id_b); // Add same edge again

        // Should only have one edge
        assert_eq!(resolve.deps(id_a).len(), 1);
    }

    #[test]
    fn test_resolve_deps_empty() {
        let mut resolve = Resolve::new();
        let (id, summary) = create_test_summary("nodeps", "1.0.0");
        resolve.add_package(id, summary);

        assert!(resolve.deps(id).is_empty());
    }

    #[test]
    fn test_resolve_dependents_empty() {
        let mut resolve = Resolve::new();
        let (id, summary) = create_test_summary("noparents", "1.0.0");
        resolve.add_package(id, summary);

        assert!(resolve.dependents(id).is_empty());
    }

    #[test]
    fn test_resolve_deps_nonexistent() {
        let resolve = Resolve::new();
        let tmp = TempDir::new().unwrap();
        let source = SourceId::for_path(tmp.path()).unwrap();
        let fake_id = PackageId::new("fake", Version::new(1, 0, 0), source);

        // Deps of non-existent package should be empty
        assert!(resolve.deps(fake_id).is_empty());
    }

    #[test]
    fn test_resolve_summary() {
        let mut resolve = Resolve::new();
        let tmp = TempDir::new().unwrap();
        let source = SourceId::for_path(tmp.path()).unwrap();
        let pkg_id = PackageId::new("mysummary", Version::new(2, 0, 0), source);
        let summary = Summary::new(pkg_id, vec![], Some("sha256:abc123".into()));

        resolve.add_package(pkg_id, summary);

        let retrieved = resolve.summary(pkg_id).unwrap();
        assert_eq!(retrieved.name().as_str(), "mysummary");
        assert_eq!(retrieved.version(), &Version::new(2, 0, 0));
        assert_eq!(retrieved.checksum(), Some("sha256:abc123"));
    }

    #[test]
    fn test_resolve_checksum() {
        let mut resolve = Resolve::new();
        let tmp = TempDir::new().unwrap();
        let source = SourceId::for_path(tmp.path()).unwrap();
        let pkg_id = PackageId::new("withsum", Version::new(1, 0, 0), source);
        let summary = Summary::new(pkg_id, vec![], Some("checksum123".into()));

        resolve.add_package(pkg_id, summary);

        assert_eq!(resolve.checksum(pkg_id), Some("checksum123"));
    }

    #[test]
    fn test_resolve_checksum_none() {
        let mut resolve = Resolve::new();
        let (id, summary) = create_test_summary("nosum", "1.0.0");
        resolve.add_package(id, summary);

        assert_eq!(resolve.checksum(id), None);
    }

    #[test]
    fn test_resolve_transitive_deps() {
        let mut resolve = Resolve::new();

        let (id_a, sum_a) = create_test_summary("a", "1.0.0");
        let (id_b, sum_b) = create_test_summary("b", "1.0.0");
        let (id_c, sum_c) = create_test_summary("c", "1.0.0");
        let (id_d, sum_d) = create_test_summary("d", "1.0.0");

        resolve.add_package(id_a, sum_a);
        resolve.add_package(id_b, sum_b);
        resolve.add_package(id_c, sum_c);
        resolve.add_package(id_d, sum_d);

        // a -> b -> c
        // a -> d
        resolve.add_edge(id_a, id_b);
        resolve.add_edge(id_b, id_c);
        resolve.add_edge(id_a, id_d);

        let transitive = resolve.transitive_deps(id_a);

        assert_eq!(transitive.len(), 3);
        assert!(transitive.contains(&id_b));
        assert!(transitive.contains(&id_c));
        assert!(transitive.contains(&id_d));
        assert!(!transitive.contains(&id_a)); // Should not contain itself
    }

    #[test]
    fn test_resolve_transitive_deps_empty() {
        let mut resolve = Resolve::new();
        let (id, summary) = create_test_summary("leaf", "1.0.0");
        resolve.add_package(id, summary);

        let transitive = resolve.transitive_deps(id);
        assert!(transitive.is_empty());
    }

    #[test]
    fn test_resolve_diamond_dependency() {
        let mut resolve = Resolve::new();

        let tmp = TempDir::new().unwrap();
        let source = SourceId::for_path(tmp.path()).unwrap();

        // Diamond: a -> b, a -> c, b -> d, c -> d
        let id_a = PackageId::new("a", Version::new(1, 0, 0), source);
        let id_b = PackageId::new("b", Version::new(1, 0, 0), source);
        let id_c = PackageId::new("c", Version::new(1, 0, 0), source);
        let id_d = PackageId::new("d", Version::new(1, 0, 0), source);

        resolve.add_package(id_a, Summary::new(id_a, vec![], None));
        resolve.add_package(id_b, Summary::new(id_b, vec![], None));
        resolve.add_package(id_c, Summary::new(id_c, vec![], None));
        resolve.add_package(id_d, Summary::new(id_d, vec![], None));

        resolve.add_edge(id_a, id_b);
        resolve.add_edge(id_a, id_c);
        resolve.add_edge(id_b, id_d);
        resolve.add_edge(id_c, id_d);

        let order = resolve.topological_order();

        // d must come before b and c
        // b and c must come before a
        let pos_a = order.iter().position(|&id| id == id_a).unwrap();
        let pos_b = order.iter().position(|&id| id == id_b).unwrap();
        let pos_c = order.iter().position(|&id| id == id_c).unwrap();
        let pos_d = order.iter().position(|&id| id == id_d).unwrap();

        assert!(pos_d < pos_b);
        assert!(pos_d < pos_c);
        assert!(pos_b < pos_a);
        assert!(pos_c < pos_a);
    }

    #[test]
    fn test_resolve_reverse_topological_order() {
        let mut resolve = Resolve::new();

        let (id_a, sum_a) = create_test_summary("a", "1.0.0");
        let (id_b, sum_b) = create_test_summary("b", "1.0.0");
        let (id_c, sum_c) = create_test_summary("c", "1.0.0");

        resolve.add_package(id_a, sum_a);
        resolve.add_package(id_b, sum_b);
        resolve.add_package(id_c, sum_c);

        // a -> b -> c
        resolve.add_edge(id_a, id_b);
        resolve.add_edge(id_b, id_c);

        let forward = resolve.topological_order();
        let reverse = resolve.reverse_topological_order();

        // Reverse order should be opposite
        assert_eq!(forward.len(), reverse.len());
        assert_eq!(forward[0], reverse[reverse.len() - 1]);
        assert_eq!(forward[forward.len() - 1], reverse[0]);
    }

    #[test]
    fn test_resolve_get_package_by_name() {
        let mut resolve = Resolve::new();
        let (id, summary) = create_test_summary("findme", "3.0.0");
        resolve.add_package(id, summary);

        let found = resolve.get_package_by_name("findme".into());
        assert!(found.is_some());
        assert_eq!(found.unwrap().name().as_str(), "findme");

        let not_found = resolve.get_package_by_name("nothere".into());
        assert!(not_found.is_none());
    }

    #[test]
    fn test_resolve_get_package_by_name_strict() {
        let mut resolve = Resolve::new();
        let (id, summary) = create_test_summary("unique", "1.0.0");
        resolve.add_package(id, summary);

        let result = resolve.get_package_by_name_strict("unique".into());
        assert!(result.is_ok());

        let result = resolve.get_package_by_name_strict("missing".into());
        assert!(matches!(result, Err(ResolveError::PackageNotFound { .. })));
    }

    #[test]
    fn test_resolve_get_package_with_source() {
        let tmp = TempDir::new().unwrap();
        let source = SourceId::for_path(tmp.path()).unwrap();

        let mut resolve = Resolve::new();
        let (id, summary) = create_test_summary_with_source("bysource", "1.0.0", source);
        resolve.add_package(id, summary);

        let found = resolve.get_package("bysource".into(), source);
        assert!(found.is_some());
    }

    #[test]
    fn test_resolve_unique_pkg() {
        let tmp = TempDir::new().unwrap();
        let source = SourceId::for_path(tmp.path()).unwrap();

        let mut resolve = Resolve::new();
        let pkg_id = PackageId::new("onlyone", Version::new(1, 0, 0), source);
        let summary = Summary::new(pkg_id, vec![], None);
        resolve.add_package(pkg_id, summary);

        let result = resolve.unique_pkg("onlyone".into(), source);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), pkg_id);
    }

    #[test]
    fn test_resolve_unique_pkg_not_found() {
        let tmp = TempDir::new().unwrap();
        let source = SourceId::for_path(tmp.path()).unwrap();

        let resolve = Resolve::new();
        let result = resolve.unique_pkg("missing".into(), source);

        assert!(matches!(result, Err(ResolveError::PackageNotFound { .. })));
    }

    #[test]
    fn test_resolve_packages_iterator() {
        let mut resolve = Resolve::new();
        let (id_a, sum_a) = create_test_summary("iter_a", "1.0.0");
        let (id_b, sum_b) = create_test_summary("iter_b", "2.0.0");

        resolve.add_package(id_a, sum_a);
        resolve.add_package(id_b, sum_b);

        let packages: Vec<_> = resolve.packages().collect();
        assert_eq!(packages.len(), 2);
    }

    #[test]
    fn test_resolve_get_packages_by_name() {
        let mut resolve = Resolve::new();
        let (id, summary) = create_test_summary("multi", "1.0.0");
        resolve.add_package(id, summary);

        let packages = resolve.get_packages_by_name("multi".into());
        assert_eq!(packages.len(), 1);

        let packages = resolve.get_packages_by_name("nonexistent".into());
        assert!(packages.is_empty());
    }

    #[test]
    fn test_resolve_registry_provenance() {
        use crate::resolver::encode::ResolvedSource;

        let mut resolve = Resolve::new();
        let (id, summary) = create_test_summary("prov", "1.0.0");
        resolve.add_package(id, summary);

        let provenance = RegistryProvenance {
            shim_path: "z/zlib/1.0.0.toml".to_string(),
            shim_hash: "abc123def456".to_string(),
            resolved: ResolvedSource::Tarball {
                url: "https://example.com/pkg.tar.gz".to_string(),
                sha256: "deadbeef".to_string(),
            },
        };

        resolve.set_registry_provenance(id, provenance.clone());

        let retrieved = resolve.registry_provenance(id);
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().shim_path, "z/zlib/1.0.0.toml");
    }

    #[test]
    fn test_resolve_default() {
        let resolve = Resolve::default();
        assert!(resolve.is_empty());
        assert_eq!(resolve.version(), ResolveVersion::V1);
    }

    #[test]
    fn test_resolve_version_default() {
        let version = ResolveVersion::default();
        assert_eq!(version, ResolveVersion::V1);
    }

    #[test]
    fn test_resolve_error_display() {
        let err = ResolveError::PackageNotFound {
            name: "test".into(),
        };
        assert!(err.to_string().contains("test"));
        assert!(err.to_string().contains("not found"));

        let err = ResolveError::AmbiguousPackage {
            name: "ambig".into(),
            count: 2,
            sources: "source1, source2".to_string(),
        };
        assert!(err.to_string().contains("ambiguous"));
        assert!(err.to_string().contains("ambig"));

        let err = ResolveError::MultipleVersions {
            name: "multi".into(),
            pkg_source: "registry".to_string(),
            versions: vec!["1.0.0".to_string(), "2.0.0".to_string()],
        };
        assert!(err.to_string().contains("multiple versions"));
    }
}
