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

/// Resolve a package's effective feature set.
///
/// `defs` is the package's own `[features]` declaration. `requested` is the
/// union of feature names explicitly asked for by dependents (see
/// `compute_feature_sets`). `default_features` is whether `default` should
/// be seeded (true unless every dependent set `default-features = false`).
///
/// Unknown features -- a name in `requested`, or reachable transitively via
/// `enables`, that the package's `[features]` table does not declare -- are
/// a hard error rather than a silent no-op. For a C dependency this is not
/// a cosmetic choice: a dependent asking for `fts5` and silently getting a
/// sqlite build without FTS5 is a missing-symbol link failure or worse (a
/// caller assuming a capability that silently isn't there), and both are
/// strictly worse than failing fast at resolve time with a clear message.
pub fn resolve_features(
    defs: &FeatureMap,
    requested: &[String],
    default_features: bool,
) -> Result<FeatureSet> {
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
                // `dep/feature` entries name a feature on a dependency, not
                // a feature of this package -- they never enter this
                // package's own closure or its "unknown feature" checking.
                // See `dependency_feature_requests`, which walks the same
                // lists to collect them once `enabled` is final.
                queue.extend(enables.iter().filter(|e| !e.contains('/')).cloned());
            }
            None => {
                bail!("unknown feature `{name}`: not declared in this package's [features] section")
            }
        }
    }

    Ok(enabled)
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
    enabled: &FeatureSet,
) -> BTreeMap<String, BTreeSet<String>> {
    let mut requests: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for name in enabled {
        let Some(enables) = defs.get(name) else {
            continue;
        };
        for entry in enables {
            if let Some((dep_name, feature)) = entry.split_once('/') {
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

    #[test]
    fn no_features_declared_is_fine_with_defaults() {
        let set = resolve_features(&FeatureMap::new(), &[], true).unwrap();
        assert!(set.is_empty());
    }

    #[test]
    fn default_feature_seeds_and_expands() {
        let d = defs(&[("default", &["fts5"]), ("fts5", &[])]);
        let set = resolve_features(&d, &[], true).unwrap();
        assert!(set.contains("default"));
        assert!(set.contains("fts5"));
    }

    #[test]
    fn default_features_false_skips_default() {
        let d = defs(&[("default", &["fts5"]), ("fts5", &[])]);
        let set = resolve_features(&d, &[], false).unwrap();
        assert!(!set.contains("fts5"));
        assert!(set.is_empty());
    }

    #[test]
    fn explicit_feature_expands_transitively() {
        let d = defs(&[("full", &["fts5", "json1"]), ("fts5", &[]), ("json1", &[])]);
        let set = resolve_features(&d, &["full".to_string()], false).unwrap();
        assert!(set.contains("full"));
        assert!(set.contains("fts5"));
        assert!(set.contains("json1"));
    }

    #[test]
    fn unknown_feature_is_an_error() {
        let d = defs(&[("fts5", &[])]);
        let err = resolve_features(&d, &["json1".to_string()], false).unwrap_err();
        assert!(err.to_string().contains("json1"));
    }

    #[test]
    fn cycle_in_enables_does_not_infinite_loop() {
        let d = defs(&[("a", &["b"]), ("b", &["a"])]);
        let set = resolve_features(&d, &["a".to_string()], false).unwrap();
        assert!(set.contains("a"));
        assert!(set.contains("b"));
    }

    #[test]
    fn union_of_requests_is_additive() {
        let d = defs(&[("fts5", &[]), ("json1", &[])]);
        let a = resolve_features(&d, &["fts5".to_string()], false).unwrap();
        let b = resolve_features(&d, &["json1".to_string()], false).unwrap();
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
        let set = resolve_features(&d, &["want".to_string()], false).unwrap();
        assert!(set.contains("want"));
        assert!(!set.contains("inner/deep"));
        assert!(!set.contains("deep"));
        assert!(!set.contains("inner"));
    }

    #[test]
    fn dependency_feature_requests_collects_dep_feature_entries() {
        let d = defs(&[("want", &["inner/deep"])]);
        let set = resolve_features(&d, &["want".to_string()], false).unwrap();
        let reqs = dependency_feature_requests(&d, &set);
        assert_eq!(reqs.len(), 1);
        assert!(reqs["inner"].contains("deep"));
    }

    #[test]
    fn dependency_feature_requests_unions_across_enabled_features() {
        // Two different own features each request something of the same
        // dependency -- both must show up in the union for that dependency.
        let d = defs(&[("a", &["inner/x"]), ("b", &["inner/y"])]);
        let set = resolve_features(&d, &["a".to_string(), "b".to_string()], false).unwrap();
        let reqs = dependency_feature_requests(&d, &set);
        assert_eq!(reqs.len(), 1);
        assert!(reqs["inner"].contains("x"));
        assert!(reqs["inner"].contains("y"));
    }

    #[test]
    fn dependency_feature_requests_only_considers_enabled_features() {
        // "unused" is declared but never enabled, so its dep/feature entry
        // must not leak into the result.
        let d = defs(&[("used", &["inner/x"]), ("unused", &["inner/y"])]);
        let set = resolve_features(&d, &["used".to_string()], false).unwrap();
        let reqs = dependency_feature_requests(&d, &set);
        assert_eq!(reqs["inner"].len(), 1);
        assert!(reqs["inner"].contains("x"));
        assert!(!reqs["inner"].contains("y"));
    }

    #[test]
    fn dependency_feature_requests_splits_on_first_slash_only() {
        // A feature name that itself contains a `/` on the far side of the
        // dependency name is preserved whole as the requested feature.
        let d = defs(&[("want", &["inner/deep/nested"])]);
        let set = resolve_features(&d, &["want".to_string()], false).unwrap();
        let reqs = dependency_feature_requests(&d, &set);
        assert!(reqs["inner"].contains("deep/nested"));
    }

    #[test]
    fn dep_feature_transitively_expanded_own_features_still_collected() {
        // The dep/feature entry sits behind a chain of the package's own
        // feature `enables` -- it must still be found once the closure
        // reaches the feature that declares it.
        let d = defs(&[("full", &["mid"]), ("mid", &["inner/deep"])]);
        let set = resolve_features(&d, &["full".to_string()], false).unwrap();
        assert!(set.contains("mid"));
        let reqs = dependency_feature_requests(&d, &set);
        assert!(reqs["inner"].contains("deep"));
    }
}
