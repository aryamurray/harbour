//! Workspace resolution operations.

use std::collections::HashSet;

use anyhow::{bail, Result};

use crate::core::dependency::{resolve_dependency, warn_workspace_dep_matches_member, Dependency};
use crate::core::Workspace;
use crate::ops::lockfile::{
    load_lockfile, save_workspace_lockfile, workspace_lockfile_needs_update,
};
use crate::resolver::{HarbourResolver, Resolve};
use crate::sources::SourceCache;

/// Options for workspace resolution.
#[derive(Debug, Clone, Default)]
pub struct ResolveOptions {
    /// Require lockfile to be up-to-date (error if resolution would change it)
    pub locked: bool,
}

/// Resolve the workspace dependencies.
///
/// Uses content-based freshness detection to determine if re-resolution is needed.
/// If the lockfile exists and the workspace hasn't changed, use the lockfile.
/// Otherwise, perform fresh resolution.
pub fn resolve_workspace(ws: &Workspace, source_cache: &mut SourceCache) -> Result<Resolve> {
    resolve_workspace_with_opts(ws, source_cache, &ResolveOptions::default())
}

/// Resolve the workspace dependencies with options.
///
/// If `opts.locked` is true, errors if the lockfile would change.
pub fn resolve_workspace_with_opts(
    ws: &Workspace,
    source_cache: &mut SourceCache,
    opts: &ResolveOptions,
) -> Result<Resolve> {
    let lockfile_path = ws.lockfile_path();

    // In locked mode, the lockfile must exist and be fresh
    if opts.locked {
        if !lockfile_path.exists() {
            bail!(
                "lockfile not found; run `harbour build` first to generate it, \
                 or remove --locked to allow resolution"
            );
        }

        if workspace_lockfile_needs_update(ws)? {
            bail!(
                "lockfile would change; run `harbour update` to update it, \
                 or remove --locked to allow resolution"
            );
        }

        // Lockfile exists and is fresh - just load it
        if let Some(resolve) = load_lockfile(&lockfile_path)? {
            tracing::info!("Using existing lockfile (--locked mode)");
            return Ok(resolve);
        } else {
            bail!("lockfile exists but could not be loaded; run `harbour update` to regenerate it");
        }
    }

    // Check if lockfile needs update using workspace-aware content hash
    if !workspace_lockfile_needs_update(ws)? {
        // Lockfile is fresh, try to load it
        if let Some(resolve) = load_lockfile(&lockfile_path)? {
            tracing::info!("Using existing lockfile (workspace unchanged)");
            return Ok(resolve);
        }
    }

    // Lockfile doesn't exist, is stale, or couldn't be loaded - resolve fresh
    if lockfile_path.exists() {
        tracing::info!("Workspace changed, re-resolving dependencies");
    } else {
        tracing::info!("No lockfile found, resolving dependencies");
    }

    resolve_fresh(ws, source_cache, true)
}

/// Perform fresh dependency resolution for all workspace members.
///
/// If `save_lockfile` is true, saves the lockfile after resolution.
pub fn resolve_fresh(
    ws: &Workspace,
    source_cache: &mut SourceCache,
    save_lockfile: bool,
) -> Result<Resolve> {
    // Warn if workspace dependencies match member names
    if let Some(ws_deps) = ws.workspace_dependencies() {
        warn_workspace_dep_matches_member(ws_deps, &ws.member_paths());
    }

    // Collect the *direct* dependencies of every workspace member, resolved
    // with full workspace context (local-first matching against sibling
    // members, and inheritance from `[workspace.dependencies]`). This
    // context only exists here, at the workspace level - a dependency
    // fetched from its own source later on has no workspace of its own to
    // consult (see `HarbourResolver`'s doc comment), so these direct edges
    // are seeded explicitly rather than left to be discovered lazily.
    //
    // Everything beyond this direct set - a git or registry dependency's
    // *own* dependencies, transitively, and (as of this change) path
    // dependencies-of-dependencies too - is discovered lazily by
    // `HarbourResolver` itself, as PubGrub's solver actually asks about it.
    // It is deliberately not walked eagerly here: doing that for git and
    // registry sources would mean cloning repositories the solver may
    // never end up choosing, which a local path read (the only kind of
    // walk that used to happen here) never had to worry about.
    let workspace_deps = ws.workspace_dependencies();
    let member_paths = ws.member_paths();
    // Copied up front: the loops below hold a mutable borrow of the cache.
    let default_registry = source_cache.default_registry().to_string();
    let mut all_deps: Vec<Dependency> = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new(); // (name, source_id)

    for member in ws.members() {
        let manifest = member.package.manifest();
        let manifest_dir = member.package.root();

        for (name, spec) in &manifest.dependencies {
            let dep = resolve_dependency(
                name,
                spec,
                workspace_deps,
                &member_paths,
                manifest_dir,
                &default_registry,
            )?;

            let key = (dep.name().to_string(), dep.source_id().to_string());
            if seen.insert(key) {
                all_deps.push(dep);
            }
        }
    }

    // Ensure all sources for the direct dependencies are ready, and query
    // them up front so the workspace-aware resolution above takes effect.
    source_cache.ensure_ready(&all_deps)?;

    let mut seeds: Vec<(Dependency, Vec<crate::core::Summary>)> = Vec::new();
    for dep in &all_deps {
        let found = source_cache.query(dep)?;
        seeds.push((dep.clone(), found));
    }

    // Use first member as root for resolver (will be improved when resolver supports multiple roots)
    let root_package = ws.root_package();
    let root_summary = root_package.summary(&default_registry)?;
    let mut resolver = HarbourResolver::new(root_summary.clone(), source_cache);
    for (dep, found) in seeds {
        resolver.seed(&dep, found);
    }

    // Resolve - anything beyond the seeded direct dependencies is fetched
    // lazily from here on, inside `resolver.resolve()`.
    let resolve = resolver.resolve()?;

    // Save lockfile with workspace hash (unless in dry-run mode)
    if save_lockfile {
        save_workspace_lockfile(&ws.lockfile_path(), &resolve, ws)?;
    }

    Ok(resolve)
}

/// Update the lockfile by re-resolving dependencies.
///
/// If `dry_run` is true, performs resolution but does not save the lockfile.
pub fn update_resolve(
    ws: &Workspace,
    source_cache: &mut SourceCache,
    dry_run: bool,
) -> Result<Resolve> {
    tracing::info!("Updating dependencies");
    resolve_fresh(ws, source_cache, !dry_run)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::GlobalContext;
    use tempfile::TempDir;

    fn create_test_workspace(dir: &std::path::Path) {
        std::fs::write(
            dir.join("Harbour.toml"),
            r#"
[package]
name = "test"
version = "1.0.0"

[targets.test]
kind = "exe"
sources = ["src/**/*.c"]
"#,
        )
        .unwrap();

        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/main.c"), "int main() { return 0; }").unwrap();
    }

    #[test]
    fn test_resolve_workspace() {
        let tmp = TempDir::new().unwrap();
        create_test_workspace(tmp.path());

        let ctx = GlobalContext::with_cwd(tmp.path().to_path_buf()).unwrap();
        let ws = Workspace::new(&tmp.path().join("Harbour.toml"), &ctx).unwrap();

        let mut cache = SourceCache::new(tmp.path().join("cache"));
        let resolve = resolve_workspace(&ws, &mut cache).unwrap();

        assert_eq!(resolve.len(), 1);
    }

    /// Write a `staticlib` package manifest with an optional `[dependencies]`
    /// block (already formatted, e.g. `"foo = { path = \"../foo\" }"`).
    fn write_lib_package(dir: &std::path::Path, name: &str, deps_toml: &str) {
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("Harbour.toml"),
            format!(
                r#"
[package]
name = "{name}"
version = "0.1.0"

[dependencies]
{deps_toml}

[targets.{name}]
kind = "staticlib"
sources = ["src/**/*.c"]
"#,
            ),
        )
        .unwrap();
        std::fs::write(
            dir.join("src/lib.c"),
            format!("void {name}_init(void) {{}}"),
        )
        .unwrap();
    }

    /// Write an `exe` package manifest with an optional `[dependencies]`
    /// block.
    fn write_exe_package(dir: &std::path::Path, name: &str, deps_toml: &str) {
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("Harbour.toml"),
            format!(
                r#"
[package]
name = "{name}"
version = "0.1.0"

[dependencies]
{deps_toml}

[targets.{name}]
kind = "exe"
sources = ["src/**/*.c"]
"#,
            ),
        )
        .unwrap();
        std::fs::write(dir.join("src/main.c"), "int main(void) { return 0; }").unwrap();
    }

    /// A path dependency's own path dependencies must be registered as
    /// available to the resolver, recursively - this is the core bug: only
    /// direct path dependencies of the *root* manifest used to be visible.
    ///
    /// Graph: app -> libb -> liba, with `app` never mentioning `liba`.
    #[test]
    fn test_transitive_path_dependency_resolves() {
        let tmp = TempDir::new().unwrap();

        let liba_dir = tmp.path().join("liba");
        write_lib_package(&liba_dir, "liba", "");

        let libb_dir = tmp.path().join("libb");
        write_lib_package(&libb_dir, "libb", r#"liba = { path = "../liba" }"#);

        let app_dir = tmp.path().join("app");
        write_exe_package(&app_dir, "app", r#"libb = { path = "../libb" }"#);

        let ctx = GlobalContext::with_cwd(app_dir.clone()).unwrap();
        let ws = Workspace::new(&app_dir.join("Harbour.toml"), &ctx).unwrap();
        let mut cache = SourceCache::new(tmp.path().join("cache"));

        let resolve = resolve_fresh(&ws, &mut cache, false).unwrap();

        // All three packages must be present, and the edges must chain
        // app -> libb -> liba (not just app -> libb).
        assert!(resolve.contains_name("app"));
        assert!(resolve.contains_name("libb"));
        assert!(
            resolve.contains_name("liba"),
            "liba should be transitively resolved even though app's manifest never mentions it"
        );

        let app_id = resolve.get_package_by_name("app".into()).unwrap();
        let libb_id = resolve.get_package_by_name("libb".into()).unwrap();
        let liba_id = resolve.get_package_by_name("liba".into()).unwrap();

        assert_eq!(resolve.deps(app_id), vec![libb_id]);
        assert_eq!(resolve.deps(libb_id), vec![liba_id]);
    }

    /// A path dependency's relative path must be resolved relative to the
    /// manifest that declares it, not relative to the workspace root.
    ///
    /// `libc` lives one directory deeper than `libd`, so `../../libd` in
    /// `libc`'s manifest only resolves correctly if it is anchored at
    /// `libc`'s own directory. If it were (incorrectly) anchored at the
    /// workspace root (`app`'s directory, a sibling of `nested/`), the path
    /// would walk out of the temp directory entirely and fail to load.
    #[test]
    fn test_transitive_path_dependency_relative_to_declaring_manifest() {
        let tmp = TempDir::new().unwrap();

        let libd_dir = tmp.path().join("libd");
        write_lib_package(&libd_dir, "libd", "");

        let libc_dir = tmp.path().join("nested").join("libc");
        write_lib_package(&libc_dir, "libc", r#"libd = { path = "../../libd" }"#);

        let app_dir = tmp.path().join("app");
        write_exe_package(&app_dir, "app", r#"libc = { path = "../nested/libc" }"#);

        let ctx = GlobalContext::with_cwd(app_dir.clone()).unwrap();
        let ws = Workspace::new(&app_dir.join("Harbour.toml"), &ctx).unwrap();
        let mut cache = SourceCache::new(tmp.path().join("cache"));

        let resolve = resolve_fresh(&ws, &mut cache, false).unwrap();

        assert!(resolve.contains_name("libc"));
        assert!(resolve.contains_name("libd"));

        let libc_id = resolve.get_package_by_name("libc".into()).unwrap();
        let libd_id = resolve.get_package_by_name("libd".into()).unwrap();
        assert_eq!(resolve.deps(libc_id), vec![libd_id]);
    }

    /// A diamond (two packages depending on the same third package) must be
    /// walked once, deduplicated by canonical `SourceId`, and must not
    /// produce two copies of the shared package.
    #[test]
    fn test_transitive_path_dependency_diamond_dedup() {
        let tmp = TempDir::new().unwrap();

        let libshared_dir = tmp.path().join("libshared");
        write_lib_package(&libshared_dir, "libshared", "");

        let libb_dir = tmp.path().join("libb");
        write_lib_package(
            &libb_dir,
            "libb",
            r#"libshared = { path = "../libshared" }"#,
        );

        let libc_dir = tmp.path().join("libc");
        write_lib_package(
            &libc_dir,
            "libc",
            r#"libshared = { path = "../libshared" }"#,
        );

        let app_dir = tmp.path().join("app");
        write_exe_package(
            &app_dir,
            "app",
            "libb = { path = \"../libb\" }\nlibc = { path = \"../libc\" }",
        );

        let ctx = GlobalContext::with_cwd(app_dir.clone()).unwrap();
        let ws = Workspace::new(&app_dir.join("Harbour.toml"), &ctx).unwrap();
        let mut cache = SourceCache::new(tmp.path().join("cache"));

        let resolve = resolve_fresh(&ws, &mut cache, false).unwrap();

        // Exactly one `libshared` package in the whole resolve - the MVP
        // invariant that C linking depends on (one version per package name).
        let shared_pkgs = resolve.get_packages_by_name("libshared".into());
        assert_eq!(
            shared_pkgs.len(),
            1,
            "diamond dependency must collapse to a single package, found {:?}",
            shared_pkgs
        );
    }

    /// A cycle in path dependencies (`a` -> `b` -> `a`) must be reported as
    /// a clear error, not hang, panic, or stack-overflow.
    #[test]
    fn test_transitive_path_dependency_cycle_errors() {
        let tmp = TempDir::new().unwrap();

        let cyca_dir = tmp.path().join("cyca");
        write_lib_package(&cyca_dir, "cyca", r#"cycb = { path = "../cycb" }"#);

        let cycb_dir = tmp.path().join("cycb");
        write_lib_package(&cycb_dir, "cycb", r#"cyca = { path = "../cyca" }"#);

        let app_dir = tmp.path().join("app");
        write_exe_package(&app_dir, "app", r#"cyca = { path = "../cyca" }"#);

        let ctx = GlobalContext::with_cwd(app_dir.clone()).unwrap();
        let ws = Workspace::new(&app_dir.join("Harbour.toml"), &ctx).unwrap();
        let mut cache = SourceCache::new(tmp.path().join("cache"));

        let err = resolve_fresh(&ws, &mut cache, false).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("cycle detected in dependency graph"),
            "unexpected error message: {msg}"
        );
    }

    /// A path dependency's package root, as actually loaded off disk by
    /// [`SourceCache`], must always be the *canonical* path - never
    /// whatever uncanonicalized spelling a manifest's `path = "../dep"`
    /// happened to join into.
    ///
    /// This matters because [`SourceId`] deliberately keeps `original_path`
    /// (the spelling `.path()` returns) out of its identity - see that
    /// field's doc comment - so two different callers who reach the same
    /// on-disk directory through differently-spelled paths still get one,
    /// equal `SourceId`. But `SourceId` is *interned*: whichever caller
    /// constructs a given identity *first* (in a given process) wins the
    /// spelling every later `.path()` call sees, since later calls just
    /// return the already-interned entry. A fresh resolve (which joins a
    /// manifest's relative `path` without canonicalizing, via
    /// `resolve_dependency`) and a lockfile-decoded resolve (whose
    /// `SourceId::parse` only ever sees the canonical URL a lockfile
    /// stores) can therefore end up with the very same `SourceId` holding
    /// either spelling, depending on which one ran first *in that
    /// process* - i.e. depending on whether a lockfile already existed.
    ///
    /// If `SourceCache` used that spelling as-is to load the package, the
    /// package's root directory (and therefore every compiler flag derived
    /// from it, e.g. `-I<root>/include`) would silently differ between "no
    /// lockfile yet" and "lockfile exists", defeating the incremental
    /// builder's content-addressed fingerprints and forcing a full rebuild
    /// the moment a lockfile is first written. `SourceCache::create_source`
    /// closes that gap by canonicalizing before constructing the
    /// `PathSource`, regardless of what spelling `.path()` happened to
    /// carry.
    #[test]
    fn test_path_dependency_root_is_always_canonical() {
        let tmp = TempDir::new().unwrap();

        let dep_dir = tmp.path().join("dep");
        write_lib_package(&dep_dir, "dep", "");
        // Compare against the same normalization the loader uses, not raw
        // canonicalize(): on Windows canonicalize() yields a `\\?\` verbatim
        // path, which must not reach a compiler and so is stripped. The
        // property under test is that the load root does not depend on which
        // spelling was interned -- not that it equals any particular
        // platform's canonical form.
        let expected_root = crate::util::fs::normalize_path(&dep_dir);

        // An uncanonicalized spelling of the very same directory - what a
        // fresh resolve settles on for `dep = { path = "../dep" }` inside
        // `app/Harbour.toml` (`resolve_dependency` only joins, it never
        // canonicalizes). This is the *first* construction of this
        // `SourceId` identity in this test, so whatever spelling
        // `.path()` returns from here on is this one.
        std::fs::create_dir_all(tmp.path().join("app")).unwrap();
        let uncanonical_dep_path = tmp.path().join("app").join("..").join("dep");
        assert_ne!(
            uncanonical_dep_path, expected_root,
            "the two spellings must actually differ as strings for this test to be meaningful"
        );

        let source_id = crate::core::SourceId::for_path(&uncanonical_dep_path).unwrap();
        assert_eq!(
            source_id.path(),
            Some(uncanonical_dep_path.as_path()),
            "SourceId::path() is documented to preserve the caller's spelling"
        );

        let mut cache = SourceCache::new(tmp.path().join("cache"));
        let pkg_id = crate::core::PackageId::new("dep", semver::Version::new(0, 1, 0), source_id);
        let loaded_root = cache.package_path(pkg_id).unwrap();

        assert_eq!(
            loaded_root, expected_root,
            "the package root SourceCache actually loads from must be canonical, \
             not whatever spelling SourceId::path() happens to carry - otherwise a fresh \
             resolve and a lockfile-decoded resolve of the same dependency load it from \
             differently-spelled (but identical) directories, and every compiler flag \
             derived from that root (e.g. -I<root>/include) disagrees between the two, \
             defeating the incremental build cache"
        );
    }

    // =========================================================================
    // Git / registry transitive dependencies.
    //
    // These exercise the same class of bug the tests above cover for path
    // dependencies, but for git and registry sources: a dependency's own
    // dependencies must be visible to the resolver without the *consuming*
    // package's manifest redeclaring them. Unlike path dependencies, this
    // can only be tested against a real (if local and offline) git
    // repository, since `RegistrySource`/`GitSource` fetch via `git2`.
    // =========================================================================

    use crate::test_support::fixtures::local_registry;

    /// Commit every file currently in `dir` to the git repository rooted
    /// there, creating the repository first if `dir` is not one yet.
    ///
    /// Returns the full commit SHA, needed for a registry shim's `rev`
    /// field (shims pin an exact commit, never a branch or tag).
    fn commit_all(dir: &std::path::Path, message: &str) -> String {
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

        let commit_id = repo
            .commit(Some("HEAD"), &sig, &sig, message, &tree, &parent_refs)
            .unwrap();

        commit_id.to_string()
    }

    /// Build a small git repository at `dir` containing a `staticlib`
    /// package named `name` with the given (already-formatted)
    /// `[dependencies]` block, and return the full commit SHA of its
    /// single commit.
    fn git_package_repo(dir: &std::path::Path, name: &str, deps_toml: &str) -> String {
        write_lib_package(dir, name, deps_toml);
        commit_all(dir, "initial commit")
    }

    /// Write a shim file (tier 2) for `name`@`version` into the registry
    /// index at `registry_dir`, pointing at a git source.
    ///
    /// This does *not* commit, and does *not* by itself make the version
    /// visible to `RegistrySource::query` - the whole point of these tests
    /// is that resolution now reads the tier-1 index, not the tier-2
    /// shims. Call [`finalize_registry`] once all shims for a test have
    /// been written, to (re)generate the tier-1 index from them and
    /// commit everything in one go - exactly the "CI generates and
    /// commits the index from the shims" flow the design describes.
    fn add_registry_shim(
        registry_dir: &std::path::Path,
        name: &str,
        version: &str,
        source_git_url: &str,
        source_rev: &str,
    ) {
        let first_char = name.chars().next().unwrap();
        let shim_dir = registry_dir
            .join("index")
            .join(first_char.to_string())
            .join(name);
        std::fs::create_dir_all(&shim_dir).unwrap();
        std::fs::write(
            shim_dir.join(format!("{version}.toml")),
            format!(
                r#"
[package]
name = "{name}"
version = "{version}"

[source.git]
url = "{source_git_url}"
rev = "{source_rev}"
"#
            ),
        )
        .unwrap();
    }

    /// (Re)generate the tier-1 index for every shim written into
    /// `registry_dir` so far, and commit the result to the registry's own
    /// (git-backed) repository - the step a real registry's CI performs on
    /// every publish.
    fn finalize_registry(registry_dir: &std::path::Path, gen_cache_dir: &std::path::Path) {
        crate::sources::registry::generate_index(registry_dir, gen_cache_dir)
            .expect("tier-1 index generation must succeed for a well-formed test fixture");
        commit_all(registry_dir, "generate tier-1 index");
    }

    /// The whole point of the tier-1/tier-2 split: resolving a version
    /// *range* against a registry package that has several published
    /// versions, one of which has a transitive registry dependency of its
    /// own, must never fetch a single source - not the range's
    /// candidates, and not the transitive dependency's. Only the tier-1
    /// index file is read.
    ///
    /// This asserts the absence of the fetch directly (the source cache
    /// directory tree is never created), rather than only checking the
    /// resolved answer - a resolver that happened to pick the right
    /// version *after* downloading every candidate would still pass a
    /// correctness-only test, and is exactly the regression this design
    /// exists to prevent.
    #[test]
    fn test_registry_range_resolution_fetches_no_source() {
        let tmp = TempDir::new().unwrap();

        let registry_dir = tmp.path().join("registry");
        local_registry::init(&registry_dir).unwrap();
        let registry_url = local_registry::file_url(&registry_dir);

        // `liba` has two published versions; only 0.2.0 depends on
        // `libshared` (also served from the registry).
        let liba_old_src = tmp.path().join("src-liba-0.1.0");
        let liba_old_rev = git_package_repo(&liba_old_src, "liba", "");
        let liba_old_url = local_registry::file_url(&liba_old_src);
        add_registry_shim(&registry_dir, "liba", "0.1.0", &liba_old_url, &liba_old_rev);

        let libshared_src = tmp.path().join("src-libshared");
        let libshared_rev = git_package_repo(&libshared_src, "libshared", "");
        let libshared_url = local_registry::file_url(&libshared_src);
        add_registry_shim(
            &registry_dir,
            "libshared",
            "0.1.0",
            &libshared_url,
            &libshared_rev,
        );

        let liba_new_src = tmp.path().join("src-liba-0.2.0");
        std::fs::create_dir_all(liba_new_src.join("src")).unwrap();
        std::fs::write(
            liba_new_src.join("Harbour.toml"),
            format!(
                r#"
[package]
name = "liba"
version = "0.2.0"

[dependencies]
libshared = {{ version = "0.1.0", registry = "{registry_url}" }}

[targets.liba]
kind = "staticlib"
sources = ["src/**/*.c"]
"#
            ),
        )
        .unwrap();
        std::fs::write(liba_new_src.join("src/lib.c"), "void liba_init(void) {}").unwrap();
        let liba_new_rev = commit_all(&liba_new_src, "initial commit");
        let liba_new_url = local_registry::file_url(&liba_new_src);
        add_registry_shim(&registry_dir, "liba", "0.2.0", &liba_new_url, &liba_new_rev);

        finalize_registry(&registry_dir, &tmp.path().join("gen-cache"));

        // A version *range*, not an exact pin - this is what used to force
        // a source fetch per candidate version.
        let app_dir = tmp.path().join("app");
        write_exe_package(
            &app_dir,
            "app",
            &format!(r#"liba = {{ version = ">=0.1.0, <0.3.0", registry = "{registry_url}" }}"#),
        );

        let ctx = GlobalContext::with_cwd(app_dir.clone()).unwrap();
        let ws = Workspace::new(&app_dir.join("Harbour.toml"), &ctx).unwrap();
        let cache_dir = tmp.path().join("cache");
        let mut cache = SourceCache::new(cache_dir.clone());

        let resolve = resolve_fresh(&ws, &mut cache, false).unwrap();

        assert!(resolve.contains_name("app"));
        assert!(resolve.contains_name("liba"));
        assert!(
            resolve.contains_name("libshared"),
            "liba 0.2.0's transitive dependency must be visible from the tier-1 index alone"
        );

        let liba_id = resolve.get_package_by_name("liba".into()).unwrap();
        assert_eq!(
            liba_id.version(),
            &semver::Version::new(0, 2, 0),
            "the resolver should pick the newest version satisfying the range"
        );

        let registry_src_cache = cache_dir.join("registry-src");
        assert!(
            !registry_src_cache.exists(),
            "resolving a registry version range must not fetch any package source - \
             found {} on disk, meaning some candidate's source was cloned during \
             resolution instead of just its tier-1 index record",
            registry_src_cache.display()
        );
    }

    /// A registry dependency's own dependency (also served from the
    /// registry) must resolve without the *consuming* package's manifest
    /// redeclaring it - the same bug fixed above for path dependencies,
    /// but for a registry source.
    ///
    /// Graph: app -> libb (registry) -> liba (registry), with `app` never
    /// mentioning `liba`.
    #[test]
    fn test_transitive_registry_dependency_resolves() {
        let tmp = TempDir::new().unwrap();

        let registry_dir = tmp.path().join("registry");
        local_registry::init(&registry_dir).unwrap();
        let registry_url = local_registry::file_url(&registry_dir);

        let liba_src = tmp.path().join("src-liba");
        let liba_rev = git_package_repo(&liba_src, "liba", "");
        let liba_url = local_registry::file_url(&liba_src);

        let libb_src = tmp.path().join("src-libb");
        let libb_rev = git_package_repo(
            &libb_src,
            "libb",
            &format!(r#"liba = {{ version = "0.1.0", registry = "{registry_url}" }}"#),
        );
        let libb_url = local_registry::file_url(&libb_src);

        add_registry_shim(&registry_dir, "liba", "0.1.0", &liba_url, &liba_rev);
        add_registry_shim(&registry_dir, "libb", "0.1.0", &libb_url, &libb_rev);
        finalize_registry(&registry_dir, &tmp.path().join("gen-cache"));

        let app_dir = tmp.path().join("app");
        write_exe_package(
            &app_dir,
            "app",
            &format!(r#"libb = {{ version = "0.1.0", registry = "{registry_url}" }}"#),
        );

        let ctx = GlobalContext::with_cwd(app_dir.clone()).unwrap();
        let ws = Workspace::new(&app_dir.join("Harbour.toml"), &ctx).unwrap();
        let mut cache = SourceCache::new(tmp.path().join("cache"));

        let resolve = resolve_fresh(&ws, &mut cache, false).unwrap();

        assert!(resolve.contains_name("app"));
        assert!(resolve.contains_name("libb"));
        assert!(
            resolve.contains_name("liba"),
            "liba should be transitively resolved through libb, a registry dependency, \
             even though app's manifest never mentions it"
        );

        let app_id = resolve.get_package_by_name("app".into()).unwrap();
        let libb_id = resolve.get_package_by_name("libb".into()).unwrap();
        let liba_id = resolve.get_package_by_name("liba".into()).unwrap();

        assert_eq!(resolve.deps(app_id), vec![libb_id]);
        assert_eq!(resolve.deps(libb_id), vec![liba_id]);
    }

    /// The same gap, but for a *git* dependency (not fetched through a
    /// registry at all): `libb` is a direct `git` dependency of `app`, and
    /// `libb`'s own manifest depends on `liba` through the registry. Both
    /// source kinds must go through the same lazy-discovery mechanism.
    #[test]
    fn test_transitive_git_dependency_resolves() {
        let tmp = TempDir::new().unwrap();

        let registry_dir = tmp.path().join("registry");
        local_registry::init(&registry_dir).unwrap();
        let registry_url = local_registry::file_url(&registry_dir);

        let liba_src = tmp.path().join("src-liba");
        let liba_rev = git_package_repo(&liba_src, "liba", "");
        let liba_url = local_registry::file_url(&liba_src);
        add_registry_shim(&registry_dir, "liba", "0.1.0", &liba_url, &liba_rev);
        finalize_registry(&registry_dir, &tmp.path().join("gen-cache"));

        let libb_src = tmp.path().join("src-libb");
        git_package_repo(
            &libb_src,
            "libb",
            &format!(r#"liba = {{ version = "0.1.0", registry = "{registry_url}" }}"#),
        );
        let libb_url = local_registry::file_url(&libb_src);

        let app_dir = tmp.path().join("app");
        write_exe_package(
            &app_dir,
            "app",
            &format!(r#"libb = {{ git = "{libb_url}" }}"#),
        );

        let ctx = GlobalContext::with_cwd(app_dir.clone()).unwrap();
        let ws = Workspace::new(&app_dir.join("Harbour.toml"), &ctx).unwrap();
        let mut cache = SourceCache::new(tmp.path().join("cache"));

        let resolve = resolve_fresh(&ws, &mut cache, false).unwrap();

        assert!(resolve.contains_name("app"));
        assert!(resolve.contains_name("libb"));
        assert!(
            resolve.contains_name("liba"),
            "liba should be transitively resolved through libb, a git dependency, \
             even though app's manifest never mentions it"
        );

        let libb_id = resolve.get_package_by_name("libb".into()).unwrap();
        let liba_id = resolve.get_package_by_name("liba".into()).unwrap();
        assert_eq!(resolve.deps(libb_id), vec![liba_id]);
    }

    /// A diamond through registry dependencies (two packages depending on
    /// the same third registry package) must collapse to one package, not
    /// two - the MVP invariant C linking depends on.
    #[test]
    fn test_transitive_registry_dependency_diamond_dedup() {
        let tmp = TempDir::new().unwrap();

        let registry_dir = tmp.path().join("registry");
        local_registry::init(&registry_dir).unwrap();
        let registry_url = local_registry::file_url(&registry_dir);

        let shared_src = tmp.path().join("src-shared");
        let shared_rev = git_package_repo(&shared_src, "libshared", "");
        let shared_url = local_registry::file_url(&shared_src);
        add_registry_shim(
            &registry_dir,
            "libshared",
            "0.1.0",
            &shared_url,
            &shared_rev,
        );

        let dep_on_shared =
            format!(r#"libshared = {{ version = "0.1.0", registry = "{registry_url}" }}"#);

        let libb_src = tmp.path().join("src-libb");
        let libb_rev = git_package_repo(&libb_src, "libb", &dep_on_shared);
        let libb_url = local_registry::file_url(&libb_src);
        add_registry_shim(&registry_dir, "libb", "0.1.0", &libb_url, &libb_rev);

        let libc_src = tmp.path().join("src-libc");
        let libc_rev = git_package_repo(&libc_src, "libc", &dep_on_shared);
        let libc_url = local_registry::file_url(&libc_src);
        add_registry_shim(&registry_dir, "libc", "0.1.0", &libc_url, &libc_rev);
        finalize_registry(&registry_dir, &tmp.path().join("gen-cache"));

        let app_dir = tmp.path().join("app");
        write_exe_package(
            &app_dir,
            "app",
            &format!(
                r#"libb = {{ version = "0.1.0", registry = "{registry_url}" }}
libc = {{ version = "0.1.0", registry = "{registry_url}" }}"#
            ),
        );

        let ctx = GlobalContext::with_cwd(app_dir.clone()).unwrap();
        let ws = Workspace::new(&app_dir.join("Harbour.toml"), &ctx).unwrap();
        let mut cache = SourceCache::new(tmp.path().join("cache"));

        let resolve = resolve_fresh(&ws, &mut cache, false).unwrap();

        let shared_pkgs = resolve.get_packages_by_name("libshared".into());
        assert_eq!(
            shared_pkgs.len(),
            1,
            "diamond registry dependency must collapse to a single package, found {:?}",
            shared_pkgs
        );
    }

    /// A cycle through registry dependencies (`a` -> `b` -> `a`) must be
    /// reported as a clear error, not hang or panic. Unlike the
    /// path-dependency cycle test above, PubGrub itself has no trouble
    /// *choosing versions* for this graph (it only reasons about SemVer
    /// compatibility); the cycle is caught by the post-resolution graph
    /// check that applies uniformly to every source kind.
    #[test]
    fn test_transitive_registry_dependency_cycle_errors() {
        let tmp = TempDir::new().unwrap();

        let registry_dir = tmp.path().join("registry");
        local_registry::init(&registry_dir).unwrap();
        let registry_url = local_registry::file_url(&registry_dir);

        let cyca_src = tmp.path().join("src-cyca");
        let cycb_src = tmp.path().join("src-cycb");

        let cyca_rev = git_package_repo(
            &cyca_src,
            "cyca",
            &format!(r#"cycb = {{ version = "0.1.0", registry = "{registry_url}" }}"#),
        );
        let cyca_url = local_registry::file_url(&cyca_src);

        let cycb_rev = git_package_repo(
            &cycb_src,
            "cycb",
            &format!(r#"cyca = {{ version = "0.1.0", registry = "{registry_url}" }}"#),
        );
        let cycb_url = local_registry::file_url(&cycb_src);

        add_registry_shim(&registry_dir, "cyca", "0.1.0", &cyca_url, &cyca_rev);
        add_registry_shim(&registry_dir, "cycb", "0.1.0", &cycb_url, &cycb_rev);
        finalize_registry(&registry_dir, &tmp.path().join("gen-cache"));

        let app_dir = tmp.path().join("app");
        write_exe_package(
            &app_dir,
            "app",
            &format!(r#"cyca = {{ version = "0.1.0", registry = "{registry_url}" }}"#),
        );

        let ctx = GlobalContext::with_cwd(app_dir.clone()).unwrap();
        let ws = Workspace::new(&app_dir.join("Harbour.toml"), &ctx).unwrap();
        let mut cache = SourceCache::new(tmp.path().join("cache"));

        let err = resolve_fresh(&ws, &mut cache, false).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("cycle detected in dependency graph"),
            "unexpected error message: {msg}"
        );
    }

    /// The subtle case requirement 2 calls out explicitly: the *same*
    /// library name reached through two genuinely different sources (here,
    /// a direct `git` dependency for one consumer, and the registry for
    /// another). `PubGrubPackage` identity is `(name, source_id)`, so the
    /// solver treats these as two unrelated packages and would happily
    /// resolve both - silently linking two copies of `zlib`-alike, which is
    /// exactly the outcome that must not happen silently. This must
    /// surface as a clear, actionable error instead.
    #[test]
    fn test_same_name_different_sources_errors() {
        let tmp = TempDir::new().unwrap();

        let registry_dir = tmp.path().join("registry");
        local_registry::init(&registry_dir).unwrap();
        let registry_url = local_registry::file_url(&registry_dir);

        // `shared` is served both directly via git and via the registry -
        // two different `SourceId`s for the same package name.
        let shared_src = tmp.path().join("src-shared");
        let shared_rev = git_package_repo(&shared_src, "shared", "");
        let shared_url = local_registry::file_url(&shared_src);
        add_registry_shim(&registry_dir, "shared", "0.1.0", &shared_url, &shared_rev);

        let libb_src = tmp.path().join("src-libb");
        let libb_rev = git_package_repo(
            &libb_src,
            "libb",
            &format!(r#"shared = {{ version = "0.1.0", registry = "{registry_url}" }}"#),
        );
        let libb_url = local_registry::file_url(&libb_src);
        add_registry_shim(&registry_dir, "libb", "0.1.0", &libb_url, &libb_rev);
        finalize_registry(&registry_dir, &tmp.path().join("gen-cache"));

        let app_dir = tmp.path().join("app");
        write_exe_package(
            &app_dir,
            "app",
            &format!(
                r#"shared = {{ git = "{shared_url}" }}
libb = {{ version = "0.1.0", registry = "{registry_url}" }}"#
            ),
        );

        let ctx = GlobalContext::with_cwd(app_dir.clone()).unwrap();
        let ws = Workspace::new(&app_dir.join("Harbour.toml"), &ctx).unwrap();
        let mut cache = SourceCache::new(tmp.path().join("cache"));

        let err = resolve_fresh(&ws, &mut cache, false).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("shared") && msg.contains("more than one source"),
            "unexpected error message: {msg}"
        );
    }
}
