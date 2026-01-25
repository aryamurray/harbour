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
            bail!(
                "lockfile exists but could not be loaded; run `harbour update` to regenerate it"
            );
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

    resolve_fresh(ws, source_cache)
}

/// Perform fresh dependency resolution for all workspace members.
pub fn resolve_fresh(ws: &Workspace, source_cache: &mut SourceCache) -> Result<Resolve> {
    // Warn if workspace dependencies match member names
    if let Some(ws_deps) = ws.workspace_dependencies() {
        warn_workspace_dep_matches_member(ws_deps, &ws.member_paths());
    }

    // Collect all dependencies from all members
    let workspace_deps = ws.workspace_dependencies();
    let member_paths = ws.member_paths();
    let mut all_deps: Vec<Dependency> = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new(); // (name, source_id)

    // Add dependencies from each member
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
            )?;

            // Dedupe by (name, source_id)
            let key = (dep.name().to_string(), dep.source_id().to_string());
            if !seen.contains(&key) {
                seen.insert(key);
                all_deps.push(dep);
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

    // Save lockfile with workspace hash
    save_workspace_lockfile(&ws.lockfile_path(), &resolve, ws)?;

    Ok(resolve)
}

/// Update the lockfile by re-resolving dependencies.
pub fn update_resolve(ws: &Workspace, source_cache: &mut SourceCache) -> Result<Resolve> {
    tracing::info!("Updating dependencies");
    resolve_fresh(ws, source_cache)
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
}
