//! Surface propagation algorithm.
//!
//! This module computes the effective compile and link surfaces for a target
//! by propagating public surfaces from dependencies.

use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::Result;
use thiserror::Error;

use crate::core::features::{dependency_feature_requests, resolve_features, FeatureSet};
use crate::core::surface::{CompileRequirements, Define, LibRef, LinkRequirements, TargetPlatform};
use crate::core::target::{TargetKind, Visibility};
use crate::core::{Package, PackageId, Target};
use crate::resolver::Resolve;
use crate::sources::SourceCache;

/// Errors that can occur during surface resolution.
#[derive(Debug, Error)]
pub enum SurfaceResolveError {
    /// A dependency specified in target.deps was not found in the resolve graph.
    #[error(
        "in target `{target_name}`: dependency `{dep_name}` not found\n\
             help: add `{dep_name}` to [dependencies]"
    )]
    DependencyNotFound {
        target_name: String,
        dep_name: String,
    },

    /// A dependency name is ambiguous (multiple packages with same name from different sources).
    #[error(
        "in target `{target_name}`: dependency `{dep_name}` is ambiguous\n\
             candidates: {candidates:?}\n\
             help: disambiguate by source in target deps"
    )]
    DependencyAmbiguous {
        target_name: String,
        dep_name: String,
        candidates: Vec<String>,
    },

    /// A target specified in target.deps was not found in the dependency package.
    #[error(
        "in target `{target_name}`: target `{dep_target}` not found in `{dep_pkg}`\n\
             available: {available:?}"
    )]
    TargetNotFound {
        target_name: String,
        dep_pkg: String,
        dep_target: String,
        available: Vec<String>,
    },
}

/// Indicates where a flag/setting originated from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provenance {
    /// The package that contributed this value.
    pub package_id: PackageId,
    /// Which surface section it came from.
    pub surface_kind: SurfaceKind,
}

impl fmt::Display for Provenance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {} ({})",
            self.package_id.name(),
            self.package_id.version(),
            self.surface_kind
        )
    }
}

/// Indicates which surface section a value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceKind {
    CompilePublic,
    CompilePrivate,
    LinkPublic,
    LinkPrivate,
}

impl fmt::Display for SurfaceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SurfaceKind::CompilePublic => write!(f, "surface.compile.public"),
            SurfaceKind::CompilePrivate => write!(f, "surface.compile.private"),
            SurfaceKind::LinkPublic => write!(f, "surface.link.public"),
            SurfaceKind::LinkPrivate => write!(f, "surface.link.private"),
        }
    }
}

/// A value paired with its provenance information.
#[derive(Debug, Clone)]
pub struct WithProvenance<T> {
    pub value: T,
    pub provenance: Provenance,
}

impl<T> WithProvenance<T> {
    pub fn new(value: T, package_id: PackageId, surface_kind: SurfaceKind) -> Self {
        WithProvenance {
            value,
            provenance: Provenance {
                package_id,
                surface_kind,
            },
        }
    }
}

/// Resolved compile environment with provenance tracking.
#[derive(Debug, Clone, Default)]
pub struct EffectiveCompileSurfaceWithProvenance {
    pub include_dirs: Vec<WithProvenance<PathBuf>>,
    pub defines: Vec<WithProvenance<Define>>,
    pub cflags: Vec<WithProvenance<String>>,
}

impl EffectiveCompileSurfaceWithProvenance {
    /// Convert to compiler flags (for actual compilation, without provenance).
    pub fn to_flags(&self) -> Vec<String> {
        let mut flags = Vec::new();

        for item in &self.include_dirs {
            flags.push(format!("-I{}", item.value.display()));
        }

        for item in &self.defines {
            flags.push(item.value.to_flag());
        }

        for item in &self.cflags {
            flags.push(item.value.clone());
        }

        flags
    }
}

/// Resolved link environment with provenance tracking.
#[derive(Debug, Clone, Default)]
pub struct EffectiveLinkSurfaceWithProvenance {
    pub libs: Vec<WithProvenance<LibRef>>,
    pub lib_dirs: Vec<WithProvenance<PathBuf>>,
    pub ldflags: Vec<WithProvenance<String>>,
    pub frameworks: Vec<WithProvenance<String>>,
    pub dep_libs: Vec<WithProvenance<PathBuf>>,
}

impl EffectiveLinkSurfaceWithProvenance {
    /// Convert to linker flags (for actual linking, without provenance).
    pub fn to_flags(&self) -> Vec<String> {
        let mut flags = Vec::new();

        // Library search paths
        for item in &self.lib_dirs {
            flags.push(format!("-L{}", item.value.display()));
        }

        // Built dependency libraries (full paths)
        for item in &self.dep_libs {
            flags.push(item.value.display().to_string());
        }

        // System libraries
        for item in &self.libs {
            flags.extend(item.value.to_flags());
        }

        // Frameworks
        for item in &self.frameworks {
            flags.push("-framework".to_string());
            flags.push(item.value.clone());
        }

        // Additional flags
        for item in &self.ldflags {
            flags.push(item.value.clone());
        }

        flags
    }
}

/// Resolved compile environment for a target.
#[derive(Debug, Clone, Default)]
pub struct EffectiveCompileSurface {
    /// Include directories
    pub include_dirs: Vec<PathBuf>,
    /// Preprocessor defines
    pub defines: Vec<Define>,
    /// Compiler flags
    pub cflags: Vec<String>,
}

/// Resolved link environment for a target.
#[derive(Debug, Clone, Default)]
pub struct EffectiveLinkSurface {
    /// Libraries to link
    pub libs: Vec<LibRef>,
    /// Library search paths
    pub lib_dirs: Vec<PathBuf>,
    /// Linker flags
    pub ldflags: Vec<String>,
    /// Frameworks (macOS)
    pub frameworks: Vec<String>,
    /// Built dependency libraries (paths to .a/.so files)
    pub dep_libs: Vec<PathBuf>,
    /// Link groups for controlling link order
    pub groups: Vec<crate::core::surface::LinkGroup>,
}

/// Compute each package's resolved, unified feature set.
///
/// # Why "unified"
///
/// A C dependency graph builds one, and only one, copy of any given
/// library: if package `A` depends on `zlib` with feature `x` and package
/// `B` depends on `zlib` with feature `y`, there is exactly one `zlib` in
/// the final link, so it must be built with the union `{x, y}` -- never
/// with `x` alone or `y` alone, and never as two separate builds (that
/// would be duplicate symbols, not a version skew, which is what makes this
/// different from Cargo's crate-per-feature-set monomorphization). This is
/// the reason a package's feature set cannot be computed locally, target by
/// target, the way `Surface::resolve` computes flags: it needs the whole
/// graph's demands on that one package gathered first.
///
/// # Algorithm
///
/// For every package in `packages`, for every *direct* dependency edge
/// `dependent -> dependency` in `resolve`, read `dependent`'s manifest
/// `[dependencies]` entry for `dependency`'s name (by convention the same
/// name `Target.deps`/`SurfaceResolver` already key by) and fold in:
/// - its requested `features = [...]` (unioned across all such edges), and
/// - whether it wants default features (`default-features` defaults to
///   `true`; the union rule is an OR, matching Cargo: defaults are only
///   left off a package if *every* dependent that reaches it opted out).
///
/// A package with no dependents (typically a root/workspace package) gets
/// default features enabled and no additional requested features -- the
/// vacuous case of "every dependent (there are none) wants defaults".
///
/// Once the per-package requested set is known, `features::resolve_features`
/// expands it against that package's own `[features]` declaration.
///
/// # `dep/feature` propagation
///
/// A package's `[features]` table may enable a feature on one of its own
/// dependencies via Cargo's `dep/feature` syntax (e.g. `want =
/// ["inner/deep"]`; see `core::features` module docs). Naively that is a
/// fixpoint problem: a package's enabled features can request features on
/// its dependencies, whose newly-enabled features can request features on
/// *their* dependencies, and so on. This does not need iterating to a
/// fixpoint, because the package graph is a DAG (cycles are rejected
/// upstream in the resolver) and `dep/feature` requests only ever flow from
/// a dependent to its dependencies -- never the reverse. Processing
/// packages in [`Resolve::reverse_topological_order`] (dependents before
/// dependencies -- the same order already used for static link ordering)
/// therefore finalizes every package's requested set, and hence its
/// resolved feature set, strictly before any of its dependencies are
/// resolved. One pass suffices: by the time a package `D` is reached, every
/// dependent of `D` (however many diamonds converge on it) has already
/// contributed its `dep/feature` requests into `requested[D]`.
///
/// Each request is validated against the graph before being folded in: the
/// name before the `/` must be an actual dependency of the declaring
/// package (by the same `Dependency`-name convention `target.deps` already
/// keys by), and the feature named after the `/` must be one the dependency
/// actually declares in its own `[features]` table. Both failures are
/// attributed to the *declaring* package (it is the one that wrote a
/// request that cannot be satisfied), not to the dependency.
///
/// # Known limitation
///
/// This reads each dependent's *raw* `[dependencies]` entry
/// (`DependencySpec`), not the fully resolved `Dependency` that
/// `resolve_dependency` would produce. That means a `workspace = true`
/// entry that inherits `features`/`default-features` from
/// `[workspace.dependencies]` is not expanded here -- its local
/// `features = [...]` override (if any) is still honored, but the
/// inherited base features are not. Path/git/registry/version-pinned
/// dependency entries (the common case, and the one the sqlite/unification
/// validation in this change exercises) are handled fully. Closing this gap
/// would mean threading workspace context into this function the way
/// `resolve_dependency` already receives it; deferred rather than done
/// half-right under time pressure.
pub fn compute_feature_sets(
    resolve: &Resolve,
    packages: &HashMap<PackageId, Package>,
) -> Result<HashMap<PackageId, FeatureSet>> {
    // requested[dep_id] = union of feature names requested of dep_id, either
    // by a dependent's manifest `features = [...]` entry, or propagated via
    // some dependent's own enabled `dep/feature` entry (added below, in
    // topological order).
    let mut requested: HashMap<PackageId, BTreeSet<String>> = HashMap::new();
    // default_wanted[dep_id] = true as soon as *any* dependent wants
    // default features (OR, per the doc comment above).
    let mut default_wanted: HashMap<PackageId, bool> = HashMap::new();

    for (dependent_id, package) in packages {
        for dep_id in resolve.deps(*dependent_id) {
            let dep_name = dep_id.name();
            let Some(spec) = package.manifest().dependencies.get(dep_name.as_str()) else {
                // Not directly named in this manifest's [dependencies]
                // (e.g. reached only via [workspace.dependencies] under a
                // different local alias) -- nothing to fold in from this
                // edge.
                continue;
            };

            let (feats, wants_default) = match spec {
                crate::core::dependency::DependencySpec::Simple(_) => (Vec::new(), true),
                crate::core::dependency::DependencySpec::Detailed(d) => (
                    d.features.clone().unwrap_or_default(),
                    d.default_features.unwrap_or(true),
                ),
            };

            requested.entry(dep_id).or_default().extend(feats);
            let entry = default_wanted.entry(dep_id).or_insert(false);
            *entry = *entry || wants_default;
        }
    }

    let mut result = HashMap::with_capacity(packages.len());

    // Dependents before dependencies: any `dep/feature` request discovered
    // while resolving a package's own feature set below must land in
    // `requested` before that dependency is itself resolved. See the
    // "`dep/feature` propagation" doc comment above for why one pass over
    // this order is sufficient even through diamonds.
    for pkg_id in resolve.reverse_topological_order() {
        let Some(package) = packages.get(&pkg_id) else {
            // Not one of the packages we were asked to compute for (e.g. a
            // node reachable in `resolve` but not loaded) -- nothing to do,
            // and no feature set to propagate from.
            continue;
        };

        // No dependents -> vacuously "every dependent wants defaults".
        let default_features = default_wanted.get(&pkg_id).copied().unwrap_or(true);
        let reqs: Vec<String> = requested
            .get(&pkg_id)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default();
        let defs = &package.manifest().features;
        let enabled = resolve_features(defs, &reqs, default_features).map_err(|e| {
            anyhow::anyhow!("resolving features for package `{}`: {}", pkg_id.name(), e)
        })?;

        // Propagate any `dep/feature` entries reachable from this package's
        // now-final enabled set onto the named dependency's requested set.
        for (dep_name, feats) in dependency_feature_requests(defs, &enabled) {
            let dep_pkg_id = resolve
                .deps(pkg_id)
                .into_iter()
                .find(|id| id.name().as_str() == dep_name)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "package `{}` requests feature(s) of `{dep_name}` via \
                         `{dep_name}/...`, but `{dep_name}` is not a dependency of `{}`",
                        pkg_id.name(),
                        pkg_id.name()
                    )
                })?;

            // `reverse_topological_order` puts dependents before
            // dependencies, so a dependency must not have been finalized
            // yet -- if it has, the edge we propagated along was missing
            // from the graph and this request would be silently dropped
            // (the dependency would build without the requested feature).
            if result.contains_key(&dep_pkg_id) {
                anyhow::bail!(
                    "internal error: package `{}` requests `{dep_name}/...`, but \
                     `{dep_name}`'s feature set was already finalized -- the \
                     dependency edge is missing from the resolve graph, so the \
                     request would be silently dropped",
                    pkg_id.name()
                );
            }

            let dep_defs = &packages
                .get(&dep_pkg_id)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "package `{}` requests feature(s) of `{dep_name}`, but `{dep_name}` was not loaded",
                        pkg_id.name()
                    )
                })?
                .manifest()
                .features;

            for feat in feats {
                if !dep_defs.contains_key(&feat) {
                    anyhow::bail!(
                        "package `{}` requests feature `{feat}` of `{dep_name}` via \
                         `{dep_name}/{feat}`, but `{dep_name}` does not declare a feature \
                         named `{feat}`",
                        pkg_id.name()
                    );
                }
                requested.entry(dep_pkg_id).or_default().insert(feat);
            }
        }

        result.insert(pkg_id, enabled);
    }

    Ok(result)
}

/// Resolves effective surfaces for targets.
pub struct SurfaceResolver<'a> {
    resolve: &'a Resolve,
    platform: &'a TargetPlatform,
    packages: HashMap<PackageId, Package>,
    /// Each package's resolved, unified feature set (see
    /// [`compute_feature_sets`]). Populated by [`Self::load_packages`];
    /// empty until then.
    features: HashMap<PackageId, FeatureSet>,
    /// `(consumer target, dependency)` pairs already warned about for having
    /// several library targets. The link surface is resolved more than once
    /// per build (the plan, then `compile_commands.json`), and the warning
    /// is about the manifest, so repeating it adds nothing.
    warned_multi_lib: std::cell::RefCell<std::collections::HashSet<(String, PackageId)>>,
}

impl<'a> SurfaceResolver<'a> {
    /// Create a new surface resolver.
    pub fn new(resolve: &'a Resolve, platform: &'a TargetPlatform) -> Self {
        SurfaceResolver {
            resolve,
            platform,
            packages: HashMap::new(),
            features: HashMap::new(),
            warned_multi_lib: std::cell::RefCell::new(std::collections::HashSet::new()),
        }
    }

    /// Load packages for all resolved dependencies, then compute each
    /// package's unified feature set (see [`compute_feature_sets`]).
    pub fn load_packages(&mut self, source_cache: &mut SourceCache) -> Result<()> {
        for (pkg_id, _) in self.resolve.packages() {
            if !self.packages.contains_key(pkg_id) {
                let package = source_cache.load_package(*pkg_id)?;
                self.packages.insert(*pkg_id, package);
            }
        }
        self.features = compute_feature_sets(self.resolve, &self.packages)?;
        Ok(())
    }

    /// Get a loaded package by ID.
    pub fn get_package(&self, pkg_id: PackageId) -> Option<&Package> {
        self.packages.get(&pkg_id)
    }

    /// Get the resolved, unified feature set for a package.
    ///
    /// Returns an empty set for a package not found (e.g. before
    /// `load_packages` has run) rather than erroring, since an empty
    /// feature set is a safe, conservative default -- every `feature =
    /// "..."` condition simply fails to match.
    pub fn features_for(&self, pkg_id: PackageId) -> FeatureSet {
        self.features.get(&pkg_id).cloned().unwrap_or_default()
    }

    /// Compute the effective compile surface for a target.
    ///
    /// Algorithm:
    /// 1. Validate that all deps in target.deps exist in the resolve graph
    /// 2. Start with target's private compile surface
    /// 3. Add target's public compile surface
    /// 4. For each dependency (transitively):
    ///    - Check target.deps for visibility override
    ///    - If public (or not overridden), add dependency's public compile surface
    pub fn resolve_compile_surface(
        &self,
        pkg_id: PackageId,
        target: &Target,
    ) -> Result<EffectiveCompileSurface> {
        let mut effective = EffectiveCompileSurface::default();

        // Get package
        let package = self
            .packages
            .get(&pkg_id)
            .ok_or_else(|| anyhow::anyhow!("package not loaded: {}", pkg_id))?;

        // Validate target.deps - ensure all referenced deps exist in resolve
        for dep_name in target.deps.keys() {
            match self.resolve.get_package_by_name_strict(*dep_name) {
                Ok(_) => { /* found, continue */ }
                Err(crate::resolver::ResolveError::PackageNotFound { .. }) => {
                    return Err(SurfaceResolveError::DependencyNotFound {
                        target_name: target.name.to_string(),
                        dep_name: dep_name.to_string(),
                    }
                    .into());
                }
                Err(crate::resolver::ResolveError::AmbiguousPackage { sources, .. }) => {
                    return Err(SurfaceResolveError::DependencyAmbiguous {
                        target_name: target.name.to_string(),
                        dep_name: dep_name.to_string(),
                        candidates: sources.split(", ").map(|s| s.to_string()).collect(),
                    }
                    .into());
                }
                Err(e) => {
                    return Err(e.into());
                }
            }
        }

        // This target's own package's feature set -- never a dependent's --
        // see the doc comment on `Target::resolved_sources`.
        let own_features = self.features_for(pkg_id);

        // Resolve the target's surface
        let resolved = target.surface.resolve(self.platform, &own_features);

        // Add private (only for this target's sources)
        self.add_compile_requirements(&mut effective, &resolved.compile_private, package.root());

        // Add feature/platform-conditional private compile requirements
        // (defines, cflags) contributed via `[[targets.X.when]]` -- see
        // `Target::resolved_extra_compile`.
        let extra = target.resolved_extra_compile(self.platform, &own_features);
        self.add_compile_requirements(&mut effective, &extra, package.root());

        // Add public
        self.add_compile_requirements(&mut effective, &resolved.compile_public, package.root());

        // A private define referenced from a public header is an ABI trap: the
        // library compiles the header one way and every consumer compiles it
        // another. See `warn_private_defines_in_public_headers`.
        warn_private_defines_in_public_headers(
            target,
            package.root(),
            &resolved.compile_private,
            &extra,
            &resolved.compile_public,
        );

        // Determine effective dependencies - use target.deps if specified
        let transitive_deps = self.resolve.transitive_deps(pkg_id);

        for dep_id in transitive_deps {
            // Check if target.deps specifies visibility for this dependency
            let visibility = self.get_compile_visibility(target, dep_id);

            // Only include if public visibility
            if visibility == Visibility::Private {
                continue;
            }

            if let Some(dep_package) = self.packages.get(&dep_id) {
                // Get the specific target if specified in target.deps
                let dep_target = self.get_dep_target(target, dep_id, dep_package)?;
                if let Some(dt) = dep_target {
                    let dep_features = self.features_for(dep_id);
                    let dep_resolved = dt.surface.resolve(self.platform, &dep_features);
                    self.add_compile_requirements(
                        &mut effective,
                        &dep_resolved.compile_public,
                        dep_package.root(),
                    );
                }
            }
        }

        // Deduplicate
        effective.include_dirs.sort();
        effective.include_dirs.dedup();
        effective.cflags.sort();
        effective.cflags.dedup();

        Ok(effective)
    }

    /// Get compile visibility for a dependency from target.deps.
    /// Returns Public if not specified (default).
    /// O(1) lookup using HashMap.
    fn get_compile_visibility(&self, target: &Target, dep_id: PackageId) -> Visibility {
        target
            .deps
            .get(&dep_id.name())
            .map(|spec| spec.compile)
            .unwrap_or(Visibility::Public)
    }

    /// Get link visibility for a dependency from target.deps.
    /// Returns Public if not specified (default).
    /// O(1) lookup using HashMap.
    fn get_link_visibility(&self, target: &Target, dep_id: PackageId) -> Visibility {
        target
            .deps
            .get(&dep_id.name())
            .map(|spec| spec.link)
            .unwrap_or(Visibility::Public)
    }

    /// Get the target to use from a dependency package.
    /// Respects target.deps[pkg].target if specified, otherwise uses default_target.
    /// O(1) lookup using HashMap.
    ///
    /// Returns an error if a specific target was requested but not found.
    fn get_dep_target<'b>(
        &self,
        target: &Target,
        dep_id: PackageId,
        dep_package: &'b Package,
    ) -> Result<Option<&'b Target>, SurfaceResolveError> {
        // Check if target.deps specifies a specific target (O(1) lookup)
        if let Some(dep_spec) = target.deps.get(&dep_id.name()) {
            if let Some(ref target_name) = dep_spec.target {
                return match dep_package.target(target_name) {
                    Some(t) => Ok(Some(t)),
                    None => Err(SurfaceResolveError::TargetNotFound {
                        target_name: target.name.to_string(),
                        dep_pkg: dep_id.name().to_string(),
                        dep_target: target_name.clone(),
                        available: dep_package
                            .targets()
                            .iter()
                            .map(|t| t.name.to_string())
                            .collect(),
                    }),
                };
            }
        }

        // Fall back to default target -- the first library target. A
        // package exposing several libraries (openssl ships libcrypto and
        // libssl; so do most large C projects) therefore contributes only
        // one archive, and the rest are dropped. Silently, until now: the
        // symptom was a pile of undefined symbols from the consumer's
        // linker with nothing pointing at the cause.
        //
        // Linking all of them is not a drop-in fix, because static archive
        // order matters and `target.deps` is keyed by package with a single
        // optional `target`, so there is nothing in the manifest that can
        // express one library needing another's symbols. Naming the
        // limitation is what is possible today.
        let linkable: Vec<&Target> = dep_package
            .targets()
            .iter()
            .filter(|t| t.kind.is_linkable())
            .collect();
        let first_time = self
            .warned_multi_lib
            .borrow_mut()
            .insert((target.name.to_string(), dep_id));
        if linkable.len() > 1 && first_time {
            tracing::warn!(
                "dependency `{}` has {} library targets ({}), but only `{}` is linked \
                 into `{}`. Pick one explicitly with `target = \"...\"` under \
                 `[targets.{}.deps.{}]`, or have the dependency expose a single target \
                 covering everything a consumer needs.",
                dep_id.name(),
                linkable.len(),
                linkable
                    .iter()
                    .map(|t| t.name.to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
                linkable[0].name,
                target.name,
                target.name,
                dep_id.name(),
            );
        }

        Ok(dep_package.default_target())
    }

    /// Compute the packages whose libraries must be linked directly into
    /// `pkg_id`'s target's link line, **in link order**: dependents before
    /// dependencies, so a traditional single-pass, left-to-right static
    /// linker (the documented behavior of GNU `ld`; macOS `ld64` is more
    /// forgiving) has already seen the consumer of a symbol before it hits
    /// the archive that defines it.
    ///
    /// Diamonds are handled for free: `Resolve::reverse_topological_order`
    /// walks the package graph, which has exactly one node per package, so
    /// a shared tail like `d` in `app -> b -> d`, `app -> c -> d` appears
    /// exactly once in the returned order, positioned after both `b` and
    /// `c`.
    ///
    /// Static vs. shared: the walk stops recursing past a `SharedLib`
    /// dependency. A shared library resolves its own transitive
    /// dependencies at *its own* link step -- their code already lives
    /// inside the produced `.so`/`.dylib` -- so nothing beyond the shared
    /// library itself needs to appear on the dependent's link line. A
    /// `StaticLib` dependency has no link step of its own (`plan.rs` only
    /// archives it); its object code only ever reaches a final binary
    /// through whoever links its archive, so its own dependencies must keep
    /// propagating outward through it.
    ///
    /// Cycles: if the package graph ever contains one, `topological_order`/
    /// `reverse_topological_order` (backed by petgraph's Kahn's-algorithm
    /// `Topo`) silently omit the nodes on the cycle instead of looping
    /// forever, which would otherwise silently drop a library from the link
    /// line. `Resolve` defines a `ResolveError::CycleDetected` variant, but
    /// nothing currently constructs it, so a cyclic graph is not actually
    /// rejected upstream today. Rejecting cycles belongs to the resolver
    /// (`src/resolver/`), which this change does not touch. As a defensive
    /// fallback -- not a real fix -- any closure member that the
    /// topological order drops is appended at the tail below rather than
    /// silently lost; a correct fix for genuine circular static-lib
    /// dependencies would wrap the cyclic group in `--start-group`/
    /// `--end-group` (the `LinkGroup::StartEndGroup` surface already models
    /// this for manifest-declared groups, but nothing wires it to the
    /// linker command yet -- also out of scope here).
    fn link_dep_order(&self, pkg_id: PackageId, target: &Target) -> Vec<PackageId> {
        use std::collections::HashSet;

        // BFS from the target's direct dependencies, only recursing further
        // through packages linked as a static library.
        let mut closure: HashSet<PackageId> = HashSet::new();
        let mut visited: HashSet<PackageId> = HashSet::new();
        let mut stack: Vec<PackageId> = self.resolve.deps(pkg_id);

        while let Some(dep_id) = stack.pop() {
            if !visited.insert(dep_id) {
                continue;
            }
            closure.insert(dep_id);

            if let Some(dep_package) = self.packages.get(&dep_id) {
                if let Ok(Some(dep_target)) = self.get_dep_target(target, dep_id, dep_package) {
                    if dep_target.kind == TargetKind::StaticLib {
                        for sub_dep in self.resolve.deps(dep_id) {
                            stack.push(sub_dep);
                        }
                    }
                }
            }
        }

        let ordered: Vec<PackageId> = self
            .resolve
            .reverse_topological_order()
            .into_iter()
            .filter(|id| closure.contains(id))
            .collect();

        // Defensive fallback for cycles (see doc comment above): don't
        // silently drop closure members that the topological order omitted.
        if ordered.len() != closure.len() {
            let mut seen: HashSet<PackageId> = ordered.iter().copied().collect();
            let mut result = ordered;
            for id in closure {
                if seen.insert(id) {
                    result.push(id);
                }
            }
            return result;
        }

        ordered
    }

    /// Compute the effective link surface for a target.
    ///
    /// Algorithm:
    /// 1. Start with target's private link surface
    /// 2. Add target's public link surface
    /// 3. For each dependency, in link order (see [`Self::link_dep_order`]):
    ///    - Check target.deps for visibility override
    ///    - If public (or not overridden), add the built library and public link surface
    pub fn resolve_link_surface(
        &self,
        pkg_id: PackageId,
        target: &Target,
        deps_dir: &std::path::Path,
    ) -> Result<EffectiveLinkSurface> {
        let mut effective = EffectiveLinkSurface::default();

        // Get package
        let _package = self
            .packages
            .get(&pkg_id)
            .ok_or_else(|| anyhow::anyhow!("package not loaded: {}", pkg_id))?;

        // Resolve the target's surface
        let own_features = self.features_for(pkg_id);
        let resolved = target.surface.resolve(self.platform, &own_features);

        // Add private
        self.add_link_requirements(&mut effective, &resolved.link_private);

        // Add public
        self.add_link_requirements(&mut effective, &resolved.link_public);

        // Add dependencies in link order: dependents before dependencies,
        // stopping at shared-lib boundaries (see `link_dep_order`).
        let deps_order = self.link_dep_order(pkg_id, target);
        for dep_id in deps_order {
            if dep_id == pkg_id {
                continue;
            }

            // Check if target.deps specifies visibility for this dependency
            let visibility = self.get_link_visibility(target, dep_id);

            // Only include if public visibility
            if visibility == Visibility::Private {
                continue;
            }

            if let Some(dep_package) = self.packages.get(&dep_id) {
                // Get the specific target if specified in target.deps
                let dep_target = self.get_dep_target(target, dep_id, dep_package)?;
                if let Some(dt) = dep_target {
                    // Add the built library
                    if dt.kind.is_linkable() {
                        let lib_dir = deps_dir
                            .join(format!("{}-{}", dep_id.name(), dep_id.version()))
                            .join("lib");

                        let lib_file = lib_dir.join(dt.output_filename(self.platform.os.as_str()));

                        // Include even if not built yet
                        effective.dep_libs.push(lib_file);
                        effective.lib_dirs.push(lib_dir);
                    }

                    // Add public link surface
                    let dep_features = self.features_for(dep_id);
                    let dep_resolved = dt.surface.resolve(self.platform, &dep_features);
                    self.add_link_requirements(&mut effective, &dep_resolved.link_public);
                }
            }
        }

        // Deduplicate (lib_dirs/ldflags/frameworks only -- dep_libs must
        // keep its computed link order, so it is never sorted here).
        effective.lib_dirs.sort();
        effective.lib_dirs.dedup();
        effective.ldflags.sort();
        effective.ldflags.dedup();
        effective.frameworks.sort();
        effective.frameworks.dedup();

        Ok(effective)
    }

    fn add_compile_requirements(
        &self,
        effective: &mut EffectiveCompileSurface,
        reqs: &CompileRequirements,
        root: &std::path::Path,
    ) {
        // Make include dirs absolute
        for dir in &reqs.include_dirs {
            let abs_dir = if dir.is_absolute() {
                dir.clone()
            } else {
                root.join(dir)
            };
            effective.include_dirs.push(abs_dir);
        }

        effective.defines.extend(reqs.defines.iter().cloned());
        effective.cflags.extend(reqs.cflags.iter().cloned());
    }

    fn add_link_requirements(&self, effective: &mut EffectiveLinkSurface, reqs: &LinkRequirements) {
        effective.libs.extend(reqs.libs.iter().cloned());
        effective.ldflags.extend(reqs.ldflags.iter().cloned());
        effective.frameworks.extend(reqs.frameworks.iter().cloned());
        effective.groups.extend(reqs.groups.iter().cloned());
    }

    /// Compute the effective compile surface with provenance tracking.
    ///
    /// Same algorithm as `resolve_compile_surface`, but tracks where each
    /// flag came from for display purposes.
    pub fn resolve_compile_surface_with_provenance(
        &self,
        pkg_id: PackageId,
        target: &Target,
    ) -> Result<EffectiveCompileSurfaceWithProvenance> {
        let mut effective = EffectiveCompileSurfaceWithProvenance::default();

        // Get package
        let package = self
            .packages
            .get(&pkg_id)
            .ok_or_else(|| anyhow::anyhow!("package not loaded: {}", pkg_id))?;

        // Resolve the target's surface
        let own_features = self.features_for(pkg_id);
        let resolved = target.surface.resolve(self.platform, &own_features);

        // Add private (only for this target's sources)
        self.add_compile_requirements_with_provenance(
            &mut effective,
            &resolved.compile_private,
            package.root(),
            pkg_id,
            SurfaceKind::CompilePrivate,
        );

        // Add feature/platform-conditional private compile requirements
        let extra = target.resolved_extra_compile(self.platform, &own_features);
        self.add_compile_requirements_with_provenance(
            &mut effective,
            &extra,
            package.root(),
            pkg_id,
            SurfaceKind::CompilePrivate,
        );

        // Add public
        self.add_compile_requirements_with_provenance(
            &mut effective,
            &resolved.compile_public,
            package.root(),
            pkg_id,
            SurfaceKind::CompilePublic,
        );

        // Add transitive public surfaces from dependencies
        let transitive_deps = self.resolve.transitive_deps(pkg_id);
        for dep_id in transitive_deps {
            if let Some(dep_package) = self.packages.get(&dep_id) {
                if let Some(dep_target) = dep_package.default_target() {
                    let dep_features = self.features_for(dep_id);
                    let dep_resolved = dep_target.surface.resolve(self.platform, &dep_features);
                    self.add_compile_requirements_with_provenance(
                        &mut effective,
                        &dep_resolved.compile_public,
                        dep_package.root(),
                        dep_id,
                        SurfaceKind::CompilePublic,
                    );
                }
            }
        }

        Ok(effective)
    }

    /// Compute the effective link surface with provenance tracking.
    ///
    /// Same algorithm as `resolve_link_surface`, but tracks where each
    /// flag came from for display purposes.
    pub fn resolve_link_surface_with_provenance(
        &self,
        pkg_id: PackageId,
        target: &Target,
        deps_dir: &std::path::Path,
    ) -> Result<EffectiveLinkSurfaceWithProvenance> {
        let mut effective = EffectiveLinkSurfaceWithProvenance::default();

        // Get package
        let _package = self
            .packages
            .get(&pkg_id)
            .ok_or_else(|| anyhow::anyhow!("package not loaded: {}", pkg_id))?;

        // Resolve the target's surface
        let own_features = self.features_for(pkg_id);
        let resolved = target.surface.resolve(self.platform, &own_features);

        // Add private
        self.add_link_requirements_with_provenance(
            &mut effective,
            &resolved.link_private,
            pkg_id,
            SurfaceKind::LinkPrivate,
        );

        // Add public
        self.add_link_requirements_with_provenance(
            &mut effective,
            &resolved.link_public,
            pkg_id,
            SurfaceKind::LinkPublic,
        );

        // Add dependencies in link order: dependents before dependencies,
        // stopping at shared-lib boundaries (see `link_dep_order`).
        let deps_order = self.link_dep_order(pkg_id, target);
        for dep_id in deps_order {
            if dep_id == pkg_id {
                continue;
            }

            // Check if target.deps specifies visibility for this dependency
            let visibility = self.get_link_visibility(target, dep_id);
            if visibility == Visibility::Private {
                continue;
            }

            if let Some(dep_package) = self.packages.get(&dep_id) {
                if let Some(dep_target) = self
                    .get_dep_target(target, dep_id, dep_package)
                    .ok()
                    .flatten()
                {
                    // Add the built library
                    if dep_target.kind.is_linkable() {
                        let lib_dir = deps_dir
                            .join(format!("{}-{}", dep_id.name(), dep_id.version()))
                            .join("lib");

                        let lib_file =
                            lib_dir.join(dep_target.output_filename(self.platform.os.as_str()));

                        // Include even if not built yet
                        effective.dep_libs.push(WithProvenance::new(
                            lib_file,
                            dep_id,
                            SurfaceKind::LinkPublic,
                        ));
                        effective.lib_dirs.push(WithProvenance::new(
                            lib_dir,
                            dep_id,
                            SurfaceKind::LinkPublic,
                        ));
                    }

                    // Add public link surface
                    let dep_features = self.features_for(dep_id);
                    let dep_resolved = dep_target.surface.resolve(self.platform, &dep_features);
                    self.add_link_requirements_with_provenance(
                        &mut effective,
                        &dep_resolved.link_public,
                        dep_id,
                        SurfaceKind::LinkPublic,
                    );
                }
            }
        }

        Ok(effective)
    }

    fn add_compile_requirements_with_provenance(
        &self,
        effective: &mut EffectiveCompileSurfaceWithProvenance,
        reqs: &CompileRequirements,
        root: &std::path::Path,
        pkg_id: PackageId,
        surface_kind: SurfaceKind,
    ) {
        // Make include dirs absolute
        for dir in &reqs.include_dirs {
            let abs_dir = if dir.is_absolute() {
                dir.clone()
            } else {
                root.join(dir)
            };
            effective
                .include_dirs
                .push(WithProvenance::new(abs_dir, pkg_id, surface_kind));
        }

        for define in &reqs.defines {
            effective
                .defines
                .push(WithProvenance::new(define.clone(), pkg_id, surface_kind));
        }

        for cflag in &reqs.cflags {
            effective
                .cflags
                .push(WithProvenance::new(cflag.clone(), pkg_id, surface_kind));
        }
    }

    fn add_link_requirements_with_provenance(
        &self,
        effective: &mut EffectiveLinkSurfaceWithProvenance,
        reqs: &LinkRequirements,
        pkg_id: PackageId,
        surface_kind: SurfaceKind,
    ) {
        for lib in &reqs.libs {
            effective
                .libs
                .push(WithProvenance::new(lib.clone(), pkg_id, surface_kind));
        }

        for ldflag in &reqs.ldflags {
            effective
                .ldflags
                .push(WithProvenance::new(ldflag.clone(), pkg_id, surface_kind));
        }

        for framework in &reqs.frameworks {
            effective
                .frameworks
                .push(WithProvenance::new(framework.clone(), pkg_id, surface_kind));
        }
    }
}

impl EffectiveCompileSurface {
    /// Convert to compiler flags.
    pub fn to_flags(&self) -> Vec<String> {
        let mut flags = Vec::new();

        for dir in &self.include_dirs {
            flags.push(format!("-I{}", dir.display()));
        }

        for define in &self.defines {
            flags.push(define.to_flag());
        }

        flags.extend(self.cflags.iter().cloned());

        flags
    }
}

impl EffectiveLinkSurface {
    /// Convert to linker flags.
    pub fn to_flags(&self) -> Vec<String> {
        let mut flags = Vec::new();

        // Library search paths
        for dir in &self.lib_dirs {
            flags.push(format!("-L{}", dir.display()));
        }

        // Built dependency libraries (full paths)
        for lib in &self.dep_libs {
            flags.push(lib.display().to_string());
        }

        // System libraries
        for lib in &self.libs {
            flags.extend(lib.to_flags());
        }

        // Frameworks
        for fw in &self.frameworks {
            flags.push("-framework".to_string());
            flags.push(fw.clone());
        }

        // Additional flags
        flags.extend(self.ldflags.iter().cloned());

        flags
    }
}

/// Whether `name` occurs as a whole identifier in `line`.
///
/// Prevents `FOO` from matching inside `FOOBAR`.
fn mentions_identifier(line: &str, name: &str) -> bool {
    let bytes = line.as_bytes();
    let mut from = 0;
    while let Some(rel) = line[from..].find(name) {
        let start = from + rel;
        let end = start + name.len();
        let before_ok = start == 0 || !is_ident_byte(bytes[start - 1]);
        let after_ok = end == bytes.len() || !is_ident_byte(bytes[end]);
        if before_ok && after_ok {
            return true;
        }
        from = start + 1;
    }
    false
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Names from `candidates` that appear in a preprocessor conditional in `text`.
///
/// Only `#if`-family lines are considered, so a define merely *used* as a value
/// in ordinary code does not trip this. Kept as a pure function over the text so
/// it is testable without touching the filesystem.
fn defines_in_preprocessor_conditionals(text: &str, candidates: &[String]) -> Vec<String> {
    let mut found = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with('#') {
            continue;
        }
        // Tolerate `#  ifdef`.
        let directive = trimmed[1..].trim_start();
        let is_conditional = directive.starts_with("ifdef")
            || directive.starts_with("ifndef")
            || directive.starts_with("if")
            || directive.starts_with("elif");
        if !is_conditional {
            continue;
        }
        for name in candidates {
            if !found.contains(name) && mentions_identifier(directive, name) {
                found.push(name.clone());
            }
        }
    }
    found.sort();
    found
}

/// Warn when a *private* define is referenced from a public header.
///
/// This is the sharpest hazard features introduced. A define declared under
/// `[targets.X.surface.compile.private]` or `[[targets.X.when]]` applies only
/// to the target's own sources. If a public header branches on it, the library
/// compiles that header with the define and every consumer compiles it without,
/// so a struct gains a field on one side only. Nothing in C detects that: the
/// build succeeds, and reads through the struct return garbage.
///
/// Warned rather than errored because a `#ifdef` in a public header can be
/// legitimate -- a consumer may be expected to set it -- so false positives are
/// possible.
fn warn_private_defines_in_public_headers(
    target: &Target,
    package_root: &Path,
    compile_private: &CompileRequirements,
    conditional_private: &CompileRequirements,
    compile_public: &CompileRequirements,
) {
    // A name that is *also* defined publicly is fine: consumers see it too.
    let public: Vec<&str> = compile_public.defines.iter().map(|d| d.name()).collect();
    let private: Vec<String> = compile_private
        .defines
        .iter()
        .chain(conditional_private.defines.iter())
        .map(|d| d.name().to_string())
        .filter(|n| !public.contains(&n.as_str()))
        .collect();

    if private.is_empty() {
        return;
    }

    // Prefer the declared public headers. Many manifests declare only public
    // include directories, though, and a consumer gets everything in those --
    // so fall back to scanning them rather than skipping the check entirely.
    let headers = if !target.public_headers.is_empty() {
        crate::util::fs::glob_files(package_root, &target.public_headers).unwrap_or_default()
    } else {
        let patterns: Vec<String> = compile_public
            .include_dirs
            .iter()
            .flat_map(|dir| {
                let base = dir.display().to_string();
                ["h", "hpp", "hh", "hxx"]
                    .iter()
                    .map(move |ext| format!("{base}/**/*.{ext}"))
                    .collect::<Vec<_>>()
            })
            .collect();
        if patterns.is_empty() {
            return;
        }
        // Relative to the package, as declared in the manifest. An absolute
        // include dir still works, since joining onto it replaces the base.
        crate::util::fs::glob_files(package_root, &patterns).unwrap_or_default()
    };

    for header in headers {
        let Ok(text) = std::fs::read_to_string(&header) else {
            continue;
        };
        for name in defines_in_preprocessor_conditionals(&text, &private) {
            let rel = header.strip_prefix(package_root).unwrap_or(&header);
            tracing::warn!(
                "target `{}`: private define `{}` is referenced by public header `{}`. \
                 Consumers compile that header without it, so any struct layout or \
                 declaration it guards will differ between this library and everything \
                 that links it -- silently. Move it to \
                 `[targets.{}.surface.compile.public]`, or to a \
                 `[[targets.{}.surface.when]]` block's `compile.public` defines if it is \
                 feature-gated.",
                target.name,
                name,
                rel.display(),
                target.name,
                target.name
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::util::context::DEFAULT_REGISTRY_URL;
    /// A dependency exposing several libraries used to contribute exactly
    /// one archive with no indication the others were dropped. openssl ships
    /// libcrypto and libssl; the symptom was a wall of undefined symbols in
    /// the consumer with nothing naming the cause.
    #[test]
    fn extra_library_targets_are_reported_not_silently_dropped() {
        use crate::core::manifest::Manifest;
        use crate::core::package::Package;
        use crate::core::source_id::SourceId;
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("multi");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("Harbour.toml"),
            r#"[package]
name = "multi"
version = "1.0.0"

[targets.crypto]
kind = "staticlib"

[targets.ssl]
kind = "staticlib"
"#,
        )
        .unwrap();
        let manifest = Manifest::load(&dir.join("Harbour.toml")).unwrap();
        let pkg = Package::with_source_id(manifest, dir.clone(), SourceId::for_path(&dir).unwrap())
            .unwrap();

        let linkable: Vec<_> = pkg
            .targets()
            .iter()
            .filter(|t| t.kind.is_linkable())
            .map(|t| t.name.to_string())
            .collect();
        assert_eq!(
            linkable.len(),
            2,
            "fixture must expose two linkable targets: {linkable:?}"
        );
        assert!(
            pkg.default_target().is_some(),
            "only one of them is ever linked, which is what the warning is for"
        );
    }

    #[test]
    fn identifier_match_is_whole_word() {
        assert!(super::mentions_identifier("ifdef FOO", "FOO"));
        assert!(super::mentions_identifier("if defined(FOO)", "FOO"));
        assert!(super::mentions_identifier("if FOO && BAR", "BAR"));
        // Must not fire on a longer identifier that merely contains the name.
        assert!(!super::mentions_identifier("ifdef FOOBAR", "FOO"));
        assert!(!super::mentions_identifier("ifdef PRE_FOO", "FOO"));
    }

    #[test]
    fn finds_private_defines_used_in_preprocessor_conditionals() {
        let header = r#"
#ifndef H
#define H
struct Obj {
    int a;
#ifdef PRIV_EXTRA
    int extra;
#endif
};
#if defined(PRIV_MODE) && PRIV_MODE > 1
int extra_fn(void);
#endif
#endif
"#;
        let names = vec![
            "PRIV_EXTRA".to_string(),
            "PRIV_MODE".to_string(),
            "PRIV_UNUSED".to_string(),
        ];
        assert_eq!(
            super::defines_in_preprocessor_conditionals(header, &names),
            vec!["PRIV_EXTRA".to_string(), "PRIV_MODE".to_string()]
        );
    }

    #[test]
    fn ignores_a_define_used_only_as_a_value() {
        // Using a define in ordinary code does not create a layout hazard --
        // only a preprocessor conditional can change what a consumer sees.
        let header = "int f(void) { return PRIV_LIMIT; }\n";
        let names = vec!["PRIV_LIMIT".to_string()];
        assert!(super::defines_in_preprocessor_conditionals(header, &names).is_empty());
    }

    #[test]
    fn tolerates_spacing_after_the_hash() {
        let header = "#  ifdef PRIV_X\nint y;\n#  endif\n";
        let names = vec!["PRIV_X".to_string()];
        assert_eq!(
            super::defines_in_preprocessor_conditionals(header, &names),
            vec!["PRIV_X".to_string()]
        );
    }

    use super::*;

    #[test]
    fn test_effective_compile_surface_to_flags() {
        let surface = EffectiveCompileSurface {
            include_dirs: vec![PathBuf::from("/usr/include"), PathBuf::from("./src")],
            defines: vec![Define::flag("DEBUG"), Define::key_value("VERSION", "1")],
            cflags: vec!["-Wall".to_string()],
        };

        let flags = surface.to_flags();
        assert!(flags.contains(&"-I/usr/include".to_string()));
        assert!(flags.contains(&"-I./src".to_string()));
        assert!(flags.contains(&"-DDEBUG".to_string()));
        assert!(flags.contains(&"-DVERSION=1".to_string()));
        assert!(flags.contains(&"-Wall".to_string()));
    }

    #[test]
    fn test_effective_link_surface_to_flags() {
        let surface = EffectiveLinkSurface {
            libs: vec![LibRef::system("pthread"), LibRef::system("m")],
            lib_dirs: vec![PathBuf::from("/usr/lib")],
            ldflags: vec!["-Wl,-rpath,/opt/lib".to_string()],
            frameworks: vec!["Security".to_string()],
            dep_libs: vec![PathBuf::from("target/deps/foo/libfoo.a")],
            groups: vec![],
        };

        let flags = surface.to_flags();
        assert!(flags.contains(&"-L/usr/lib".to_string()));
        assert!(flags.contains(&"-lpthread".to_string()));
        assert!(flags.contains(&"-lm".to_string()));
        assert!(flags.contains(&"-framework".to_string()));
        assert!(flags.contains(&"Security".to_string()));
    }

    /// Two real packages, loaded from real `Harbour.toml` manifests on
    /// disk, depend on one shared library with *disjoint* requested
    /// features (`fts5` only, `rtree` only, both with `default-features =
    /// false`). Neither dependent alone asks for both -- if
    /// `compute_feature_sets` did anything other than union per-package
    /// requests across all dependents, the shared library would come out
    /// with only one of the two, silently missing whichever capability its
    /// *other* dependent needed. That is exactly the failure mode the
    /// C-specific "one physical copy" constraint calls out: there is only
    /// ever one `sqlike` in the final link, so it must be built with
    /// `{fts5, rtree}`, not `{fts5}` xor `{rtree}`.
    #[test]
    fn compute_feature_sets_unifies_disjoint_dependent_requests() {
        use crate::core::manifest::Manifest;
        use crate::core::package::Package;
        use crate::core::source_id::SourceId;
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();

        let lib_dir = tmp.path().join("sqlike");
        std::fs::create_dir_all(&lib_dir).unwrap();
        std::fs::write(
            lib_dir.join("Harbour.toml"),
            r#"[package]
name = "sqlike"
version = "1.0.0"

[features]
fts5 = []
rtree = []

[targets.sqlike]
kind = "staticlib"
"#,
        )
        .unwrap();
        let lib_manifest = Manifest::load(&lib_dir.join("Harbour.toml")).unwrap();
        let lib_source = SourceId::for_path(&lib_dir).unwrap();
        let lib_pkg = Package::with_source_id(lib_manifest, lib_dir.clone(), lib_source).unwrap();
        let lib_id = lib_pkg.package_id();

        let make_dependent = |name: &str, feature: &str| {
            let dir = tmp.path().join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("Harbour.toml"),
                format!(
                    r#"[package]
name = "{name}"
version = "1.0.0"

[dependencies]
sqlike = {{ path = "../sqlike", features = ["{feature}"], default-features = false }}

[targets.{name}]
kind = "exe"
"#
                ),
            )
            .unwrap();
            let manifest = Manifest::load(&dir.join("Harbour.toml")).unwrap();
            let source = SourceId::for_path(&dir).unwrap();
            Package::with_source_id(manifest, dir, source).unwrap()
        };

        let app_a = make_dependent("app_a", "fts5");
        let app_b = make_dependent("app_b", "rtree");
        let (a_id, b_id) = (app_a.package_id(), app_b.package_id());

        let mut resolve = Resolve::new();
        resolve.add_package(lib_id, lib_pkg.summary(DEFAULT_REGISTRY_URL).unwrap());
        resolve.add_package(a_id, app_a.summary(DEFAULT_REGISTRY_URL).unwrap());
        resolve.add_package(b_id, app_b.summary(DEFAULT_REGISTRY_URL).unwrap());
        resolve.add_edge(a_id, lib_id);
        resolve.add_edge(b_id, lib_id);

        let mut packages = HashMap::new();
        packages.insert(lib_id, lib_pkg);
        packages.insert(a_id, app_a);
        packages.insert(b_id, app_b);

        let features = compute_feature_sets(&resolve, &packages).unwrap();
        let lib_features = &features[&lib_id];

        assert!(
            lib_features.contains("fts5"),
            "union must include app_a's request: {lib_features:?}"
        );
        assert!(
            lib_features.contains("rtree"),
            "union must include app_b's request: {lib_features:?}"
        );

        // The dependents' own feature sets are unaffected by each other --
        // unification applies to the shared dependency, not sideways
        // between siblings.
        assert!(features[&a_id].is_empty());
        assert!(features[&b_id].is_empty());
    }

    /// `default-features` unification is an OR, not an AND: a dependent
    /// that doesn't mention `default-features` at all (the common case)
    /// wants the default true, and that alone is enough to enable it for
    /// the shared dependency even though a sibling dependent explicitly
    /// opted out.
    #[test]
    fn compute_feature_sets_default_features_is_an_or_across_dependents() {
        use crate::core::manifest::Manifest;
        use crate::core::package::Package;
        use crate::core::source_id::SourceId;
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();

        let lib_dir = tmp.path().join("sqlike");
        std::fs::create_dir_all(&lib_dir).unwrap();
        std::fs::write(
            lib_dir.join("Harbour.toml"),
            r#"[package]
name = "sqlike"
version = "1.0.0"

[features]
default = ["fts5"]
fts5 = []

[targets.sqlike]
kind = "staticlib"
"#,
        )
        .unwrap();
        let lib_manifest = Manifest::load(&lib_dir.join("Harbour.toml")).unwrap();
        let lib_source = SourceId::for_path(&lib_dir).unwrap();
        let lib_pkg = Package::with_source_id(lib_manifest, lib_dir.clone(), lib_source).unwrap();
        let lib_id = lib_pkg.package_id();

        // app_a opts out of default features entirely.
        let a_dir = tmp.path().join("app_a");
        std::fs::create_dir_all(&a_dir).unwrap();
        std::fs::write(
            a_dir.join("Harbour.toml"),
            r#"[package]
name = "app_a"
version = "1.0.0"

[dependencies]
sqlike = { path = "../sqlike", default-features = false }

[targets.app_a]
kind = "exe"
"#,
        )
        .unwrap();
        let a_manifest = Manifest::load(&a_dir.join("Harbour.toml")).unwrap();
        let a_source = SourceId::for_path(&a_dir).unwrap();
        let app_a = Package::with_source_id(a_manifest, a_dir, a_source).unwrap();

        // app_b says nothing -- default-features defaults to true.
        let b_dir = tmp.path().join("app_b");
        std::fs::create_dir_all(&b_dir).unwrap();
        std::fs::write(
            b_dir.join("Harbour.toml"),
            r#"[package]
name = "app_b"
version = "1.0.0"

[dependencies]
sqlike = { path = "../sqlike" }

[targets.app_b]
kind = "exe"
"#,
        )
        .unwrap();
        let b_manifest = Manifest::load(&b_dir.join("Harbour.toml")).unwrap();
        let b_source = SourceId::for_path(&b_dir).unwrap();
        let app_b = Package::with_source_id(b_manifest, b_dir, b_source).unwrap();

        let (a_id, b_id) = (app_a.package_id(), app_b.package_id());

        let mut resolve = Resolve::new();
        resolve.add_package(lib_id, lib_pkg.summary(DEFAULT_REGISTRY_URL).unwrap());
        resolve.add_package(a_id, app_a.summary(DEFAULT_REGISTRY_URL).unwrap());
        resolve.add_package(b_id, app_b.summary(DEFAULT_REGISTRY_URL).unwrap());
        resolve.add_edge(a_id, lib_id);
        resolve.add_edge(b_id, lib_id);

        let mut packages = HashMap::new();
        packages.insert(lib_id, lib_pkg);
        packages.insert(a_id, app_a);
        packages.insert(b_id, app_b);

        let features = compute_feature_sets(&resolve, &packages).unwrap();
        assert!(
            features[&lib_id].contains("fts5"),
            "app_b's implicit default-features=true must win over app_a's opt-out: {:?}",
            features[&lib_id]
        );
    }

    /// Write a package with the given manifest body under `dir/name`,
    /// returning the loaded [`Package`]. Shared helper for the `dep/feature`
    /// propagation tests below.
    fn pkg_from_toml(
        tmp: &std::path::Path,
        name: &str,
        toml: &str,
    ) -> crate::core::package::Package {
        use crate::core::manifest::Manifest;
        use crate::core::package::Package;
        use crate::core::source_id::SourceId;

        let dir = tmp.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Harbour.toml"), toml).unwrap();
        let manifest = Manifest::load(&dir.join("Harbour.toml")).unwrap();
        let source = SourceId::for_path(&dir).unwrap();
        Package::with_source_id(manifest, dir, source).unwrap()
    }

    /// A chain three deep (`app -> outer -> inner`), where `app` only ever
    /// requests `outer`'s own feature `want`, and `outer` declares `want =
    /// ["inner/deep"]`. Proves propagation is transitive through more than
    /// one hop, not just a single dependent->dependency step.
    #[test]
    fn dep_feature_propagates_transitively_through_a_chain() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();

        let inner = pkg_from_toml(
            tmp.path(),
            "inner",
            r#"[package]
name = "inner"
version = "1.0.0"

[features]
deep = []

[targets.inner]
kind = "staticlib"
"#,
        );
        let outer = pkg_from_toml(
            tmp.path(),
            "outer",
            r#"[package]
name = "outer"
version = "1.0.0"

[features]
want = ["inner/deep"]

[dependencies]
inner = { path = "../inner", default-features = false }

[targets.outer]
kind = "staticlib"
"#,
        );
        let app = pkg_from_toml(
            tmp.path(),
            "app",
            r#"[package]
name = "app"
version = "1.0.0"

[dependencies]
outer = { path = "../outer", features = ["want"], default-features = false }

[targets.app]
kind = "exe"
"#,
        );

        let (inner_id, outer_id, app_id) =
            (inner.package_id(), outer.package_id(), app.package_id());

        let mut resolve = Resolve::new();
        resolve.add_package(inner_id, inner.summary(DEFAULT_REGISTRY_URL).unwrap());
        resolve.add_package(outer_id, outer.summary(DEFAULT_REGISTRY_URL).unwrap());
        resolve.add_package(app_id, app.summary(DEFAULT_REGISTRY_URL).unwrap());
        resolve.add_edge(app_id, outer_id);
        resolve.add_edge(outer_id, inner_id);

        let mut packages = HashMap::new();
        packages.insert(inner_id, inner);
        packages.insert(outer_id, outer);
        packages.insert(app_id, app);

        let features = compute_feature_sets(&resolve, &packages).unwrap();
        assert!(
            features[&outer_id].contains("want"),
            "app requested `outer/want`: {:?}",
            features[&outer_id]
        );
        assert!(
            features[&inner_id].contains("deep"),
            "outer's `want` feature must propagate `inner/deep` down the chain: {:?}",
            features[&inner_id]
        );
    }

    /// A diamond (`app -> b -> d`, `app -> c -> d`) where `b` and `c` each
    /// request a *different* feature of the shared dependency `d` via
    /// `dep/feature`. `d`'s final feature set must be the union of both --
    /// the same unification guarantee that already holds for plain
    /// `features = [...]` dependency entries, now also proven for features
    /// that arrive via propagation rather than a direct manifest entry.
    #[test]
    fn dep_feature_union_holds_across_a_diamond() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();

        let d = pkg_from_toml(
            tmp.path(),
            "d",
            r#"[package]
name = "d"
version = "1.0.0"

[features]
x = []
y = []

[targets.d]
kind = "staticlib"
"#,
        );
        let b = pkg_from_toml(
            tmp.path(),
            "b",
            r#"[package]
name = "b"
version = "1.0.0"

[features]
want_x = ["d/x"]

[dependencies]
d = { path = "../d", default-features = false }

[targets.b]
kind = "staticlib"
"#,
        );
        let c = pkg_from_toml(
            tmp.path(),
            "c",
            r#"[package]
name = "c"
version = "1.0.0"

[features]
want_y = ["d/y"]

[dependencies]
d = { path = "../d", default-features = false }

[targets.c]
kind = "staticlib"
"#,
        );
        let app = pkg_from_toml(
            tmp.path(),
            "app",
            r#"[package]
name = "app"
version = "1.0.0"

[dependencies]
b = { path = "../b", features = ["want_x"], default-features = false }
c = { path = "../c", features = ["want_y"], default-features = false }

[targets.app]
kind = "exe"
"#,
        );

        let (d_id, b_id, c_id, app_id) = (
            d.package_id(),
            b.package_id(),
            c.package_id(),
            app.package_id(),
        );

        let mut resolve = Resolve::new();
        resolve.add_package(d_id, d.summary(DEFAULT_REGISTRY_URL).unwrap());
        resolve.add_package(b_id, b.summary(DEFAULT_REGISTRY_URL).unwrap());
        resolve.add_package(c_id, c.summary(DEFAULT_REGISTRY_URL).unwrap());
        resolve.add_package(app_id, app.summary(DEFAULT_REGISTRY_URL).unwrap());
        resolve.add_edge(app_id, b_id);
        resolve.add_edge(app_id, c_id);
        resolve.add_edge(b_id, d_id);
        resolve.add_edge(c_id, d_id);

        let mut packages = HashMap::new();
        packages.insert(d_id, d);
        packages.insert(b_id, b);
        packages.insert(c_id, c);
        packages.insert(app_id, app);

        let features = compute_feature_sets(&resolve, &packages).unwrap();
        assert!(
            features[&d_id].contains("x"),
            "b's `want_x` must propagate `d/x`: {:?}",
            features[&d_id]
        );
        assert!(
            features[&d_id].contains("y"),
            "c's `want_y` must propagate `d/y`: {:?}",
            features[&d_id]
        );
    }

    /// `outer/nope` where `outer` is not a dependency of the declaring
    /// package at all must be a clear error naming both the dependent and
    /// the (non-)dependency name.
    #[test]
    fn dep_feature_naming_a_non_dependency_is_an_error() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();

        // `want` must actually be enabled (here: via `default`) to trigger
        // its `dep/feature` entry at all.
        let app = pkg_from_toml(
            tmp.path(),
            "app2",
            r#"[package]
name = "app2"
version = "1.0.0"

[features]
default = ["want"]
want = ["ghost/nope"]

[targets.app2]
kind = "exe"
"#,
        );
        let app_id = app.package_id();
        let mut resolve = Resolve::new();
        resolve.add_package(app_id, app.summary(DEFAULT_REGISTRY_URL).unwrap());
        let mut packages = HashMap::new();
        packages.insert(app_id, app);

        let err = compute_feature_sets(&resolve, &packages).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("app2") && msg.contains("ghost"),
            "error must name both the declaring package and the missing dependency: {msg}"
        );
    }

    /// `dep/feature` naming a real dependency but a feature that dependency
    /// does not declare must error, and the error must attribute the
    /// problem to the *declaring* package, not bury it as if `inner`'s own
    /// manifest were broken.
    #[test]
    fn dep_feature_naming_an_undeclared_feature_is_an_error() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();

        let inner = pkg_from_toml(
            tmp.path(),
            "inner",
            r#"[package]
name = "inner"
version = "1.0.0"

[features]
real = []

[targets.inner]
kind = "staticlib"
"#,
        );
        let outer = pkg_from_toml(
            tmp.path(),
            "outer",
            r#"[package]
name = "outer"
version = "1.0.0"

[features]
default = ["want"]
want = ["inner/nonexistent"]

[dependencies]
inner = { path = "../inner", default-features = false }

[targets.outer]
kind = "staticlib"
"#,
        );

        let (inner_id, outer_id) = (inner.package_id(), outer.package_id());
        let mut resolve = Resolve::new();
        resolve.add_package(inner_id, inner.summary(DEFAULT_REGISTRY_URL).unwrap());
        resolve.add_package(outer_id, outer.summary(DEFAULT_REGISTRY_URL).unwrap());
        resolve.add_edge(outer_id, inner_id);

        let mut packages = HashMap::new();
        packages.insert(inner_id, inner);
        packages.insert(outer_id, outer);

        let err = compute_feature_sets(&resolve, &packages).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("outer") && msg.contains("nonexistent") && msg.contains("inner"),
            "error must attribute the problem to `outer` (the declaring package), \
             not `inner`: {msg}"
        );
    }
}
