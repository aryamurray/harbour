//! Lockfile I/O operations.

use std::path::Path;

use anyhow::{Context, Result};

use crate::resolver::encode::Lockfile;
use crate::resolver::Resolve;

/// Load a lockfile from the given path.
pub fn load_lockfile(path: &Path) -> Result<Option<Resolve>> {
    if !path.exists() {
        return Ok(None);
    }

    let lockfile = Lockfile::load(path)?;

    if !lockfile.is_compatible() {
        anyhow::bail!(
            "lockfile version {} is not compatible with this version of Harbour",
            lockfile.version
        );
    }

    let resolve = lockfile.to_resolve()?;
    Ok(Some(resolve))
}

/// Save a resolve to the lockfile.
pub fn save_lockfile(path: &Path, resolve: &Resolve) -> Result<()> {
    let lockfile = Lockfile::from_resolve(resolve);
    lockfile.save(path)?;
    Ok(())
}

/// Check if the lockfile needs updating.
///
/// Returns true if:
/// - Lockfile doesn't exist
/// - Manifest has changed since lockfile was created
pub fn lockfile_needs_update(manifest_path: &Path, lockfile_path: &Path) -> bool {
    if !lockfile_path.exists() {
        return true;
    }

    // Compare modification times
    let manifest_mtime = std::fs::metadata(manifest_path)
        .and_then(|m| m.modified())
        .ok();
    let lockfile_mtime = std::fs::metadata(lockfile_path)
        .and_then(|m| m.modified())
        .ok();

    match (manifest_mtime, lockfile_mtime) {
        (Some(m), Some(l)) => m > l,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{PackageId, SourceId, Summary};
    use semver::Version;
    use tempfile::TempDir;

    #[test]
    fn test_lockfile_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let lockfile_path = tmp.path().join("Harbor.lock");

        let source = SourceId::for_path(tmp.path()).unwrap();
        let pkg_id = PackageId::new("test", Version::new(1, 0, 0), source);

        let mut resolve = Resolve::new();
        resolve.add_package(pkg_id, Summary::new(pkg_id, vec![], None));

        save_lockfile(&lockfile_path, &resolve).unwrap();

        let loaded = load_lockfile(&lockfile_path).unwrap().unwrap();
        assert_eq!(loaded.len(), 1);
    }

    #[test]
    fn test_missing_lockfile() {
        let tmp = TempDir::new().unwrap();
        let lockfile_path = tmp.path().join("nonexistent.lock");

        let result = load_lockfile(&lockfile_path).unwrap();
        assert!(result.is_none());
    }
}
