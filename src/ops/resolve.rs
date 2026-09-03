//! Workspace resolution operations.

use std::collections::{HashMap, HashSet};

use anyhow::{bail, Context, Result};

use crate::core::dependency::{resolve_dependency, warn_workspace_dep_matches_member, Dependency};
use crate::core::workspace::find_manifest;
use crate::core::{Package, SourceId, Workspace};
use crate::ops::lockfile::{
    load_lockfile, save_workspace_lockfile, workspace_lockfile_needs_update,
};
use crate::resolver::{HarbourResolver, Resolve};
use crate::sources::SourceCache;

/// Recursively discover path dependencies reachable from `dep`, appending
/// every path dependency found (including `dep` itself, if it is a path
/// dependency) to `out`.
///
/// This closes the gap where only path dependencies declared directly in a
/// workspace member's manifest were ever registered as available to the
/// resolver: a path dependency's *own* path dependencies (and theirs,
/// recursively) were never read, so a library could never depend on another
/// library transitively through a path dependency.
///
/// Relative paths inside a dependency's manifest are resolved relative to
/// *that* manifest's directory (via [`resolve_dependency`]'s `manifest_dir`
/// argument), not relative to the workspace root - `../liba` in
/// `libb/Harbour.toml` means `libb/../liba`.
///
/// Termination and cycle handling:
/// - `done` holds the canonical [`SourceId`] of every path dependency whose
///   manifest has already been fully walked; encountering one again (e.g. a
///   diamond where two packages both depend on the same third package) is a
///   no-op rather than a re-walk.
/// - `in_progress` holds the canonical `SourceId`s currently on the walk's
///   call stack; encountering one of those again means a cycle
///   (`a` depends on `b` depends on `a`) and produces a clear error instead
///   of recursing forever.
///
/// `SourceId` equality (and therefore membership in `done`/`in_progress`) is
/// based on the canonicalized path, not the original spelling used in the
/// manifest, so `../liba` and `../../foo/liba` that happen to point at the
/// same directory are correctly recognized as the same package.
fn collect_transitive_path_deps(
    dep: &Dependency,
    in_progress: &mut Vec<SourceId>,
    done: &mut HashSet<SourceId>,
    out: &mut Vec<Dependency>,
) -> Result<()> {
    if !dep.is_path() {
        // Git and registry sources fetch their own manifests through a
        // different route and are out of scope for this fix.
        return Ok(());
    }

    let source_id = dep.source_id();

    if done.contains(&source_id) {
        // Already fully walked via another route (diamond) - nothing to do.
        return Ok(());
    }

    if in_progress.contains(&source_id) {
        let cycle_desc = in_progress
            .iter()
            .map(|s| s.to_string())
            .chain(std::iter::once(source_id.to_string()))
            .collect::<Vec<_>>()
            .join(" -> ");
        bail!(
            "cycle detected in path dependencies: {}\n\
             help: a path dependency cannot (transitively) depend on itself",
            cycle_desc
        );
    }

    let path = source_id
        .path()
        .ok_or_else(|| anyhow::anyhow!("path dependency `{}` is missing a path", dep.name()))?;

    let manifest_path = find_manifest(path)
        .with_context(|| format!("while loading path dependency `{}`", dep.name()))?;
    let package = Package::load(&manifest_path)
        .with_context(|| format!("while loading path dependency `{}`", dep.name()))?;

    let manifest = package.manifest();
    let manifest_dir = package.root();

    in_progress.push(source_id);

    for (child_name, child_spec) in &manifest.dependencies {
        // A dependency-of-a-dependency has no workspace context of its own:
        // it isn't a member of *this* workspace, so there is no local-first
        // matching against workspace members and no [workspace.dependencies]
        // to inherit from.
        let child_dep =
            resolve_dependency(child_name, child_spec, None, &HashMap::new(), manifest_dir)
                .with_context(|| {
                    format!(
                        "while resolving dependency `{}` of path dependency `{}`",
                        child_name,
                        dep.name()
                    )
                })?;

        out.push(child_dep.clone());
        collect_transitive_path_deps(&child_dep, in_progress, done, out)?;
    }

    in_progress.pop();
    done.insert(source_id);

    Ok(())
}

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

    // Collect all dependencies from all members
    let workspace_deps = ws.workspace_dependencies();
    let member_paths = ws.member_paths();
    let mut all_deps: Vec<Dependency> = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new(); // (name, source_id)

    // Path dependencies already fully walked (or currently being walked, for
    // cycle detection) while discovering transitive path dependencies below.
    let mut path_deps_done: HashSet<SourceId> = HashSet::new();
    let mut path_deps_in_progress: Vec<SourceId> = Vec::new();

    // Add dependencies from each member
    for member in ws.members() {
        let manifest = member.package.manifest();
        let manifest_dir = member.package.root();

        for (name, spec) in &manifest.dependencies {
            let dep = resolve_dependency(name, spec, workspace_deps, &member_paths, manifest_dir)?;

            // Recursively pull in this dependency's own path dependencies
            // (and theirs, and so on) so that a library can depend on
            // another library transitively through a path dependency.
            let mut transitive = Vec::new();
            collect_transitive_path_deps(
                &dep,
                &mut path_deps_in_progress,
                &mut path_deps_done,
                &mut transitive,
            )?;

            // Dedupe by (name, source_id)
            for dep in std::iter::once(dep).chain(transitive) {
                let key = (dep.name().to_string(), dep.source_id().to_string());
                if !seen.contains(&key) {
                    seen.insert(key);
                    all_deps.push(dep);
                }
            }
        }
    }

    // Use first member as root for resolver (will be improved when resolver supports multiple roots)
    let root_package = ws.root_package();
    let root_summary = root_package.summary()?;
    let mut resolver = HarbourResolver::new(root_summary.clone());

    // Ensure all sources are ready
    source_cache.ensure_ready(&all_deps)?;

    // Query each dependency and add summaries
    for dep in &all_deps {
        let summaries = source_cache.query(dep)?;
        resolver.add_summaries(summaries);
    }

    // Resolve
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
            msg.contains("cycle detected in path dependencies"),
            "unexpected error message: {msg}"
        );
    }
}
