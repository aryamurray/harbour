//! Tier-1 index generation from tier-2 shims.
//!
//! Per the design doc, a git registry's tier-1 index is **committed**:
//! generated from the shims by CI and checked for freshness there, not
//! computed on the fly by the client. This module is that generator.
//!
//! It is not wired to a CLI command - publish tooling is explicitly out of
//! scope for this change - but it exists as a plain function because tests
//! need a real tier-1 index to resolve against, and "regenerate from shims"
//! is also exactly what a future `harbour registry generate-index` (or a CI
//! freshness check) would call.
//!
//! Deriving a version's dependencies requires fetching its source once (to
//! read its own `Harbour.toml`) - the same fetch `RegistrySource::query`
//! used to perform per candidate version, per resolution, before this
//! change. Doing it here instead means the cost is paid once at publish
//! time, not once per resolution: exactly the "worthwhile on its own
//! merits" fix the design doc calls for.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Result};

use super::index::{
    self, IndexDependency, IndexDependencyKind, IndexRecord, CURRENT_FORMAT_VERSION,
};
use super::shim::Shim;
use super::transport::{GitTransport, RegistryTransport};
use crate::core::workspace::find_manifest;
use crate::core::Manifest;

/// Regenerate the entire tier-1 index for a registry checkout at
/// `registry_root`, from the tier-2 shims already committed there.
///
/// `cache_dir` is used as scratch space to fetch each shim's source once
/// (mirroring `RegistrySource`'s own source cache); it need not be shared
/// with any running `RegistrySource`.
///
/// Existing tier-1 files are fully overwritten with a freshly computed,
/// deterministically ordered snapshot - this is the "recompute and diff"
/// shape a CI freshness check wants, not an incremental append.
pub fn generate_index(registry_root: &Path, cache_dir: &Path) -> Result<()> {
    let index_root = registry_root.join("index");
    if !index_root.exists() {
        return Ok(());
    }

    let mut transport = GitTransport::from_local_path(registry_root, cache_dir)?;
    let registry_url = transport.registry_url().clone();

    let shim_relative_paths = find_shim_files(&index_root)?;

    let mut by_package: BTreeMap<String, Vec<IndexRecord>> = BTreeMap::new();

    for relative in shim_relative_paths {
        let shim_file = index_root.join(&relative);
        let shim = Shim::load(&shim_file)?;

        // `relative` is relative to `index/`; the transport addresses
        // paths relative to the registry root.
        let shim_relative_to_root = format!("index/{relative}");
        let source_dir = transport.fetch_artifact(&shim, &shim_relative_to_root)?;
        let deps = collect_index_deps(&shim, &source_dir, &registry_url)?;

        let checksum = shim
            .git_source()
            .and_then(|git| git.checksum.clone())
            .or_else(|| shim.tarball_source().map(|tarball| tarball.sha256.clone()));

        let record = IndexRecord {
            format_version: CURRENT_FORMAT_VERSION,
            name: shim.package.name.clone(),
            version: shim.package.version.clone(),
            yanked: false,
            deps,
            checksum,
            shim: relative,
        };

        by_package
            .entry(shim.package.name.clone())
            .or_default()
            .push(record);
    }

    for (name, mut records) in by_package {
        records.sort_by(|a, b| {
            let va: semver::Version = a.version.parse().expect("shim validated its own version");
            let vb: semver::Version = b.version.parse().expect("shim validated its own version");
            va.cmp(&vb)
        });

        let relative_idx = index::index_path(&name)?;
        let idx_file = index_root.join(relative_idx);
        index::write_index_file(&idx_file, &records)?;
    }

    Ok(())
}

/// Walk `index_root` for tier-2 shim files (`<letter>/<name>/<version>.toml`),
/// returning their paths relative to `index_root` with `/`-separated
/// components regardless of platform.
fn find_shim_files(index_root: &Path) -> Result<Vec<String>> {
    let mut paths = Vec::new();

    for entry in walkdir::WalkDir::new(index_root)
        .into_iter()
        .filter_map(|entry| entry.ok())
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.extension().is_none_or(|ext| ext != "toml") {
            continue;
        }

        let relative = path
            .strip_prefix(index_root)
            .expect("walked entry is under index_root")
            .to_string_lossy()
            .replace('\\', "/");
        paths.push(relative);
    }

    paths.sort();
    Ok(paths)
}

/// Derive tier-1 dependency records for one shim's version by loading the
/// manifest of its (already-fetched) source.
///
/// A registry package may depend only on registry packages, following
/// Cargo's rule that a published crate cannot carry `path` or `git`
/// dependencies. A published package has to be resolvable and reproducible
/// from the index alone, and neither of those sources is: a `path` is
/// meaningless once the package leaves its author's disk, and a `git`
/// dependency is uncurated and not guaranteed to exist tomorrow, even
/// pinned by SHA. Both are errors here rather than warnings, because a
/// skipped dependency yields an index that resolves cleanly and then fails
/// at build time -- the wrong direction for that failure.
///
/// vcpkg dependencies are deliberately *not* an exception to that rule.
/// They are never resolved against the registry -- the environment
/// satisfies them -- so they are not a tier-1 concern at all and belong
/// with the rest of the build recipe in tier 2. They are skipped here
/// silently rather than warned about.
fn collect_index_deps(
    shim: &Shim,
    source_dir: &Path,
    registry_url: &url::Url,
) -> Result<Vec<IndexDependency>> {
    let manifest_path = match find_manifest(source_dir) {
        Ok(path) => path,
        // Bootstrap packages with only a `surface_override` and no
        // Harbour.toml genuinely have no dependencies to record.
        Err(_) => return Ok(Vec::new()),
    };

    let manifest = Manifest::load(&manifest_path)?;

    let mut deps = Vec::new();
    for (dep_name, spec) in &manifest.dependencies {
        let dep = spec.to_dependency(dep_name, source_dir)?;

        // Satisfied by the environment, never by the solver, so it is not a
        // tier-1 concern. Tier 2 carries it along with the build recipe.
        if dep.source_id().is_vcpkg() {
            continue;
        }

        if dep.is_path() {
            bail!(
                "registry package '{}' {} cannot depend on '{}' by path: a path is \
                 meaningless once the package leaves the machine that published it.\n\
                 hint: publish '{}' to a registry and depend on it by version",
                shim.package.name,
                shim.package.version,
                dep_name,
                dep_name
            );
        }

        if dep.is_git() {
            bail!(
                "registry package '{}' {} cannot depend on '{}' via git: a published \
                 package must be resolvable from the index alone, and a git dependency \
                 is uncurated and not guaranteed to remain available.\n\
                 hint: publish '{}' to a registry and depend on it by version",
                shim.package.name,
                shim.package.version,
                dep_name,
                dep_name
            );
        }

        if !dep.is_registry() {
            bail!(
                "registry package '{}' {} depends on '{}' via an unsupported source \
                 kind; a registry package may depend only on registry packages",
                shim.package.name,
                shim.package.version,
                dep_name
            );
        }

        let dep_registry = if dep.source_id().url() == registry_url {
            None
        } else {
            Some(dep.source_id().url().to_string())
        };

        deps.push(IndexDependency {
            name: dep_name.clone(),
            version_req: dep.version_req().to_string(),
            optional: dep.is_optional(),
            default_features: dep.uses_default_features(),
            kind: IndexDependencyKind::Normal,
            registry: dep_registry,
        });
    }

    Ok(deps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::fixtures::local_registry;
    use tempfile::TempDir;

    fn commit_all(dir: &Path, message: &str) -> String {
        let repo = git2::Repository::open(dir)
            .or_else(|_| git2::Repository::init(dir))
            .unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig =
            git2::Signature::now("Harbour Test Fixture", "harbour-tests@example.invalid").unwrap();
        let parents: Vec<git2::Commit> = match repo.head().and_then(|h| h.peel_to_commit()) {
            Ok(commit) => vec![commit],
            Err(_) => vec![],
        };
        let parent_refs: Vec<&git2::Commit> = parents.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parent_refs)
            .unwrap()
            .to_string()
    }

    fn write_lib_package(dir: &Path, name: &str, deps_toml: &str) {
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("Harbour.toml"),
            format!(
                r#"[package]
name = "{name}"
version = "0.1.0"

[dependencies]
{deps_toml}

[targets.{name}]
kind = "staticlib"
sources = ["src/lib.c"]
"#
            ),
        )
        .unwrap();
        std::fs::write(dir.join("src/lib.c"), "int x;\n").unwrap();
    }

    fn add_shim(registry_dir: &Path, name: &str, version: &str, url: &str, rev: &str) {
        let first = name.chars().next().unwrap();
        let shim_dir = registry_dir
            .join("index")
            .join(first.to_string())
            .join(name);
        std::fs::create_dir_all(&shim_dir).unwrap();
        std::fs::write(
            shim_dir.join(format!("{version}.toml")),
            format!(
                r#"[package]
name = "{name}"
version = "{version}"

[source.git]
url = "{url}"
rev = "{rev}"
"#
            ),
        )
        .unwrap();
    }

    #[test]
    fn generates_index_with_dependency_from_source_manifest() {
        let tmp = TempDir::new().unwrap();

        let registry_dir = tmp.path().join("registry");
        local_registry::init(&registry_dir).unwrap();
        let registry_url = local_registry::file_url(&registry_dir);

        let liba_src = tmp.path().join("src-liba");
        write_lib_package(&liba_src, "liba", "");
        let liba_rev = commit_all(&liba_src, "init");
        let liba_url = local_registry::file_url(&liba_src);
        add_shim(&registry_dir, "liba", "0.1.0", &liba_url, &liba_rev);

        let libb_src = tmp.path().join("src-libb");
        write_lib_package(
            &libb_src,
            "libb",
            &format!(r#"liba = {{ version = "0.1.0", registry = "{registry_url}" }}"#),
        );
        let libb_rev = commit_all(&libb_src, "init");
        let libb_url = local_registry::file_url(&libb_src);
        add_shim(&registry_dir, "libb", "0.1.0", &libb_url, &libb_rev);

        commit_all(&registry_dir, "add shims");

        generate_index(&registry_dir, &tmp.path().join("cache")).unwrap();

        let liba_idx = registry_dir.join("index/l/liba.idx");
        let liba_records = index::read_index_file(&liba_idx).unwrap().unwrap();
        assert_eq!(liba_records.len(), 1);
        assert_eq!(liba_records[0].name, "liba");
        assert!(liba_records[0].deps.is_empty());

        let libb_idx = registry_dir.join("index/l/libb.idx");
        let libb_records = index::read_index_file(&libb_idx).unwrap().unwrap();
        assert_eq!(libb_records.len(), 1);
        assert_eq!(libb_records[0].deps.len(), 1);
        assert_eq!(libb_records[0].deps[0].name, "liba");
        assert_eq!(libb_records[0].deps[0].version_req, "^0.1.0");
    }

    /// Cargo's rule: a published package must be resolvable from the index
    /// alone, so `path` and `git` dependencies are rejected at publish time
    /// rather than skipped. Skipping produced an index that resolved cleanly
    /// and then failed at build time.
    #[test]
    fn rejects_a_path_dependency_in_a_registry_package() {
        let tmp = TempDir::new().unwrap();
        let registry_dir = tmp.path().join("registry");
        local_registry::init(&registry_dir).unwrap();

        // The dependency target has to exist for the manifest to load.
        write_lib_package(&tmp.path().join("src-sibling"), "sibling", "");

        let src = tmp.path().join("src-libp");
        write_lib_package(&src, "libp", r#"sibling = { path = "../src-sibling" }"#);
        let rev = commit_all(&src, "init");
        add_shim(
            &registry_dir,
            "libp",
            "0.1.0",
            &local_registry::file_url(&src),
            &rev,
        );
        commit_all(&registry_dir, "add shim");

        let err = generate_index(&registry_dir, &tmp.path().join("cache")).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("cannot depend on 'sibling' by path"), "{msg}");
        // The message has to say what to do instead, or it is a dead end.
        assert!(msg.contains("publish"), "{msg}");
    }

    #[test]
    fn rejects_a_git_dependency_in_a_registry_package() {
        let tmp = TempDir::new().unwrap();
        let registry_dir = tmp.path().join("registry");
        local_registry::init(&registry_dir).unwrap();

        let dep_src = tmp.path().join("src-gitdep");
        write_lib_package(&dep_src, "gitdep", "");
        let dep_rev = commit_all(&dep_src, "init");
        let dep_url = local_registry::file_url(&dep_src);

        let src = tmp.path().join("src-libg");
        write_lib_package(
            &src,
            "libg",
            &format!(r#"gitdep = {{ git = "{dep_url}", rev = "{dep_rev}" }}"#),
        );
        let rev = commit_all(&src, "init");
        add_shim(
            &registry_dir,
            "libg",
            "0.1.0",
            &local_registry::file_url(&src),
            &rev,
        );
        commit_all(&registry_dir, "add shim");

        let err = generate_index(&registry_dir, &tmp.path().join("cache")).unwrap_err();
        let msg = format!("{err:#}");
        // Rejected even though the shim format pins git by full SHA: the
        // objection is availability and curation, not immutability.
        assert!(msg.contains("cannot depend on 'gitdep' via git"), "{msg}");
        assert!(msg.contains("publish"), "{msg}");
    }

    /// vcpkg is deliberately not an exception to the registry-only rule. Such
    /// a dependency is satisfied by the environment and never by the solver,
    /// so it is not a tier-1 concern at all -- it belongs with the build
    /// recipe in tier 2, and is omitted here without complaint.
    #[test]
    fn omits_a_vcpkg_dependency_without_failing() {
        let tmp = TempDir::new().unwrap();
        let registry_dir = tmp.path().join("registry");
        local_registry::init(&registry_dir).unwrap();

        let src = tmp.path().join("src-libv");
        write_lib_package(&src, "libv", r#"zlib = { vcpkg = true }"#);
        let rev = commit_all(&src, "init");
        add_shim(
            &registry_dir,
            "libv",
            "0.1.0",
            &local_registry::file_url(&src),
            &rev,
        );
        commit_all(&registry_dir, "add shim");

        generate_index(&registry_dir, &tmp.path().join("cache"))
            .expect("a vcpkg dependency must not fail index generation");

        let records = index::read_index_file(&registry_dir.join("index/l/libv.idx"))
            .unwrap()
            .unwrap();
        assert_eq!(records.len(), 1);
        assert!(
            records[0].deps.is_empty(),
            "a vcpkg dependency is not a tier-1 dependency: {:?}",
            records[0].deps
        );
    }
}
