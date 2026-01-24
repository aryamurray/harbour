//! Lockfile I/O operations.

use std::path::Path;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use crate::core::Manifest;
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
pub fn save_lockfile(path: &Path, resolve: &Resolve, manifest_path: &Path) -> Result<()> {
    let manifest_hash = compute_manifest_hash(manifest_path)?;
    let lockfile = Lockfile::from_resolve(resolve).with_manifest_hash(manifest_hash);
    lockfile.save(path)?;
    Ok(())
}

/// Compute a hash of the manifest's resolution-affecting fields.
///
/// This creates a normalized representation of fields that affect dependency
/// resolution, making the hash stable across whitespace/comment changes.
pub fn compute_manifest_hash(manifest_path: &Path) -> Result<String> {
    let manifest = Manifest::load(manifest_path)?;

    // Create a normalized JSON representation of resolution-affecting fields.
    // This includes dependencies and target-level deps, but not things like
    // compile flags that don't affect which packages are resolved.
    let mut normalized = serde_json::Map::new();

    // Dependencies (sorted for determinism)
    let mut deps: Vec<_> = manifest.dependencies.iter().collect();
    deps.sort_by_key(|(name, _)| *name);
    let deps_json: serde_json::Value = deps
        .iter()
        .map(|(name, spec)| {
            let spec_json = serde_json::to_value(spec).unwrap_or(serde_json::Value::Null);
            ((*name).clone(), spec_json)
        })
        .collect::<serde_json::Map<_, _>>()
        .into();
    normalized.insert("dependencies".to_string(), deps_json);

    // Target deps (sorted for determinism)
    let mut targets_json = serde_json::Map::new();
    let mut targets: Vec<_> = manifest.targets.iter().collect();
    targets.sort_by_key(|t| t.name.as_str());
    for target in targets {
        if !target.deps.is_empty() {
            let mut target_deps: Vec<_> = target.deps.iter().collect();
            target_deps.sort_by_key(|(pkg_name, _)| pkg_name.as_str());
            let deps_json: serde_json::Value = target_deps
                .iter()
                .map(|(pkg_name, spec)| {
                    serde_json::json!({
                        "package": pkg_name.as_str(),
                        "target": spec.target,
                    })
                })
                .collect::<Vec<_>>()
                .into();
            targets_json.insert(target.name.to_string(), deps_json);
        }
    }
    if !targets_json.is_empty() {
        normalized.insert("target_deps".to_string(), targets_json.into());
    }

    // Hash the normalized representation
    let bytes = serde_json::to_vec(&serde_json::Value::Object(normalized))
        .context("failed to serialize normalized manifest")?;
    let hash = Sha256::digest(&bytes);
    Ok(hex::encode(hash))
}

/// Check if the lockfile needs updating.
///
/// Uses content-based detection by comparing manifest hashes.
/// This is more reliable than timestamps (which can lie due to git checkout,
/// unzip, clock skew, etc.).
///
/// Returns true if:
/// - Lockfile doesn't exist
/// - Lockfile has no manifest hash (old format)
/// - Manifest content hash differs from lockfile's recorded hash
pub fn lockfile_needs_update(manifest_path: &Path, lockfile_path: &Path) -> Result<bool> {
    if !lockfile_path.exists() {
        return Ok(true);
    }

    // Load the lockfile to get its stored hash
    let lockfile = match Lockfile::load(lockfile_path) {
        Ok(lf) => lf,
        Err(_) => return Ok(true), // Corrupted lockfile, needs regeneration
    };

    // If lockfile has no hash (old format), fall back to timestamp comparison
    let stored_hash = match lockfile.manifest_hash() {
        Some(h) => h,
        None => {
            // Old lockfile format without hash - use timestamp fallback
            return Ok(timestamp_based_check(manifest_path, lockfile_path));
        }
    };

    // Compute current manifest hash
    let current_hash = compute_manifest_hash(manifest_path)?;

    Ok(stored_hash != current_hash)
}

/// Legacy timestamp-based check for old lockfiles without content hash.
fn timestamp_based_check(manifest_path: &Path, lockfile_path: &Path) -> bool {
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

    fn create_test_manifest(dir: &Path) -> std::path::PathBuf {
        let manifest_path = dir.join("Harbour.toml");
        std::fs::write(
            &manifest_path,
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
        manifest_path
    }

    #[test]
    fn test_lockfile_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let manifest_path = create_test_manifest(tmp.path());
        let lockfile_path = tmp.path().join("Harbour.lock");

        let source = SourceId::for_path(tmp.path()).unwrap();
        let pkg_id = PackageId::new("test", Version::new(1, 0, 0), source);

        let mut resolve = Resolve::new();
        resolve.add_package(pkg_id, Summary::new(pkg_id, vec![], None));

        save_lockfile(&lockfile_path, &resolve, &manifest_path).unwrap();

        let loaded = load_lockfile(&lockfile_path).unwrap().unwrap();
        assert_eq!(loaded.len(), 1);
    }

    #[test]
    fn test_lockfile_has_manifest_hash() {
        let tmp = TempDir::new().unwrap();
        let manifest_path = create_test_manifest(tmp.path());
        let lockfile_path = tmp.path().join("Harbour.lock");

        let source = SourceId::for_path(tmp.path()).unwrap();
        let pkg_id = PackageId::new("test", Version::new(1, 0, 0), source);

        let mut resolve = Resolve::new();
        resolve.add_package(pkg_id, Summary::new(pkg_id, vec![], None));

        save_lockfile(&lockfile_path, &resolve, &manifest_path).unwrap();

        // Verify the lockfile contains a manifest hash
        let content = std::fs::read_to_string(&lockfile_path).unwrap();
        assert!(content.contains("root_manifest_hash"));
    }

    #[test]
    fn test_lockfile_needs_update_detects_manifest_change() {
        let tmp = TempDir::new().unwrap();
        let manifest_path = create_test_manifest(tmp.path());
        let lockfile_path = tmp.path().join("Harbour.lock");

        let source = SourceId::for_path(tmp.path()).unwrap();
        let pkg_id = PackageId::new("test", Version::new(1, 0, 0), source);

        let mut resolve = Resolve::new();
        resolve.add_package(pkg_id, Summary::new(pkg_id, vec![], None));

        save_lockfile(&lockfile_path, &resolve, &manifest_path).unwrap();

        // Lockfile should not need update initially
        assert!(!lockfile_needs_update(&manifest_path, &lockfile_path).unwrap());

        // Add a dependency to the manifest
        std::fs::write(
            &manifest_path,
            r#"
[package]
name = "test"
version = "1.0.0"

[dependencies]
newdep = { path = "../newdep" }

[targets.test]
kind = "exe"
sources = ["src/**/*.c"]
"#,
        )
        .unwrap();

        // Lockfile should now need update
        assert!(lockfile_needs_update(&manifest_path, &lockfile_path).unwrap());
    }

    #[test]
    fn test_lockfile_hash_ignores_whitespace_changes() {
        let tmp = TempDir::new().unwrap();
        let manifest_path = tmp.path().join("Harbour.toml");

        // Create initial manifest
        std::fs::write(
            &manifest_path,
            r#"[package]
name = "test"
version = "1.0.0"
"#,
        )
        .unwrap();

        let hash1 = compute_manifest_hash(&manifest_path).unwrap();

        // Add whitespace and comments
        std::fs::write(
            &manifest_path,
            r#"
# This is a comment

[package]
name = "test"
version = "1.0.0"

# Another comment
"#,
        )
        .unwrap();

        let hash2 = compute_manifest_hash(&manifest_path).unwrap();

        // Hashes should be the same (normalized representation)
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_missing_lockfile() {
        let tmp = TempDir::new().unwrap();
        let lockfile_path = tmp.path().join("nonexistent.lock");

        let result = load_lockfile(&lockfile_path).unwrap();
        assert!(result.is_none());
    }
}
