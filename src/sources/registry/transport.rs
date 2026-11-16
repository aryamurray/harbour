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
use std::time::{Duration, SystemTime};

use anyhow::{bail, Context, Result};
use url::Url;

/// How long a fetched index is trusted for resolution before it is fetched
/// again (`harbour add`, `harbour search`, a re-resolve). Long enough that a
/// burst of commands fetches once, short enough that a newly published
/// version shows up without asking; a miss also fetches (see
/// `RegistrySource::query`).
pub const DEFAULT_INDEX_MAX_AGE: Duration = Duration::from_secs(5 * 60);

/// How recent a transport's local index must be before it is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    /// Any local copy will do; the network is used only if there is none.
    /// For reading what a lockfile already pinned: a published version's
    /// shim does not change, so the copy that resolved it can still load it.
    Local,
    /// Fetch unless the local copy was fetched within this long.
    Within(Duration),
    /// Fetch, unless that already happened in this process.
    Latest,
}

use super::shim::Shim;
use crate::sources::git_cli;
use crate::util::fs::sanitize_url_for_path;
use crate::util::redact::redact_url;

/// A transport a registry can be served over.
///
/// Every method here is either metadata-only (`ensure_ready`,
/// `fetch_index_path`) or fetches exactly one artifact
/// (`fetch_artifact`) - resolving a version range must never call
/// `fetch_artifact`, which is what makes metadata-only resolution possible
/// regardless of which transport is in play.
pub trait RegistryTransport {
    /// Make sure the transport can serve reads at least as fresh as
    /// `freshness`: for git, the index is cloned, and fetched if the local
    /// copy is older than asked. Cheap to call repeatedly.
    fn ensure_ready(&mut self, freshness: Freshness) -> Result<()>;

    /// Whether the index is already as new as the remote can make it in this
    /// process (it was fetched here, or it is a local checkout that nothing
    /// fetches). A caller that finds nothing in an index that is not latest
    /// can fetch with [`Freshness::Latest`] and look again.
    fn is_latest(&self) -> bool;

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

    /// Tell the transport which platform builds are for.
    ///
    /// Only a shim with per-platform archives (`[[source.archive]]`) cares:
    /// it is how [`fetch_artifact`](Self::fetch_artifact) knows which one to
    /// download. `None` means "whatever a build with no `--target-triple`
    /// would target" (the configured `toolchain.target`, else the host). A
    /// transport that serves nothing per platform ignores it.
    fn set_target(&mut self, _target: Option<crate::core::target::TargetTriple>) {}
}

/// The git transport: a registry served as a plain git repository.
///
/// By design, a git registry's tier-1 index is **committed** - generated from the
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

    /// Whether the index was cloned or fetched in this process (or is a
    /// local checkout, which is never fetched). Once set, nothing fetches
    /// again.
    fetched: bool,

    /// The platform builds are for, when the command named one. See
    /// [`RegistryTransport::set_target`].
    target: Option<crate::core::target::TargetTriple>,
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
            fetched: false,
            target: None,
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
            fetched: true,
            target: None,
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
        tracing::info!(
            "Cloning registry index from {}",
            redact_url(self.registry_url.as_str())
        );

        std::fs::create_dir_all(self.index_path.parent().unwrap())?;

        // Only the current tree is ever read, so history is not fetched.
        git_cli::clone_repo(self.registry_url.as_str(), &self.index_path, true, true)?;

        Ok(())
    }

    fn update_index(&self) -> Result<()> {
        tracing::info!(
            "Updating registry index from {}",
            redact_url(self.registry_url.as_str())
        );

        let tip = git_cli::fetch_head(&self.index_path, self.registry_url.as_str(), true)?;
        git_cli::checkout_exact(&self.index_path, &tip)
            .with_context(|| "failed to update registry index")?;

        Ok(())
    }

    /// The file whose modification time records the last clone or fetch.
    ///
    /// Inside `.git`, which git leaves alone and a checkout never touches, so
    /// it cannot show up as a change to the index. A process that finds it
    /// recent knows another one (or an earlier command) just fetched.
    fn fetch_stamp(&self) -> PathBuf {
        self.index_path.join(".git").join("harbour-last-fetch")
    }

    fn record_fetch(&self) {
        // Best effort: without the stamp the next command just fetches again.
        if let Err(e) = std::fs::write(self.fetch_stamp(), b"") {
            tracing::debug!("could not record registry fetch time: {e}");
        }
    }

    /// How long ago the index was last fetched, if that is known.
    fn since_last_fetch(&self) -> Option<Duration> {
        let modified = std::fs::metadata(self.fetch_stamp())
            .ok()?
            .modified()
            .ok()?;
        // A stamp in the future (clock change) says nothing; treat it as stale.
        SystemTime::now().duration_since(modified).ok()
    }

    /// The platform an archive is chosen for, and how to name it in a message.
    fn target_platform(&self) -> (crate::core::surface::TargetPlatform, String) {
        let triple = crate::builder::toolchain::resolve_target(self.target.as_ref());
        (
            crate::core::surface::TargetPlatform::for_target(&triple),
            triple.to_string(),
        )
    }

    /// Where a shim's fetched, patched source tree is kept.
    ///
    /// `<name>/<version>/<hash>`. For a git or tarball shim the hash is the
    /// shim's source hash. For a per-platform shim it is the hash of the one
    /// archive being used (and the patches), so the trees of two platforms
    /// never share a directory -- which matters for a cache shared between
    /// machines, and for cross builds on one.
    fn get_source_cache_path(&self, shim: &Shim) -> Result<PathBuf> {
        let key = if shim.is_archives() {
            let (platform, triple) = self.target_platform();
            shim.archive_cache_key(shim.select_archive(&platform, &triple)?)
        } else {
            shim.source_hash()
        };
        Ok(self
            .src_cache_path
            .join(&shim.package.name)
            .join(&shim.package.version)
            .join(key))
    }

    fn fetch_git_source(&self, git: &super::shim::GitSource, dest: &Path) -> Result<()> {
        tracing::info!(
            "Fetching git source from {} at {}",
            redact_url(&git.url),
            &git.rev[..8]
        );

        git_cli::validate_commit_id(&git.rev)?;
        git_cli::fetch_rev(&git.url, dest, &git.rev)?;
        git_cli::checkout_exact(dest, &git.rev)?;

        Ok(())
    }

    /// Fetch, verify and unpack one archive into `dest`.
    ///
    /// Shared by `[source.tarball]` and `[[source.archive]]`: the archive is
    /// streamed to disk and hashed on the way (see
    /// [`archive::fetch_archive`](super::archive::fetch_archive)), compared
    /// with the pinned hash, and only then unpacked.
    fn fetch_and_unpack(
        &self,
        what: &str,
        label: &str,
        url: &str,
        sha256: &str,
        format: super::archive::ArchiveFormat,
        strip_prefix: Option<&str>,
        dest: &Path,
    ) -> Result<()> {
        tracing::info!("Fetching {what} from {url}");

        // Staged beside the destination, so the download and the unpacked
        // tree are on one volume and nothing lands in a small `/tmp`.
        let staging = dest.parent().unwrap_or(dest);
        let fetched = super::archive::fetch_archive(url, label, staging)?;

        if !fetched.sha256.eq_ignore_ascii_case(sha256) {
            bail!(
                "{what} hash mismatch for {url}:\n  expected: {sha256}\n  actual:   {}",
                fetched.sha256
            );
        }

        tracing::debug!(
            "{what} hash verified: {} ({})",
            &fetched.sha256[..16],
            super::archive::human_size(fetched.size)
        );

        super::archive::extract_archive(fetched.path(), format, dest, strip_prefix)
            .with_context(|| format!("failed to extract {what} from {url}"))?;

        Ok(())
    }

    /// Fetch a `[source.tarball]`.
    ///
    /// A URL that does not say what it is (`.../archive/v1.2.3`) is taken to
    /// be a gzip-compressed tarball, which is what this source always was.
    fn fetch_tarball_source(
        &self,
        shim: &Shim,
        tarball: &super::shim::TarballSource,
        dest: &Path,
    ) -> Result<()> {
        let format = super::archive::ArchiveFormat::from_file_name(&tarball.url)
            .unwrap_or(super::archive::ArchiveFormat::TarGz);
        self.fetch_and_unpack(
            "tarball",
            &format!("{} {}", shim.package.name, shim.package.version),
            &tarball.url,
            &tarball.sha256,
            format,
            tarball.strip_prefix.as_deref(),
            dest,
        )
    }

    /// Fetch the one `[[source.archive]]` for the platform being built for.
    /// The others are never touched.
    fn fetch_archive_source(&self, shim: &Shim, dest: &Path) -> Result<()> {
        let (platform, triple) = self.target_platform();
        let archive = shim.select_archive(&platform, &triple)?;
        self.fetch_and_unpack(
            "archive",
            &format!(
                "{} {} for {triple}",
                shim.package.name, shim.package.version
            ),
            &archive.url,
            &archive.sha256,
            archive.archive_format()?,
            archive.strip_prefix.as_deref(),
            dest,
        )
    }

    /// The manifest a per-platform shim's patches create, without any archive.
    ///
    /// `harbour registry index` needs each package's dependencies, which are
    /// in its `Harbour.toml`; for a prebuilt package that file is added by a
    /// patch, since upstream's archive has none. Applying the patches to an
    /// empty directory therefore yields the manifest without downloading
    /// gigabytes, and without needing an archive for the machine doing the
    /// indexing. Returns `None` when that does not work (a patch that edits
    /// one of upstream's files does not apply to nothing), and the caller
    /// falls back to fetching.
    pub fn manifest_preview(&self, shim: &Shim, shim_relative_path: &str) -> Option<PathBuf> {
        if !shim.is_archives() || shim.patches.is_empty() {
            return None;
        }
        let dir = self
            .src_cache_path
            .join(&shim.package.name)
            .join(&shim.package.version)
            .join(format!("manifest-{}", shim.source_hash()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).ok()?;
        match self.apply_patches(shim, shim_relative_path, &dir) {
            Ok(()) if crate::core::workspace::find_manifest(&dir).is_ok() => Some(dir),
            _ => {
                let _ = std::fs::remove_dir_all(&dir);
                None
            }
        }
    }

    fn apply_patches(
        &self,
        shim: &Shim,
        shim_relative_path: &str,
        source_dir: &Path,
    ) -> Result<()> {
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

        // git runs in `source_dir`, so a patch path relative to *our* working
        // directory names a different file there (or none: on Windows the
        // doubled-up path overflowed 260 characters and git reported
        // "Filename too long"). Whatever the cache directory was given as, git
        // gets an absolute path.
        let patch_path = &std::path::absolute(patch_path)
            .with_context(|| format!("failed to resolve patch path {}", patch_path.display()))?;

        // `git_cli::apply_patch` stops repository discovery at `source_dir`'s
        // parent, so a cache inside some other repository (a project's own
        // `.harbour`, a home directory under version control) does not make git
        // resolve the patch's paths against that repository's root.
        if let Some(stderr) = git_cli::apply_patch(source_dir, patch_path, true)? {
            bail!(
                "patch '{}' will not apply cleanly:\n{}",
                patch_path.display(),
                stderr
            );
        }

        if let Some(stderr) = git_cli::apply_patch(source_dir, patch_path, false)? {
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
    fn ensure_ready(&mut self, freshness: Freshness) -> Result<()> {
        if self.fetched {
            return Ok(());
        }

        if !self.index_path.exists() {
            self.clone_index()?;
        } else {
            let stale = match freshness {
                Freshness::Local => false,
                Freshness::Latest => true,
                Freshness::Within(max_age) => {
                    self.since_last_fetch().is_none_or(|age| age > max_age)
                }
            };
            if !stale {
                return Ok(());
            }
            self.update_index()?;
        }

        self.record_fetch();
        self.fetched = true;
        Ok(())
    }

    fn is_latest(&self) -> bool {
        self.fetched
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
        let source_dir = self.get_source_cache_path(shim)?;

        // A per-platform archive is large enough that being interrupted while
        // unpacking it is an ordinary event, and a half-unpacked tree looks
        // like a cache hit to "the directory has something in it". So such a
        // tree counts only once a marker beside it says it was finished.
        let complete_marker = source_dir.with_extension("complete");

        if source_dir.exists() {
            if shim.is_git() && source_dir.join(".git").exists() {
                return Ok(source_dir);
            } else if shim.is_archives() {
                if complete_marker.exists() {
                    return Ok(source_dir);
                }
                tracing::debug!(
                    "{} was left unfinished by an earlier run; fetching it again",
                    source_dir.display()
                );
                std::fs::remove_dir_all(&source_dir)?;
            } else if shim.is_tarball() {
                if let Ok(mut entries) = std::fs::read_dir(&source_dir) {
                    if entries.next().is_some() {
                        return Ok(source_dir);
                    }
                }
            }
        }

        std::fs::create_dir_all(&source_dir)?;

        // Fetch and patch into the cache directory, and take the directory away
        // again if either fails. The cache hit above is "the directory exists
        // and has a `.git`", so a clone whose patch failed to apply used to be
        // served on the next call -- unpatched, with the real error long gone and
        // a later, misleading one ("no Harbour.toml") in its place.
        let prepared = (|| -> Result<()> {
            if let Some(git) = &shim.source.git {
                self.fetch_git_source(git, &source_dir)?;
            } else if let Some(tarball) = &shim.source.tarball {
                self.fetch_tarball_source(shim, tarball, &source_dir)?;
            } else if shim.is_archives() {
                self.fetch_archive_source(shim, &source_dir)?;
            } else {
                bail!("shim has no source specified");
            }

            if !shim.patches.is_empty() {
                self.apply_patches(shim, shim_relative_path, &source_dir)?;
            }
            Ok(())
        })();
        if let Err(err) = prepared {
            let _ = std::fs::remove_dir_all(&source_dir);
            return Err(err);
        }
        if shim.is_archives() {
            std::fs::write(&complete_marker, b"")
                .with_context(|| format!("failed to write {}", complete_marker.display()))?;
        }

        Ok(source_dir)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn set_target(&mut self, target: Option<crate::core::target::TargetTriple>) {
        self.target = target;
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

    /// The cache directory can reach here as a relative path (the fallback when
    /// no home directory can be found used to be a bare `.harbour`). git runs in
    /// the source directory, where that relative path names nothing.
    #[test]
    fn a_patch_given_by_relative_path_is_applied() {
        // Under the working directory on purpose, so the relative path has no
        // `..` in it. A path that climbs to the filesystem root resolves from
        // any directory, and would pass without the fix.
        let cwd = std::env::current_dir().unwrap();
        let tmp = tempfile::Builder::new()
            .prefix(".harbour-relative-patch-")
            .tempdir_in(&cwd)
            .unwrap();
        let source_dir = tmp.path().join("src");
        std::fs::create_dir_all(&source_dir).unwrap();
        let patch = tmp.path().join("add.patch");
        std::fs::write(
            &patch,
            "diff --git a/new.txt b/new.txt\nnew file mode 100644\n--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1 @@\n+hello\n",
        )
        .unwrap();
        let relative = patch.strip_prefix(&cwd).unwrap().to_path_buf();
        assert!(relative.is_relative());

        let url = Url::parse("https://example.invalid/registry").unwrap();
        let transport = GitTransport::new(url, tmp.path());
        transport
            .apply_single_patch(&relative, &source_dir)
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(source_dir.join("new.txt"))
                .unwrap()
                .trim_end(),
            "hello"
        );
    }

    #[test]
    fn local_path_transport_is_ready_without_cloning() {
        let tmp = TempDir::new().unwrap();
        let registry_dir = tmp.path().join("reg");
        std::fs::create_dir_all(&registry_dir).unwrap();

        let mut transport = GitTransport::from_local_path(&registry_dir, tmp.path()).unwrap();
        assert!(transport.is_latest());
        // ensure_ready must be a no-op (no network/clone attempted).
        transport.ensure_ready(Freshness::Latest).unwrap();
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

    /// Tarball fetching is the only TLS user in Harbour, and it has no
    /// automated coverage -- every registry test runs against a local
    /// fixture. That mattered when reqwest 0.13 changed the default TLS
    /// backend from native-tls to rustls with aws-lc-rs: nothing in the
    /// suite would have noticed a TLS stack that built but could not
    /// negotiate a connection or verify a certificate.
    ///
    /// Ignored because it needs the network. Run it after any change to the
    /// TLS backend or its features:
    ///
    /// ```text
    /// cargo test --all-features tls_stack_can_complete_an_https_request -- --ignored
    /// ```
    #[test]
    #[ignore = "requires network access"]
    fn tls_stack_can_complete_an_https_request() {
        // Receiving *any* HTTP status is the proof that matters here: it
        // means the handshake completed and the certificate verified. The
        // status itself is the server's business, not the TLS stack's.
        let response = reqwest::blocking::get("https://example.com")
            .expect("HTTPS request must complete: a TLS backend that builds is not enough");
        assert!(
            response.status().is_success(),
            "unexpected status: {}",
            response.status()
        );
    }
}
