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
            Some(enables) => queue.extend(enables.iter().cloned()),
            None => {
                bail!("unknown feature `{name}`: not declared in this package's [features] section")
            }
        }
    }

    Ok(enabled)
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
}
