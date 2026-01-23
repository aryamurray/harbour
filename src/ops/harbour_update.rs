//! Implementation of `harbour update`.

use anyhow::Result;

use crate::core::Workspace;
use crate::ops::resolve::update_resolve;
use crate::resolver::Resolve;
use crate::sources::SourceCache;

/// Options for update command.
#[derive(Debug, Clone, Default)]
pub struct UpdateOptions {
    /// Specific packages to update (empty = all)
    pub packages: Vec<String>,

    /// Perform aggressive update (ignore lockfile completely)
    pub aggressive: bool,

    /// Dry run - show what would be updated without changing lockfile
    pub dry_run: bool,
}

/// Update the lockfile by re-resolving dependencies.
///
/// This is the ONLY command that modifies the lockfile.
pub fn update(ws: &Workspace, source_cache: &mut SourceCache, opts: &UpdateOptions) -> Result<Resolve> {
    if opts.dry_run {
        tracing::info!("Dry run - lockfile will not be modified");
    }

    let resolve = update_resolve(ws, source_cache)?;

    if opts.dry_run {
        // Don't save - already saved by update_resolve, need to restore
        // For proper dry run, we'd need to refactor to separate resolve and save
        tracing::warn!("Note: dry run not fully implemented yet");
    }

    Ok(resolve)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::GlobalContext;
    use tempfile::TempDir;

    fn create_test_workspace(dir: &std::path::Path) {
        std::fs::write(
            dir.join("Harbor.toml"),
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
    fn test_update() {
        let tmp = TempDir::new().unwrap();
        create_test_workspace(tmp.path());

        let ctx = GlobalContext::with_cwd(tmp.path().to_path_buf()).unwrap();
        let ws = Workspace::new(&tmp.path().join("Harbor.toml"), &ctx).unwrap();

        let mut cache = SourceCache::new(tmp.path().join("cache"));
        let opts = UpdateOptions::default();

        let resolve = update(&ws, &mut cache, &opts).unwrap();
        assert!(resolve.len() >= 1);

        // Lockfile should exist
        assert!(ws.lockfile_path().exists());
    }
}
