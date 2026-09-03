//! Transport abstraction for registries.
//!
//! A transport answers exactly two questions:
//!
//! 1. Fetch the bytes at some path under the registry's `index/` tree (a
//!    tier-1 index file, or a tier-2 shim).
//! 2. Fetch the artifact a shim points at (clone the git source, or
//!    download+verify the tarball), returning where it landed on disk.
//!
//! `RegistrySource` (`mod.rs`) is transport-agnostic: it drives resolution
//! and package loading purely in terms of this trait, so a second
//! implementation (the sparse HTTP transport, `sparse+https://`) is a new
//! `impl RegistryTransport` with no changes to `RegistrySource` itself.
//!
//! [`GitTransport`] is the only implementation today. It answers both
//! questions by reading out of a local git clone - cloning/fetching it on
//! demand - which is exactly what `RegistrySource` used to do directly
//! before this abstraction existed. Porting it here changed nothing a user
//! can observe: same clone, same on-disk layout, same fetch semantics.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use git2::{Repository, ResetType};
use url::Url;

use super::shim::Shim;
use crate::util::fs::sanitize_url_for_path;

/// A transport a registry can be served over.
///
/// Every method here is either metadata-only (`ensure_ready`,
/// `fetch_index_path`) or fetches exactly one artifact
/// (`fetch_artifact`) - resolving a version range must never call
/// `fetch_artifact`, which is what makes metadata-only resolution possible
/// regardless of which transport is in play.
pub trait RegistryTransport {
    /// Make sure the transport is ready to serve reads: for git, this
    /// means the index is cloned or up to date. Called once per
    /// `RegistrySource` before any other method.
    fn ensure_ready(&mut self) -> Result<()>;

    /// Fetch the raw bytes at `relative_path` (relative to the registry's
    /// root, e.g. `"config.toml"`, `"index/z/zlib.idx"`, or
    /// `"index/z/zlib/1.3.1.toml"`).
    ///
    /// Returns `Ok(None)` if the path does not exist - a registry that
    /// simply doesn't have this package/version, not an error.
    fn fetch_index_path(&mut self, relative_path: &str) -> Result<Option<Vec<u8>>>;

    /// Fetch the artifact a shim describes (clone the git source or
    /// download+verify the tarball, applying any patches), returning the
    /// local directory it was materialized into.
    ///
    /// `shim_relative_path` is the shim's own path relative to the
    /// registry root (as carried in a tier-1 record's `shim` field,
    /// prefixed with `"index/"`); it is needed to resolve patch files,
    /// which are stored relative to the shim.
    fn fetch_artifact(&mut self, shim: &Shim, shim_relative_path: &str) -> Result<PathBuf>;

    /// Downcasting hook so callers that need transport-specific behavior
    /// (e.g. `RegistrySource::index_path`, used by CI verification tooling
    /// that walks a git clone directly) can recover the concrete type.
    /// Transports that have no such escape hatch to offer can still
    /// implement this trivially - it never changes the trait's core
    /// contract of "two questions."
    fn as_any(&self) -> &dyn std::any::Any;
}

/// The git transport: a registry served as a plain git repository.
///
/// Per the design this ports (`docs/superpowers/specs/2026-09-03-http-registry-design.md`),
/// a git registry's tier-1 index is **committed** - generated from the
/// shims by CI and checked for freshness there (see `generate_index` in
/// `index.rs`) - rather than computed by the client. `GitTransport` simply
/// reads whatever tier-1/tier-2 files are checked into the clone; it never
/// derives an index itself. That is what keeps development (against a git
/// clone) and production (against R2) resolving against byte-identical
/// data.
pub struct GitTransport {
    /// Registry git URL.
    registry_url: Url,

    /// Local path to the cloned registry index.
    index_path: PathBuf,

    /// Local path for fetched package sources (artifacts).
    src_cache_path: PathBuf,

    /// Whether the index has already been made ready this session.
    ready: bool,
}

impl GitTransport {
    /// Create a transport that will clone/fetch `registry_url` into
    /// `cache_dir` on first use.
    pub fn new(registry_url: Url, cache_dir: &Path) -> Self {
        let registry_dir_name = sanitize_url_for_path(&registry_url);

        let index_path = cache_dir.join("registry").join(&registry_dir_name);
        let src_cache_path = cache_dir.join("registry-src").join(&registry_dir_name);

        GitTransport {
            registry_url,
            index_path,
            src_cache_path,
            ready: false,
        }
    }

    /// Create a transport pointed directly at an already-checked-out
    /// registry directory (e.g. a CI checkout). No clone/fetch is ever
    /// performed.
    pub fn from_local_path(registry_path: &Path, cache_dir: &Path) -> Result<Self> {
        let registry_url = Url::from_file_path(registry_path)
            .map_err(|_| anyhow::anyhow!("invalid registry path: {}", registry_path.display()))?;

        Ok(GitTransport {
            registry_url,
            index_path: registry_path.to_path_buf(),
            src_cache_path: cache_dir.join("registry-src").join("local"),
            ready: true,
        })
    }

    /// The registry's URL.
    pub fn registry_url(&self) -> &Url {
        &self.registry_url
    }

    /// Local path of the cloned registry index.
    pub fn index_path(&self) -> &Path {
        &self.index_path
    }

    fn clone_index(&self) -> Result<()> {
        tracing::info!("Cloning registry index from {}", self.registry_url);

        std::fs::create_dir_all(self.index_path.parent().unwrap())?;

        Repository::clone(self.registry_url.as_str(), &self.index_path).with_context(|| {
            format!("failed to clone registry index from {}", self.registry_url)
        })?;

        Ok(())
    }

    fn update_index(&self) -> Result<()> {
        tracing::info!("Updating registry index from {}", self.registry_url);

        let repo =
            Repository::open(&self.index_path).with_context(|| "failed to open registry index")?;

        let mut remote = repo.find_remote("origin")?;
        remote.fetch(&["refs/heads/*:refs/heads/*"], None, None)?;

        let head = repo.head()?;
        let commit = head.peel_to_commit()?;
        repo.reset(commit.as_object(), ResetType::Hard, None)?;

        Ok(())
    }

    fn get_source_cache_path(&self, shim: &Shim) -> PathBuf {
        let source_hash = shim.source_hash();
        self.src_cache_path
            .join(&shim.package.name)
            .join(&shim.package.version)
            .join(source_hash)
    }

    fn fetch_git_source(&self, git: &super::shim::GitSource, dest: &Path) -> Result<()> {
        tracing::info!("Fetching git source from {} at {}", git.url, &git.rev[..8]);

        let repo = Repository::clone(&git.url, dest)
            .with_context(|| format!("failed to clone {}", git.url))?;

        let oid = git2::Oid::from_str(&git.rev)?;
        let commit = repo.find_commit(oid)?;
        repo.reset(commit.as_object(), ResetType::Hard, None)?;

        Ok(())
    }

    fn fetch_tarball_source(
        &self,
        tarball: &super::shim::TarballSource,
        dest: &Path,
    ) -> Result<()> {
        tracing::info!("Fetching tarball from {}", tarball.url);

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

        super::extract_tarball(&tarball_bytes, dest, tarball.strip_prefix.as_deref())
            .with_context(|| format!("failed to extract tarball from {}", tarball.url))?;

        Ok(())
    }

    fn apply_patches(
        &self,
        shim: &Shim,
        shim_relative_path: &str,
        source_dir: &Path,
    ) -> Result<()> {
        if !shim.is_git() {
            bail!("patches can only be applied to git sources, not tarballs");
        }

        let shim_dir = self
            .index_path
            .join(shim_relative_path)
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.index_path.clone());

        for patch in &shim.patches {
            let patch_path = shim_dir.join(&patch.file);

            if !patch_path.exists() {
                bail!(
                    "patch file not found: {} (expected at {})",
                    patch.file,
                    patch_path.display()
                );
            }

            super::shim::verify_patch_hash(&patch_path, &patch.sha256)?;
            self.apply_single_patch(&patch_path, source_dir)?;
        }

        Ok(())
    }

    fn apply_single_patch(&self, patch_path: &Path, source_dir: &Path) -> Result<()> {
        tracing::info!("Applying patch: {}", patch_path.display());

        let check_output = std::process::Command::new("git")
            .args(["apply", "--check"])
            .arg(patch_path)
            .current_dir(source_dir)
            .output()
            .with_context(|| "failed to run git apply --check")?;

        if !check_output.status.success() {
            let stderr = String::from_utf8_lossy(&check_output.stderr);
            bail!(
                "patch '{}' will not apply cleanly:\n{}",
                patch_path.display(),
                stderr
            );
        }

        let apply_output = std::process::Command::new("git")
            .arg("apply")
            .arg(patch_path)
            .current_dir(source_dir)
            .output()
            .with_context(|| "failed to run git apply")?;

        if !apply_output.status.success() {
            let stderr = String::from_utf8_lossy(&apply_output.stderr);
            bail!(
                "failed to apply patch '{}':\n{}",
                patch_path.display(),
                stderr
            );
        }

        Ok(())
    }
}

impl RegistryTransport for GitTransport {
    fn ensure_ready(&mut self) -> Result<()> {
        if self.ready {
            return Ok(());
        }

        if self.index_path.exists() {
            self.update_index()?;
        } else {
            self.clone_index()?;
        }

        self.ready = true;
        Ok(())
    }

    fn fetch_index_path(&mut self, relative_path: &str) -> Result<Option<Vec<u8>>> {
        let full_path = self.index_path.join(relative_path);

        match std::fs::read(&full_path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err).with_context(|| {
                format!("failed to read registry index file {}", full_path.display())
            }),
        }
    }

    fn fetch_artifact(&mut self, shim: &Shim, shim_relative_path: &str) -> Result<PathBuf> {
        let source_dir = self.get_source_cache_path(shim);

        if source_dir.exists() {
            if shim.is_git() && source_dir.join(".git").exists() {
                return Ok(source_dir);
            } else if shim.is_tarball() {
                if let Ok(mut entries) = std::fs::read_dir(&source_dir) {
                    if entries.next().is_some() {
                        return Ok(source_dir);
                    }
                }
            }
        }

        std::fs::create_dir_all(&source_dir)?;

        if let Some(git) = &shim.source.git {
            self.fetch_git_source(git, &source_dir)?;
        } else if let Some(tarball) = &shim.source.tarball {
            self.fetch_tarball_source(tarball, &source_dir)?;
        } else {
            bail!("shim has no source specified");
        }

        if !shim.patches.is_empty() {
            self.apply_patches(shim, shim_relative_path, &source_dir)?;
        }

        Ok(source_dir)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn new_transport_paths_are_namespaced_by_registry() {
        let tmp = TempDir::new().unwrap();
        let url = Url::parse("https://github.com/harbour-project/registry").unwrap();
        let transport = GitTransport::new(url, tmp.path());

        assert!(transport.index_path.to_string_lossy().contains("registry"));
        assert!(transport
            .src_cache_path
            .to_string_lossy()
            .contains("registry-src"));
    }

    #[test]
    fn local_path_transport_is_ready_without_cloning() {
        let tmp = TempDir::new().unwrap();
        let registry_dir = tmp.path().join("reg");
        std::fs::create_dir_all(&registry_dir).unwrap();

        let mut transport = GitTransport::from_local_path(&registry_dir, tmp.path()).unwrap();
        assert!(transport.ready);
        // ensure_ready must be a no-op (no network/clone attempted).
        transport.ensure_ready().unwrap();
    }

    #[test]
    fn fetch_index_path_returns_none_for_missing_file() {
        let tmp = TempDir::new().unwrap();
        let registry_dir = tmp.path().join("reg");
        std::fs::create_dir_all(&registry_dir).unwrap();

        let mut transport = GitTransport::from_local_path(&registry_dir, tmp.path()).unwrap();
        assert!(transport
            .fetch_index_path("index/z/zlib.idx")
            .unwrap()
            .is_none());
    }

    #[test]
    fn fetch_index_path_reads_committed_bytes() {
        let tmp = TempDir::new().unwrap();
        let registry_dir = tmp.path().join("reg");
        let idx_dir = registry_dir.join("index").join("z");
        std::fs::create_dir_all(&idx_dir).unwrap();
        std::fs::write(idx_dir.join("zlib.idx"), b"hello").unwrap();

        let mut transport = GitTransport::from_local_path(&registry_dir, tmp.path()).unwrap();
        let bytes = transport
            .fetch_index_path("index/z/zlib.idx")
            .unwrap()
            .unwrap();
        assert_eq!(bytes, b"hello");
    }
}
