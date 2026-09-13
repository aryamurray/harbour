//! Dependency resolution.
//!
//! This module implements PubGrub-based version resolution for Harbour packages.
//! The resolver is pure and deterministic - all I/O happens before resolution.

pub mod cpp_constraints;
pub mod encode;
pub mod errors;
pub mod resolve;
pub mod version;

pub use cpp_constraints::{
    warn_ignored_dependency_build_sections, warn_ignored_dependency_profiles, CppConstraints,
};
pub use resolve::{Resolve, ResolveError};

use std::cell::RefCell;
use std::collections::HashMap;
use std::error::Error as StdError;
use std::fmt;

use anyhow::{bail, Result};
use pubgrub::{
    DefaultStringReporter, Dependencies, DependencyProvider, PackageResolutionStatistics, Range,
    Reporter,
};
use semver::Version;

use crate::core::{Dependency, SourceId, Summary};
use crate::sources::SourceCache;
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
///
/// Candidate discovery is **lazy**: rather than precomputing the whole pool
/// of available packages up front, summaries for a `(name, source_id)` pair
/// are fetched from the [`SourceCache`] the first time PubGrub actually asks
/// about that package (`choose_version` or `get_dependencies`), and cached
/// from then on. This is the same shape Cargo and PubGrub's
/// [`DependencyProvider`] are designed for, and it is what makes a git or
/// registry dependency's *own* dependencies resolvable without walking (and
/// fetching) the whole transitive closure eagerly - a git/registry walk
/// means cloning a repository, unlike a path dependency's walk, which is
/// just a local file read.
///
/// `DependencyProvider` methods take `&self` (PubGrub drives resolution
/// through a shared reference), so the caches below use [`RefCell`] for
/// interior mutability. Resolution is single-threaded, so this is sound.
pub struct HarbourResolver<'a> {
    /// Summaries fetched so far, keyed by the *unpinned* `(name,
    /// source_id)` a dependency requirement declares (never the `precise`
    /// variant a resolved package ends up pinned to - see
    /// [`SourceId::is_same_source`](crate::core::SourceId::is_same_source)).
    ///
    /// Keying on `source_id` (not just `name`) is deliberate: two different
    /// sources that happen to produce a same-named package (e.g. `zlib` via
    /// the registry for one dependent, and via a `git` URL for another) are
    /// tracked as entirely separate candidate pools, exactly mirroring
    /// `PubGrubPackage`'s own identity. See
    /// [`Resolve::duplicate_name_sources`] for what happens if both end up
    /// in the final solution.
    summaries: RefCell<HashMap<(InternedString, SourceId), Vec<Summary>>>,

    /// `(name, source_id)` pairs whose fetch failed, and the resulting
    /// error message. Memoized so a candidate PubGrub keeps asking about
    /// (e.g. across backtracking) is not fetched - and does not fail -
    /// more than once.
    fetch_errors: RefCell<HashMap<(InternedString, SourceId), String>>,

    /// Root package (a workspace member; not fetched through `source_cache`).
    root: Summary,

    /// Optional dependencies that some enabled feature has activated,
    /// keyed by the name of the package that *declares* them.
    ///
    /// A dependency marked `optional = true` whose name is not listed here
    /// for its declaring package is invisible to resolution: PubGrub is
    /// never told about it, so [`Self::ensure_fetched`] is never called for
    /// it and its source is never cloned, downloaded or queried. That is
    /// the whole point of the key - skipping the *build* of an already
    /// fetched package would be a much weaker promise.
    ///
    /// Keyed by package name alone, not by `(name, source_id)`: Harbour
    /// refuses a graph containing one package name from two sources (see
    /// [`Resolve::duplicate_name_sources`]), so within a single resolution
    /// a name identifies a package. Keying by `SourceId` here would have to
    /// deal with the pinned/unpinned distinction described on `summaries`,
    /// which is exactly the mismatch that would silently never match.
    ///
    /// Populated by the caller across resolution rounds - see
    /// `ops::resolve::resolve_fresh`, which grows this set until it stops
    /// changing.
    active_optional: HashMap<InternedString, std::collections::BTreeSet<String>>,

    /// Extra requirements to resolve *alongside* the root's own, without
    /// becoming dependencies of it.
    ///
    /// This is how a workspace with several members gets one graph. PubGrub
    /// solves from a single root, and the root here is `members[0]`, so a
    /// member nothing depends on was simply absent from the solution --
    /// which is why `harbour build -p other` could validate the name,
    /// announce it, and then build `app` (#143).
    ///
    /// Listed only where PubGrub asks the *root* for its dependencies. It is
    /// deliberately **not** added to the root's summary, and the distinction
    /// is the whole reason this field exists rather than a synthesised root:
    /// both the dependency edges in [`Resolve`] and the lockfile are derived
    /// from `Summary::dependencies`, so a member spliced in there would
    /// become a real dependency of `members[0]` -- folded into its compile
    /// surface, appended to its link line, and written into the lockfile.
    /// Two members of one workspace would silently link each other.
    extra_root_deps: Vec<Dependency>,

    /// Where to fetch candidates from, on demand.
    source_cache: RefCell<&'a mut SourceCache>,
}

impl<'a> HarbourResolver<'a> {
    /// Create a new resolver with the root package.
    ///
    /// `source_cache` is consulted lazily as PubGrub encounters candidate
    /// packages during resolution; no dependency's source is fetched unless
    /// PubGrub actually asks about it.
    pub fn new(root: Summary, source_cache: &'a mut SourceCache) -> Self {
        HarbourResolver {
            summaries: RefCell::new(HashMap::new()),
            fetch_errors: RefCell::new(HashMap::new()),
            root,
            active_optional: HashMap::new(),
            extra_root_deps: Vec::new(),
            source_cache: RefCell::new(source_cache),
        }
    }

    /// Resolve these requirements as well, without making them
    /// dependencies of the root. See [`Self::extra_root_deps`].
    pub fn with_extra_root_deps(mut self, deps: Vec<Dependency>) -> Self {
        self.extra_root_deps = deps;
        self
    }

    /// Declare which optional dependencies are activated, keyed by the name
    /// of the package that declares them. See [`Self::active_optional`].
    pub fn with_active_optional(
        mut self,
        active: HashMap<InternedString, std::collections::BTreeSet<String>>,
    ) -> Self {
        self.active_optional = active;
        self
    }

    /// Whether `dep`, declared by the package named `declared_by`, takes
    /// part in this resolution.
    ///
    /// Required dependencies always do. An optional one does only if some
    /// enabled feature activated it.
    fn dep_is_active(&self, declared_by: InternedString, dep: &Dependency) -> bool {
        if !dep.is_optional() {
            return true;
        }
        self.active_optional
            .get(&declared_by)
            .is_some_and(|active| active.contains(dep.name().as_str()))
    }

    /// Seed the cache with the result of already querying `dep`'s source
    /// (e.g. a workspace member's direct dependencies, which the caller
    /// resolved with workspace context - local-first member matching,
    /// `[workspace.dependencies]` inheritance - that a bare source-cache
    /// fetch wouldn't know about).
    ///
    /// This is purely an optimization / correctness aid for the seeded
    /// entry itself: anything not seeded here is still discovered lazily
    /// when PubGrub asks about it.
    ///
    /// Deliberately keyed by `dep.name()`/`dep.source_id()` (the *unpinned*
    /// source a dependency requirement declares), not by anything read off
    /// the returned summaries - a registry/git summary's own `source_id()`
    /// is `precise`-pinned (see [`SourceId::is_same_source`]), so keying by
    /// that would silently never be found by the (unpinned) lookups
    /// `choose_version`/`get_dependencies` perform.
    pub fn seed(&mut self, dep: &Dependency, summaries: Vec<Summary>) {
        let key = (dep.name(), dep.source_id());
        self.summaries
            .borrow_mut()
            .entry(key)
            .or_default()
            .extend(summaries);
    }

    /// Get the root package.
    pub fn root(&self) -> &Summary {
        &self.root
    }

    /// Ensure summaries for `(name, source_id)` are available, fetching
    /// them from the source cache on first request and caching the result
    /// (success or failure) so later requests for the same pair are free.
    fn ensure_fetched(
        &self,
        name: InternedString,
        source_id: SourceId,
    ) -> std::result::Result<(), ResolverError> {
        if self.summaries.borrow().contains_key(&(name, source_id)) {
            return Ok(());
        }

        if let Some(msg) = self.fetch_errors.borrow().get(&(name, source_id)) {
            return Err(ResolverError(msg.clone()));
        }

        let dep = Dependency::new(name, source_id);

        match self.source_cache.borrow_mut().query(&dep) {
            Ok(found) => {
                self.summaries.borrow_mut().insert((name, source_id), found);
                Ok(())
            }
            Err(e) => {
                let msg = format!(
                    "failed to fetch package `{}` from `{}`: {:#}",
                    name, source_id, e
                );
                self.fetch_errors
                    .borrow_mut()
                    .insert((name, source_id), msg.clone());
                Err(ResolverError(msg))
            }
        }
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
                let summaries = self.summaries.borrow();

                for (pkg, version) in &solution {
                    if pkg.name == self.root.name() && pkg.source_id == self.root.source_id() {
                        resolve.add_package(self.root.package_id(), self.root.clone());
                        continue;
                    }

                    if let Some(candidates) = summaries.get(&(pkg.name, pkg.source_id)) {
                        if let Some(summary) = candidates.iter().find(|s| s.version() == version) {
                            resolve.add_package(summary.package_id(), summary.clone());
                        }
                    }
                }
                drop(summaries);

                // Add dependency edges. A dependency's `source_id` is the
                // *unpinned* one declared in the manifest; the resolved
                // package it points at is stored under a *pinned* one - see
                // `find_package`.
                let packages: Vec<_> = resolve.packages().map(|(id, s)| (*id, s.clone())).collect();
                for (pkg_id, summary) in packages {
                    for dep in summary.dependencies() {
                        // An optional dependency this package did not
                        // activate gets no edge even when it is in the
                        // graph for someone else's sake: the edge is what
                        // the compile and link surfaces are folded over,
                        // so adding it would link a library this package
                        // never asked for.
                        if !self.dep_is_active(summary.name(), dep) {
                            continue;
                        }
                        if let Some(dep_id) = resolve.find_package(dep.name(), dep.source_id()) {
                            resolve.add_edge(pkg_id, dep_id);
                        }
                    }
                }

                if let Some((name, sources)) = resolve.duplicate_name_sources() {
                    let sources_desc = sources
                        .iter()
                        .map(|s| s.to_string())
                        .collect::<Vec<_>>()
                        .join(", ");
                    bail!(
                        "package `{name}` is available from more than one source: {sources_desc}\n\
                         help: Harbour links one copy of each package, so all dependents of \
                         `{name}` must agree on a single source (the same registry entry, or \
                         the same `git` URL) - otherwise the build would silently link two \
                         copies and risk duplicate symbols",
                    );
                }

                if let Some(cycle) = resolve.find_cycle() {
                    let desc = cycle
                        .iter()
                        .map(|id| id.name().to_string())
                        .collect::<Vec<_>>()
                        .join(" -> ");
                    bail!(
                        "cycle detected in dependency graph: {desc}\n\
                         help: package dependencies must form a directed acyclic graph - \
                         there is no valid build order for a cycle",
                    );
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

impl DependencyProvider for HarbourResolver<'_> {
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
        // Higher priority = resolved first.
        // Prioritize packages with fewer available versions. Fetching here
        // (rather than treating an unfetched package as "unknown, low
        // priority") keeps this consistent with `choose_version`, and the
        // fetch is memoized so this costs nothing beyond the first call.
        if self.ensure_fetched(package.name, package.source_id).is_ok() {
            if let Some(summaries) = self
                .summaries
                .borrow()
                .get(&(package.name, package.source_id))
            {
                return (1000 - summaries.len().min(1000)) as u32;
            }
        }
        1000
    }

    fn choose_version(
        &self,
        package: &Self::P,
        range: &Self::VS,
    ) -> Result<Option<Self::V>, Self::Err> {
        // For root package
        if package.name == self.root.name() && package.source_id == self.root.source_id() {
            let version = self.root.version().clone();
            if range.contains(&version) {
                return Ok(Some(version));
            }
            return Ok(None);
        }

        self.ensure_fetched(package.name, package.source_id)?;

        // Find the highest matching version
        let summaries = self.summaries.borrow();
        if let Some(candidates) = summaries.get(&(package.name, package.source_id)) {
            let mut matching: Vec<_> = candidates
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
        if package.name == self.root.name()
            && package.source_id == self.root.source_id()
            && version == self.root.version()
        {
            // The root's own requirements, plus the workspace's other
            // members (`extra_root_deps`). The two are indistinguishable to
            // PubGrub -- which is the point, since the members must resolve
            // to one consistent set of versions -- and distinguishable
            // everywhere afterwards, because only the root's *summary* is
            // used to build the graph's edges.
            let deps = self
                .root
                .dependencies()
                .iter()
                .chain(self.extra_root_deps.iter())
                .filter(|dep| self.dep_is_active(self.root.name(), dep))
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

        self.ensure_fetched(package.name, package.source_id)?;

        // Find the summary for this version
        let summaries = self.summaries.borrow();
        if let Some(candidates) = summaries.get(&(package.name, package.source_id)) {
            if let Some(summary) = candidates.iter().find(|s| s.version() == version) {
                let deps = summary
                    .dependencies()
                    .iter()
                    .filter(|dep| self.dep_is_active(summary.name(), dep))
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

        let mut cache = SourceCache::new(tmp.path().join("cache"));
        let resolver = HarbourResolver::new(root, &mut cache);
        let resolve = resolver.resolve().unwrap();

        assert_eq!(resolve.packages().count(), 1);
    }
}
