//! Registry resolution and source fetching for verification.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use url::Url;

use super::types::{VerifyContext, VerifyOptions};
use crate::sources::registry::{RegistrySource, Shim};
use crate::sources::source::Source;
use crate::util::context::GlobalContext;

/// Resolve package from registry and set up verification context.
pub(crate) fn resolve_package(options: &VerifyOptions, ctx: &GlobalContext) -> Result<VerifyContext> {
    // Create temporary directory for verification
    let temp_dir = tempfile::tempdir().context("failed to create temp directory")?;

    // Use GlobalContext's cache directory if available, otherwise use temp dir
    let cache_dir = ctx.cache_dir().join("verify");
    std::fs::create_dir_all(&cache_dir)?;

    // Determine version to use
    let version = options.version.as_deref().unwrap_or("latest");

    // Load shim from registry
    let shim = if let Some(registry_path) = &options.registry_path {
        // Local registry (CI mode)
        load_shim_from_local_registry(registry_path, &options.package, version)?
    } else {
        // Remote registry (uses default registry from GlobalContext)
        let registry_url = ctx.default_registry_url();
        load_shim_from_remote_registry(&options.package, version, &cache_dir, &registry_url)?
    };

    Ok(VerifyContext {
        temp_dir,
        cache_dir,
        shim,
    })
}

/// Load shim from a local registry directory.
fn load_shim_from_local_registry(
    registry_path: &Path,
    package: &str,
    version: &str,
) -> Result<Shim> {
    // For local registry, directly load the shim file
    let first_char = package
        .chars()
        .next()
        .ok_or_else(|| anyhow::anyhow!("invalid empty package name"))?;

    let shim_path = if version == "latest" {
        // Find the latest version by scanning the directory
        let pkg_dir = registry_path
            .join("index")
            .join(first_char.to_string())
            .join(package);

        if !pkg_dir.exists() {
            bail!(
                "package '{}' not found in registry at {}",
                package,
                registry_path.display()
            );
        }

        find_latest_version(&pkg_dir)?
    } else {
        registry_path
            .join("index")
            .join(first_char.to_string())
            .join(package)
            .join(format!("{}.toml", version))
    };

    if !shim_path.exists() {
        bail!(
            "shim not found: {} (looked at {})",
            package,
            shim_path.display()
        );
    }

    Shim::load(&shim_path).context("failed to load shim")
}

/// Find the latest version in a package directory.
fn find_latest_version(pkg_dir: &Path) -> Result<PathBuf> {
    let mut versions: Vec<(semver::Version, PathBuf)> = Vec::new();

    for entry in std::fs::read_dir(pkg_dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.extension().map_or(false, |ext| ext == "toml") {
            if let Some(stem) = path.file_stem() {
                let version_str = stem.to_string_lossy();
                if let Ok(version) = semver::Version::parse(&version_str) {
                    versions.push((version, path));
                }
            }
        }
    }

    versions.sort_by(|a, b| b.0.cmp(&a.0)); // Sort descending

    versions
        .into_iter()
        .next()
        .map(|(_, path)| path)
        .ok_or_else(|| anyhow::anyhow!("no versions found in {}", pkg_dir.display()))
}

/// Load shim from the remote registry.
fn load_shim_from_remote_registry(
    package: &str,
    version: &str,
    cache_dir: &Path,
    registry_url: &Url,
) -> Result<Shim> {
    let source_id = crate::core::SourceId::for_registry(registry_url)?;
    let mut registry = RegistrySource::new(registry_url.clone(), cache_dir, source_id);

    // Ensure the index is fetched
    registry.ensure_ready()?;

    // Load the shim
    let version_to_load = if version == "latest" {
        // Find the latest version by scanning the package directory in the index
        let first_char = package
            .chars()
            .next()
            .ok_or_else(|| anyhow::anyhow!("invalid empty package name"))?;

        let pkg_dir = registry
            .index_path()
            .join("index")
            .join(first_char.to_string())
            .join(package);

        if !pkg_dir.exists() {
            bail!("package '{}' not found in remote registry", package);
        }

        let latest_shim_path = find_latest_version(&pkg_dir)?;
        let version_str = latest_shim_path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow::anyhow!("invalid shim file name"))?
            .to_string();

        tracing::info!(
            "Resolved 'latest' to version {} for {}",
            version_str,
            package
        );
        version_str
    } else {
        version.to_string()
    };

    registry
        .load_shim(package, &version_to_load)?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "package '{}' version '{}' not found",
                package,
                version_to_load
            )
        })
}

/// Fetch the package source.
pub(crate) fn fetch_source(ctx: &VerifyContext) -> Result<PathBuf> {
    let source_dir = ctx.cache_dir.join("src").join(&ctx.shim.package.name);

    if let Some(git) = &ctx.shim.source.git {
        fetch_git_source(git, &source_dir)?;
    } else if let Some(tarball) = &ctx.shim.source.tarball {
        fetch_tarball_source(tarball, &source_dir)?;
    } else {
        bail!("no source specified in shim");
    }

    Ok(source_dir)
}

/// Fetch a tarball source.
///
/// Downloads the tarball, verifies the SHA256 hash, and extracts to the destination.
fn fetch_tarball_source(
    tarball: &crate::sources::registry::shim::TarballSource,
    dest: &Path,
) -> Result<()> {
    // Check if destination already has content (cached)
    if dest.exists() {
        if let Ok(mut entries) = std::fs::read_dir(dest) {
            if entries.next().is_some() {
                tracing::info!("Using cached tarball source at {}", dest.display());
                return Ok(());
            }
        }
    }

    tracing::info!("Fetching tarball from {}", tarball.url);

    // Download the tarball
    let response = reqwest::blocking::get(&tarball.url)
        .with_context(|| format!("failed to download tarball from {}", tarball.url))?;

    if !response.status().is_success() {
        bail!(
            "failed to download tarball from {}: HTTP {}",
            tarball.url,
            response.status()
        );
    }

    let tarball_bytes = response
        .bytes()
        .with_context(|| "failed to read tarball response body")?;

    // Verify SHA256 hash
    let actual_hash = crate::util::hash::sha256_bytes(&tarball_bytes);
    if actual_hash != tarball.sha256 {
        bail!(
            "tarball hash mismatch for {}:\n  expected: {}\n  actual:   {}",
            tarball.url,
            tarball.sha256,
            actual_hash
        );
    }

    tracing::debug!("Tarball hash verified: {}", &actual_hash[..16]);

    // Extract using the shared extraction function
    crate::sources::registry::extract_tarball(
        &tarball_bytes,
        dest,
        tarball.strip_prefix.as_deref(),
    )
    .with_context(|| format!("failed to extract tarball from {}", tarball.url))?;

    tracing::info!(
        "Extracted tarball to {} (strip_prefix: {:?})",
        dest.display(),
        tarball.strip_prefix
    );

    Ok(())
}

/// Get a short (8-char) version of a SHA for display.
fn short_sha(sha: &str) -> &str {
    &sha[..8.min(sha.len())]
}

/// Fetch a git source.
fn fetch_git_source(git: &crate::sources::registry::shim::GitSource, dest: &Path) -> Result<()> {
    use git2::{Repository, ResetType};

    let target_oid = git2::Oid::from_str(&git.rev)
        .with_context(|| format!("invalid commit SHA: {}", git.rev))?;

    // Check if we already have the right commit checked out
    if dest.exists() {
        if let Ok(repo) = Repository::open(dest) {
            if let Ok(head) = repo.head() {
                if let Some(oid) = head.target() {
                    if oid == target_oid {
                        tracing::info!("Using cached source at {}", short_sha(&git.rev));
                        return Ok(());
                    }
                }
            }
            // Wrong commit or dirty state - try to reset to correct commit
            if let Ok(commit) = repo.find_commit(target_oid) {
                tracing::info!("Resetting to {} (cached)", short_sha(&git.rev));
                repo.reset(commit.as_object(), ResetType::Hard, None)
                    .context("failed to reset cached repository to target commit")?;
                return Ok(());
            }
        }
        // Invalid or incompatible repo - remove and re-clone
        tracing::debug!("Removing stale source directory");
        std::fs::remove_dir_all(dest)?;
    }

    tracing::info!("Cloning {} at {}", git.url, short_sha(&git.rev));

    // Clone the repository
    std::fs::create_dir_all(dest)?;
    let repo = Repository::clone(&git.url, dest)
        .with_context(|| format!("failed to clone {}", git.url))?;

    // Checkout specific commit
    let commit = repo
        .find_commit(target_oid)
        .with_context(|| format!("commit {} not found", git.rev))?;
    repo.reset(commit.as_object(), ResetType::Hard, None)
        .context("failed to reset repository to target commit")?;

    Ok(())
}

/// Apply patches to the source.
pub(crate) fn apply_patches(
    ctx: &VerifyContext,
    source_dir: &Path,
    options: &VerifyOptions,
) -> Result<()> {
    let registry_path = options
        .registry_path
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("patches require local registry path"))?;

    let first_char = ctx.shim.package.name.chars().next().unwrap();
    let shim_dir = registry_path
        .join("index")
        .join(first_char.to_string())
        .join(&ctx.shim.package.name);

    for patch in &ctx.shim.patches {
        let patch_path = shim_dir.join(&patch.file);

        if !patch_path.exists() {
            bail!("patch file not found: {}", patch_path.display());
        }

        // Verify patch hash
        crate::sources::registry::shim::verify_patch_hash(&patch_path, &patch.sha256)?;

        // Apply patch using git apply
        let output = Command::new("git")
            .args(["apply", "--check"])
            .arg(&patch_path)
            .current_dir(source_dir)
            .output()
            .context("failed to run git apply --check")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("patch '{}' will not apply cleanly: {}", patch.file, stderr);
        }

        let output = Command::new("git")
            .arg("apply")
            .arg(&patch_path)
            .current_dir(source_dir)
            .output()
            .context("failed to run git apply")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("failed to apply patch '{}': {}", patch.file, stderr);
        }

        tracing::info!("Applied patch: {}", patch.file);
    }

    Ok(())
}
