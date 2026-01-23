//! Resolve - the immutable dependency graph.
//!
//! Once created, a Resolve is read-only. Only `harbour update` can
//! create a new Resolve.

use std::collections::{HashMap, HashSet};

use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::Topo;
use semver::Version;

use crate::core::{PackageId, Summary};
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

/// The resolved dependency graph.
///
/// This is an immutable representation of all packages in the dependency tree
/// along with their exact versions and relationships.
#[derive(Debug, Clone)]
pub struct Resolve {
    /// Package graph
    graph: DiGraph<PackageId, ()>,

    /// Map from PackageId to node index
    pkg_to_node: HashMap<PackageId, NodeIndex>,

    /// Map from package name to PackageId
    name_to_pkg: HashMap<InternedString, PackageId>,

    /// Summaries for each package
    summaries: HashMap<PackageId, Summary>,

    /// Checksums for verification
    checksums: HashMap<PackageId, Option<String>>,

    /// Format version
    version: ResolveVersion,
}

impl Resolve {
    /// Create a new empty Resolve.
    pub fn new() -> Self {
        Resolve {
            graph: DiGraph::new(),
            pkg_to_node: HashMap::new(),
            name_to_pkg: HashMap::new(),
            summaries: HashMap::new(),
            checksums: HashMap::new(),
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
        self.name_to_pkg.insert(pkg_id.name(), pkg_id);

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
    pub fn get_package_by_name(&self, name: InternedString) -> Option<PackageId> {
        self.name_to_pkg.get(&name).copied()
    }

    /// Get the summary for a package.
    pub fn summary(&self, pkg_id: PackageId) -> Option<&Summary> {
        self.summaries.get(&pkg_id)
    }

    /// Get the checksum for a package.
    pub fn checksum(&self, pkg_id: PackageId) -> Option<&str> {
        self.checksums.get(&pkg_id).and_then(|c| c.as_deref())
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
            self.graph
                .neighbors(node)
                .map(|n| self.graph[n])
                .collect()
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
        self.name_to_pkg
            .keys()
            .any(|n| n.as_str() == name)
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
    use tempfile::TempDir;

    fn create_test_summary(name: &str, version: &str) -> (PackageId, Summary) {
        let tmp = TempDir::new().unwrap();
        let source = SourceId::for_path(tmp.path()).unwrap();
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
}
