//! Workspace resolution operations.

use anyhow::Result;

use crate::core::Workspace;
use crate::ops::lockfile::{load_lockfile, lockfile_needs_update, save_lockfile};
use crate::resolver::{HarbourResolver, Resolve};
use crate::sources::SourceCache;

/// Resolve the workspace dependencies.
///
/// Uses content-based freshness detection to determine if re-resolution is needed.
/// If the lockfile exists and the manifest hasn't changed, use the lockfile.
/// Otherwise, perform fresh resolution.
pub fn resolve_workspace(ws: &Workspace, source_cache: &mut SourceCache) -> Result<Resolve> {
    let lockfile_path = ws.lockfile_path();
    let manifest_path = ws.manifest_path();

    // Check if lockfile needs update using content-based hash
    if !lockfile_needs_update(&manifest_path, &lockfile_path)? {
        // Lockfile is fresh, try to load it
        if let Some(resolve) = load_lockfile(&lockfile_path)? {
            tracing::info!("Using existing lockfile (manifest unchanged)");
            return Ok(resolve);
        }
    }

    // Lockfile doesn't exist, is stale, or couldn't be loaded - resolve fresh
    if lockfile_path.exists() {
        tracing::info!("Manifest changed, re-resolving dependencies");
    } else {
        tracing::info!("No lockfile found, resolving dependencies");
    }

    resolve_fresh(ws, source_cache)
}

/// Perform fresh dependency resolution.
pub fn resolve_fresh(ws: &Workspace, source_cache: &mut SourceCache) -> Result<Resolve> {
    let root_package = ws.root_package();
    let root_summary = root_package.summary()?;

    // Create resolver with root package
    let mut resolver = HarbourResolver::new(root_summary.clone());

    // Collect and query all dependencies
    let deps: Vec<_> = ws
        .manifest()
        .dependencies
        .iter()
        .map(|(name, spec)| spec.to_dependency(name, ws.root()))
        .collect::<Result<Vec<_>>>()?;

    // Ensure all sources are ready
    source_cache.ensure_ready(&deps)?;

    // Query each dependency and add summaries
    for dep in &deps {
        let summaries = source_cache.query(dep)?;
        resolver.add_summaries(summaries);
    }

    // Resolve
    let resolve = resolver.resolve()?;

    // Save lockfile with manifest hash for content-based freshness detection
    save_lockfile(&ws.lockfile_path(), &resolve, &ws.manifest_path())?;

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
