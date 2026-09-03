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

use anyhow::Result;

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
/// Dependencies on a non-registry source (git, path, vcpkg) cannot be
/// represented in the tier-1 format described by the design doc - it only
/// carries `name`/`version_req`/`optional`/`default_features`/`kind` - so
/// they are skipped with a warning rather than silently resolved via a
/// fallback fetch. See this module's report for the disclosed limitation.
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

        if !dep.is_registry() {
            tracing::warn!(
                "package '{}' {} depends on '{}' via a non-registry source; \
                 this cannot be represented in the tier-1 index and will not be \
                 visible to metadata-only resolution",
                shim.package.name,
                shim.package.version,
                dep_name
            );
            continue;
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
}
