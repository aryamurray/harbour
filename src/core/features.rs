//! Feature declaration and selection for native packages.
//!
//! A native package can declare a `[features]` section listing named
//! toggles, exactly like Cargo's `[features]` table: each entry maps a
//! feature name to the list of *other* feature names it additionally
//! enables. A `default` entry, if present, lists the features enabled when
//! a dependent doesn't say otherwise (`default-features = false` opts out).
//!
//! ```toml
//! [features]
//! default = ["fts5"]
//! fts5 = []
//! json1 = []
//! full = ["fts5", "json1"]
//! ```
//!
//! This mirrors Cargo deliberately: it is a TOML shape package authors
//! already know, `Dependency::features()` / `Dependency::uses_default_features()`
//! (see `core::dependency`) already speak this exact vocabulary, and no
//! second convention is needed to explain "what does a feature turn on".
//!
//! What is different from Cargo, and is the entire reason this module
//! exists as its own thing rather than a thin selection layer: **a C
//! dependency graph can only build one copy of a library**, so a package's
//! *enabled* feature set is not "what its one dependent asked for" but the
//! union of what every dependent in the build asked for (see
//! `builder::surface_resolver::compute_feature_sets`). This module only
//! deals with the per-package half of that: given a package's declared
//! `[features]` table and the union of requested feature names, compute the
//! transitive closure.
//!
//! ## `dep/feature` entries
//!
//! An `enables` list may also contain Cargo's `dep/feature` syntax --
//! `want = ["inner/deep"]` -- to request a feature on one of the package's
//! *own* dependencies rather than another feature of its own. This module
//! treats any entry containing a `/` as such a reference: [`resolve_features`]
//! never treats it as one of *this* package's own feature names (so it is
//! not looked up in `defs` and cannot itself trigger "unknown feature"), and
//! [`dependency_feature_requests`] walks the same `enables` lists over the
//! now-resolved `enabled` set to collect `dep_name -> {feature, ...}` so the
//! caller (which has the dependency graph and can validate `dep_name` is an
//! actual dependency, and that it declares `feature`) can propagate it.
//!
//! Splitting is on the *first* `/` only, matching Cargo. This means a
//! feature name that itself contains a `/` can never be named from an
//! `enables` list without being misread as a dependency reference -- the
//! same ambiguity Cargo accepts for the same syntax. This module does not
//! reject such a key in `defs` (a package could still request it directly
//! via `requested`, bypassing `enables` entirely), but authors should avoid
//! slashes in feature names.
//!
//! ## Optional dependencies
//!
//! A dependency marked `optional = true` is not resolved, fetched, built or
//! linked unless some enabled feature activates it. Harbour follows Cargo's
//! two spellings exactly, because they are the ones package authors already
//! know:
//!
//! - **Implicit feature.** An optional dependency `ssl` defines a feature
//!   named `ssl`, so a dependent writing `features = ["ssl"]` (or the
//!   package's own `default = ["ssl"]`) activates it. As in Cargo, the
//!   implicit feature is *suppressed* if any feature's `enables` list names
//!   the dependency explicitly as `dep:ssl` -- that is how an author hides
//!   the dependency behind a differently-named feature.
//! - **`dep:name`.** An entry `dep:ssl` in a feature's `enables` list
//!   activates the optional dependency `ssl` without defining a feature
//!   called `ssl`. `dep:` may only name a dependency that is actually
//!   declared `optional = true` -- naming a required dependency, or a
//!   dependency that does not exist, is an error rather than a no-op.
//! - **`optdep/feature`.** A `dep/feature` entry whose left-hand side is an
//!   optional dependency activates it *and* requests that feature, again as
//!   Cargo does. Cargo's weak form `optdep?/feature` ("request the feature
//!   but do not activate the dependency") is **not** implemented and is a
//!   hard error, rather than being silently read as the strong form.
//!
//! Activation is collected by [`dependency_activations`]. Because a C
//! dependency graph links one copy of each library, activation is unified
//! across the whole graph exactly as feature sets are: an optional
//! dependency activated by *any* package in the build is in the build for
//! everyone.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use anyhow::{bail, Result};

/// A package's declared `[features]` table: feature name -> other feature
/// names it enables. `BTreeMap`/`BTreeSet` (rather than `HashMap`/`HashSet`)
/// throughout this module so that feature sets have a deterministic
/// iteration order -- they flow into fingerprint/flag hashing, where
/// nondeterministic ordering would mean two identical builds could hash
/// differently.
pub type FeatureMap = BTreeMap<String, Vec<String>>;

/// A resolved, transitively-closed set of enabled feature names.
pub type FeatureSet = BTreeSet<String>;

/// The prefix that names an optional dependency directly from a feature's
/// `enables` list, as Cargo spells it: `dep:ssl`.
const DEP_PREFIX: &str = "dep:";

/// The set of optional dependency names that some feature's `enables` list
/// names explicitly with `dep:`.
///
/// Cargo's rule, adopted verbatim: a `dep:name` entry anywhere in the
/// `[features]` table *suppresses* the implicit feature that the optional
/// dependency `name` would otherwise define. That is the only way an author
/// can expose an optional dependency under a different feature name without
/// also exposing the dependency's own name as a feature.
fn explicitly_named_deps(defs: &FeatureMap) -> BTreeSet<String> {
    defs.values()
        .flatten()
        .filter_map(|e| e.strip_prefix(DEP_PREFIX))
        .map(str::to_string)
        .collect()
}

/// The implicit feature names an optional dependency set contributes.
///
/// An optional dependency contributes a feature of its own name unless
/// either the `[features]` table already declares that name (an explicit
/// declaration wins, and is then responsible for activating the dependency
/// with `dep:`) or some feature names it with `dep:`.
pub fn implicit_features(defs: &FeatureMap, optional_deps: &BTreeSet<String>) -> BTreeSet<String> {
    let explicit = explicitly_named_deps(defs);
    optional_deps
        .iter()
        .filter(|d| !explicit.contains(*d) && !defs.contains_key(*d))
        .cloned()
        .collect()
}

/// How an entry in a feature's `enables` list is to be read.
enum Entry<'a> {
    /// Another feature of this same package.
    OwnFeature(&'a str),
    /// `dep:name` -- activate the optional dependency `name`.
    ActivateDep(&'a str),
    /// `name/feature` -- request `feature` of the dependency `name`.
    DepFeature(&'a str, &'a str),
}

/// Classify one `enables` entry, rejecting the syntax Harbour does not
/// implement rather than reading it as something close enough.
fn classify<'a>(entry: &'a str, optional_deps: &BTreeSet<String>) -> Result<Entry<'a>> {
    if let Some(dep) = entry.strip_prefix(DEP_PREFIX) {
        if dep.contains('/') {
            let (name, feature) = dep.split_once('/').expect("just checked for `/`");
            bail!(
                "invalid feature entry `{entry}`: `dep:` activates a whole optional \
                 dependency and cannot be combined with `/`\n\
                 hint: write `{name}/{feature}` to activate `{name}` and enable its \
                 `{feature}` at the same time"
            );
        }
        if dep.is_empty() {
            bail!("invalid feature entry `{entry}`: `dep:` must be followed by a dependency name");
        }
        if !optional_deps.contains(dep) {
            bail!(
                "feature entry `{entry}` names `{dep}` with `dep:`, but `{dep}` is not an \
                 optional dependency of this package\n\
                 hint: `dep:` is only for dependencies declared `optional = true`; a \
                 required dependency is always in the build and needs no activation"
            );
        }
        return Ok(Entry::ActivateDep(dep));
    }
    if let Some((dep, feature)) = entry.split_once('/') {
        // Cargo's weak form `dep?/feature` means "request the feature, but
        // do not activate the dependency". Reading it as the strong form
        // would activate a dependency the author asked *not* to activate --
        // a successful build of the wrong graph -- so it is refused instead.
        if let Some(dep) = dep.strip_suffix('?') {
            bail!(
                "feature entry `{entry}` uses weak dependency syntax (`{dep}?/{feature}`), \
                 which is not implemented\n\
                 hint: write `{dep}/{feature}` to activate `{dep}` and enable its \
                 `{feature}`, or put the entry behind a feature that is only enabled \
                 when `{dep}` is wanted"
            );
        }
        return Ok(Entry::DepFeature(dep, feature));
    }
    Ok(Entry::OwnFeature(entry))
}

/// Resolve a package's effective feature set.
///
/// `defs` is the package's own `[features]` declaration. `optional_deps` is
/// the set of names in the package's `[dependencies]` marked
/// `optional = true`; each of those contributes an implicit feature of the
/// same name unless suppressed (see [`implicit_features`]). `requested` is
/// the union of feature names explicitly asked for by dependents (see
/// `compute_feature_sets`). `default_features` is whether `default` should
/// be seeded (true unless every dependent set `default-features = false`).
///
/// Unknown features -- a name in `requested`, or reachable transitively via
/// `enables`, that the package's `[features]` table does not declare and
/// that is not an optional dependency's implicit feature -- are a hard error
/// rather than a silent no-op. For a C dependency this is not a cosmetic
/// choice: a dependent asking for `fts5` and silently getting a sqlite build
/// without FTS5 is a missing-symbol link failure or worse (a caller assuming
/// a capability that silently isn't there), and both are strictly worse than
/// failing fast at resolve time with a clear message.
pub fn resolve_features(
    defs: &FeatureMap,
    optional_deps: &BTreeSet<String>,
    requested: &[String],
    default_features: bool,
) -> Result<FeatureSet> {
    let implicit = implicit_features(defs, optional_deps);
    let mut queue: VecDeque<String> = VecDeque::new();
    let mut enabled: FeatureSet = BTreeSet::new();

    // Seed with `default` only if the package actually declares one --
    // packages with no [features] section at all (the overwhelming common
    // case) must not error out just because default_features defaults to
    // true.
    if default_features && defs.contains_key("default") {
        queue.push_back("default".to_string());
    }
    for f in requested {
        queue.push_back(f.clone());
    }

    while let Some(name) = queue.pop_front() {
        if !enabled.insert(name.clone()) {
            continue; // already processed (also breaks cycles in `enables`)
        }
        match defs.get(&name) {
            Some(enables) => {
                for entry in enables {
                    // `dep:` and `dep/feature` entries name a dependency,
                    // not a feature of this package -- they never enter
                    // this package's own closure or its "unknown feature"
                    // checking. See `dependency_feature_requests` and
                    // `dependency_activations`, which walk the same lists
                    // once `enabled` is final.
                    if let Entry::OwnFeature(f) = classify(entry, optional_deps)? {
                        queue.push_back(f.to_string());
                    }
                }
            }
            // An optional dependency's implicit feature has no `enables`
            // list of its own; enabling it means activating the dependency,
            // which `dependency_activations` reads off `enabled`.
            None if implicit.contains(&name) => {}
            None => {
                bail!("unknown feature `{name}`: not declared in this package's [features] section")
            }
        }
    }

    Ok(enabled)
}

/// Collect the optional dependencies an already-resolved `enabled` feature
/// set activates.
///
/// Three things activate an optional dependency, matching Cargo:
///
/// 1. its implicit feature being enabled (`enabled` contains its name);
/// 2. a `dep:name` entry in an enabled feature's `enables` list;
/// 3. a `name/feature` entry in an enabled feature's `enables` list, where
///    `name` is an optional dependency.
///
/// Only names drawn from `optional_deps` are returned: a required dependency
/// is always in the graph and has nothing to activate.
///
/// `defs` is assumed to have already been through [`resolve_features`], so
/// the syntax rejections in `classify` have already fired; a malformed entry
/// is skipped here rather than reported twice.
pub fn dependency_activations(
    defs: &FeatureMap,
    optional_deps: &BTreeSet<String>,
    enabled: &FeatureSet,
) -> BTreeSet<String> {
    let mut active: BTreeSet<String> = enabled
        .iter()
        .filter(|f| optional_deps.contains(*f))
        .cloned()
        .collect();

    for name in enabled {
        let Some(enables) = defs.get(name) else {
            continue;
        };
        for entry in enables {
            match classify(entry, optional_deps) {
                Ok(Entry::ActivateDep(dep)) => {
                    active.insert(dep.to_string());
                }
                Ok(Entry::DepFeature(dep, _)) if optional_deps.contains(dep) => {
                    active.insert(dep.to_string());
                }
                _ => {}
            }
        }
    }

    active
}

/// Collect `dep/feature` requests reachable from an already-resolved
/// `enabled` feature set.
///
/// For every feature in `enabled` that `defs` declares, any entry in its
/// `enables` list containing a `/` is split on the *first* `/` into a
/// dependency name and a feature name on that dependency, and folded into
/// the returned map (`dep_name -> {feature, ...}`, unioned across every
/// enabled feature that mentions it).
///
/// This is a separate pass over the same lists [`resolve_features`] already
/// walked, rather than something folded into that function's return value,
/// because validating a `dep/feature` entry (is `dep_name` actually a
/// dependency? does it declare `feature`?) needs the dependency graph and
/// the dependency's own `[features]` table, neither of which this
/// dependency-graph-agnostic module has. The caller (see
/// `builder::surface_resolver::compute_feature_sets`) does that validation
/// and attributes any error to the package that wrote the `dep/feature`
/// entry, not to the dependency.
pub fn dependency_feature_requests(
    defs: &FeatureMap,
    optional_deps: &BTreeSet<String>,
    enabled: &FeatureSet,
) -> BTreeMap<String, BTreeSet<String>> {
    let mut requests: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for name in enabled {
        let Some(enables) = defs.get(name) else {
            continue;
        };
        for entry in enables {
            // Classified by the same function `resolve_features` used, so
            // that "what counts as a `dep/feature` entry" has exactly one
            // definition. Errors have already been reported by
            // `resolve_features`; nothing here re-reports them.
            if let Ok(Entry::DepFeature(dep_name, feature)) = classify(entry, optional_deps) {
                requests
                    .entry(dep_name.to_string())
                    .or_default()
                    .insert(feature.to_string());
            }
        }
    }
    requests
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defs(pairs: &[(&str, &[&str])]) -> FeatureMap {
        pairs
            .iter()
            .map(|(k, v)| {
                (
                    k.to_string(),
                    v.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                )
            })
            .collect()
    }

    /// A package with no optional dependencies at all -- the common case,
    /// and the one every pre-existing test in this module is about.
    fn no_opt() -> BTreeSet<String> {
        BTreeSet::new()
    }

    #[test]
    fn no_features_declared_is_fine_with_defaults() {
        let set = resolve_features(&FeatureMap::new(), &no_opt(), &[], true).unwrap();
        assert!(set.is_empty());
    }

    #[test]
    fn default_feature_seeds_and_expands() {
        let d = defs(&[("default", &["fts5"]), ("fts5", &[])]);
        let set = resolve_features(&d, &no_opt(), &[], true).unwrap();
        assert!(set.contains("default"));
        assert!(set.contains("fts5"));
    }

    #[test]
    fn default_features_false_skips_default() {
        let d = defs(&[("default", &["fts5"]), ("fts5", &[])]);
        let set = resolve_features(&d, &no_opt(), &[], false).unwrap();
        assert!(!set.contains("fts5"));
        assert!(set.is_empty());
    }

    #[test]
    fn explicit_feature_expands_transitively() {
        let d = defs(&[("full", &["fts5", "json1"]), ("fts5", &[]), ("json1", &[])]);
        let set = resolve_features(&d, &no_opt(), &["full".to_string()], false).unwrap();
        assert!(set.contains("full"));
        assert!(set.contains("fts5"));
        assert!(set.contains("json1"));
    }

    #[test]
    fn unknown_feature_is_an_error() {
        let d = defs(&[("fts5", &[])]);
        let err = resolve_features(&d, &no_opt(), &["json1".to_string()], false).unwrap_err();
        assert!(err.to_string().contains("json1"));
    }

    #[test]
    fn cycle_in_enables_does_not_infinite_loop() {
        let d = defs(&[("a", &["b"]), ("b", &["a"])]);
        let set = resolve_features(&d, &no_opt(), &["a".to_string()], false).unwrap();
        assert!(set.contains("a"));
        assert!(set.contains("b"));
    }

    #[test]
    fn union_of_requests_is_additive() {
        let d = defs(&[("fts5", &[]), ("json1", &[])]);
        let a = resolve_features(&d, &no_opt(), &["fts5".to_string()], false).unwrap();
        let b = resolve_features(&d, &no_opt(), &["json1".to_string()], false).unwrap();
        let union: FeatureSet = a.union(&b).cloned().collect();
        assert!(union.contains("fts5"));
        assert!(union.contains("json1"));
    }

    // -- `dep/feature` --------------------------------------------------

    #[test]
    fn dep_feature_entry_does_not_become_an_own_feature_or_error() {
        // "inner/deep" must not be looked up in `defs` as an own feature
        // name (it would error "unknown feature `inner/deep`" if it did).
        let d = defs(&[("want", &["inner/deep"])]);
        let set = resolve_features(&d, &no_opt(), &["want".to_string()], false).unwrap();
        assert!(set.contains("want"));
        assert!(!set.contains("inner/deep"));
        assert!(!set.contains("deep"));
        assert!(!set.contains("inner"));
    }

    #[test]
    fn dependency_feature_requests_collects_dep_feature_entries() {
        let d = defs(&[("want", &["inner/deep"])]);
        let set = resolve_features(&d, &no_opt(), &["want".to_string()], false).unwrap();
        let reqs = dependency_feature_requests(&d, &no_opt(), &set);
        assert_eq!(reqs.len(), 1);
        assert!(reqs["inner"].contains("deep"));
    }

    #[test]
    fn dependency_feature_requests_unions_across_enabled_features() {
        // Two different own features each request something of the same
        // dependency -- both must show up in the union for that dependency.
        let d = defs(&[("a", &["inner/x"]), ("b", &["inner/y"])]);
        let set =
            resolve_features(&d, &no_opt(), &["a".to_string(), "b".to_string()], false).unwrap();
        let reqs = dependency_feature_requests(&d, &no_opt(), &set);
        assert_eq!(reqs.len(), 1);
        assert!(reqs["inner"].contains("x"));
        assert!(reqs["inner"].contains("y"));
    }

    #[test]
    fn dependency_feature_requests_only_considers_enabled_features() {
        // "unused" is declared but never enabled, so its dep/feature entry
        // must not leak into the result.
        let d = defs(&[("used", &["inner/x"]), ("unused", &["inner/y"])]);
        let set = resolve_features(&d, &no_opt(), &["used".to_string()], false).unwrap();
        let reqs = dependency_feature_requests(&d, &no_opt(), &set);
        assert_eq!(reqs["inner"].len(), 1);
        assert!(reqs["inner"].contains("x"));
        assert!(!reqs["inner"].contains("y"));
    }

    #[test]
    fn dependency_feature_requests_splits_on_first_slash_only() {
        // A feature name that itself contains a `/` on the far side of the
        // dependency name is preserved whole as the requested feature.
        let d = defs(&[("want", &["inner/deep/nested"])]);
        let set = resolve_features(&d, &no_opt(), &["want".to_string()], false).unwrap();
        let reqs = dependency_feature_requests(&d, &no_opt(), &set);
        assert!(reqs["inner"].contains("deep/nested"));
    }

    #[test]
    fn dep_feature_transitively_expanded_own_features_still_collected() {
        // The dep/feature entry sits behind a chain of the package's own
        // feature `enables` -- it must still be found once the closure
        // reaches the feature that declares it.
        let d = defs(&[("full", &["mid"]), ("mid", &["inner/deep"])]);
        let set = resolve_features(&d, &no_opt(), &["full".to_string()], false).unwrap();
        assert!(set.contains("mid"));
        let reqs = dependency_feature_requests(&d, &no_opt(), &set);
        assert!(reqs["inner"].contains("deep"));
    }
}

#[cfg(test)]
mod optional_dependency_tests {
    use super::*;

    fn defs(pairs: &[(&str, &[&str])]) -> FeatureMap {
        pairs
            .iter()
            .map(|(k, v)| {
                (
                    k.to_string(),
                    v.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                )
            })
            .collect()
    }

    fn opt(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    // -- the implicit feature -------------------------------------------

    #[test]
    fn optional_dep_defines_an_implicit_feature_of_its_own_name() {
        // Without the implicit feature this is "unknown feature `ssl`".
        let d = FeatureMap::new();
        let set = resolve_features(&d, &opt(&["ssl"]), &["ssl".to_string()], false).unwrap();
        assert!(set.contains("ssl"));
        assert_eq!(
            dependency_activations(&d, &opt(&["ssl"]), &set),
            opt(&["ssl"])
        );
    }

    #[test]
    fn an_optional_dep_nobody_asked_for_is_not_activated() {
        let d = FeatureMap::new();
        let set = resolve_features(&d, &opt(&["ssl"]), &[], true).unwrap();
        assert!(set.is_empty(), "no feature was requested: {set:?}");
        assert!(dependency_activations(&d, &opt(&["ssl"]), &set).is_empty());
    }

    #[test]
    fn default_can_activate_an_optional_dep_through_its_implicit_feature() {
        let d = defs(&[("default", &["ssl"])]);
        let set = resolve_features(&d, &opt(&["ssl"]), &[], true).unwrap();
        assert_eq!(
            dependency_activations(&d, &opt(&["ssl"]), &set),
            opt(&["ssl"])
        );
        // ... and `default-features = false` switches it back off.
        let off = resolve_features(&d, &opt(&["ssl"]), &[], false).unwrap();
        assert!(dependency_activations(&d, &opt(&["ssl"]), &off).is_empty());
    }

    #[test]
    fn a_feature_reaching_the_implicit_feature_transitively_activates_it() {
        let d = defs(&[("tls", &["ssl"])]);
        let set = resolve_features(&d, &opt(&["ssl"]), &["tls".to_string()], false).unwrap();
        assert!(set.contains("ssl"));
        assert_eq!(
            dependency_activations(&d, &opt(&["ssl"]), &set),
            opt(&["ssl"])
        );
    }

    #[test]
    fn a_required_dependency_does_not_define_a_feature() {
        // `ssl` is a *required* dependency here (not in optional_deps), so
        // naming it as a feature is still an unknown feature.
        let err = resolve_features(&FeatureMap::new(), &opt(&[]), &["ssl".to_string()], false)
            .unwrap_err();
        assert!(err.to_string().contains("unknown feature `ssl`"), "{err}");
    }

    // -- `dep:name` ------------------------------------------------------

    #[test]
    fn dep_prefix_activates_without_defining_a_feature_of_that_name() {
        let d = defs(&[("tls", &["dep:ssl"])]);
        let set = resolve_features(&d, &opt(&["ssl"]), &["tls".to_string()], false).unwrap();
        assert!(set.contains("tls"));
        assert!(
            !set.contains("ssl"),
            "`dep:ssl` must not put `ssl` in the feature set: {set:?}"
        );
        assert_eq!(
            dependency_activations(&d, &opt(&["ssl"]), &set),
            opt(&["ssl"])
        );
    }

    #[test]
    fn naming_a_dep_with_dep_prefix_suppresses_its_implicit_feature() {
        // Cargo's rule: once some feature says `dep:ssl`, the dependency's
        // own name is no longer a feature a dependent can request.
        let d = defs(&[("tls", &["dep:ssl"])]);
        let err = resolve_features(&d, &opt(&["ssl"]), &["ssl".to_string()], false).unwrap_err();
        assert!(err.to_string().contains("unknown feature `ssl`"), "{err}");
    }

    #[test]
    fn dep_prefix_naming_a_required_dependency_is_an_error() {
        let d = defs(&[("tls", &["dep:ssl"])]);
        let err = resolve_features(&d, &opt(&[]), &["tls".to_string()], false).unwrap_err();
        assert!(
            err.to_string().contains("not an optional dependency"),
            "{err}"
        );
    }

    #[test]
    fn dep_prefix_with_no_name_is_an_error() {
        let d = defs(&[("tls", &["dep:"])]);
        let err = resolve_features(&d, &opt(&["ssl"]), &["tls".to_string()], false).unwrap_err();
        assert!(
            err.to_string()
                .contains("must be followed by a dependency name"),
            "{err}"
        );
    }

    #[test]
    fn dep_prefix_combined_with_a_slash_is_an_error() {
        let d = defs(&[("tls", &["dep:ssl/asm"])]);
        let err = resolve_features(&d, &opt(&["ssl"]), &["tls".to_string()], false).unwrap_err();
        assert!(
            err.to_string().contains("cannot be combined with `/`"),
            "{err}"
        );
        assert!(err.to_string().contains("ssl/asm"), "{err}");
    }

    // -- `optdep/feature` ------------------------------------------------

    #[test]
    fn dep_feature_on_an_optional_dep_activates_it_and_requests_the_feature() {
        let d = defs(&[("tls", &["ssl/asm"])]);
        let set = resolve_features(&d, &opt(&["ssl"]), &["tls".to_string()], false).unwrap();
        assert_eq!(
            dependency_activations(&d, &opt(&["ssl"]), &set),
            opt(&["ssl"])
        );
        // The feature request itself still flows through the existing path.
        let reqs = dependency_feature_requests(&d, &opt(&["ssl"]), &set);
        assert!(reqs["ssl"].contains("asm"));
    }

    #[test]
    fn dep_feature_on_a_required_dep_activates_nothing() {
        let d = defs(&[("tls", &["ssl/asm"])]);
        let set = resolve_features(&d, &opt(&[]), &["tls".to_string()], false).unwrap();
        assert!(dependency_activations(&d, &opt(&[]), &set).is_empty());
    }

    #[test]
    fn weak_dependency_syntax_is_refused_rather_than_read_as_the_strong_form() {
        let d = defs(&[("tls", &["ssl?/asm"])]);
        let err = resolve_features(&d, &opt(&["ssl"]), &["tls".to_string()], false).unwrap_err();
        assert!(err.to_string().contains("weak dependency syntax"), "{err}");
    }

    // -- activation is not triggered by a feature nobody enabled ----------

    #[test]
    fn activation_only_considers_enabled_features() {
        let d = defs(&[("used", &["dep:a"]), ("unused", &["dep:b"])]);
        let optional = opt(&["a", "b"]);
        let set = resolve_features(&d, &optional, &["used".to_string()], false).unwrap();
        assert_eq!(dependency_activations(&d, &optional, &set), opt(&["a"]));
    }

    #[test]
    fn activation_unions_across_several_enabled_features() {
        let d = defs(&[("x", &["dep:a"]), ("y", &["b"])]);
        let optional = opt(&["a", "b"]);
        let set =
            resolve_features(&d, &optional, &["x".to_string(), "y".to_string()], false).unwrap();
        assert_eq!(
            dependency_activations(&d, &optional, &set),
            opt(&["a", "b"])
        );
    }

    #[test]
    fn an_explicit_feature_named_after_an_optional_dep_still_activates_it() {
        // The author declared `ssl` themselves, so their `enables` list is
        // what runs; the implicit-feature rule still activates the
        // dependency, because `ssl` is in the enabled set and is an
        // optional dependency name.
        let d = defs(&[("ssl", &["fast"]), ("fast", &[])]);
        let optional = opt(&["ssl"]);
        let set = resolve_features(&d, &optional, &["ssl".to_string()], false).unwrap();
        assert!(set.contains("fast"));
        assert_eq!(dependency_activations(&d, &optional, &set), opt(&["ssl"]));
    }
}
