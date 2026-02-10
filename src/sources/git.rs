//! Git source - dependencies from git repositories.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use git2::{Repository, ResetType};
use url::Url;

use crate::core::source_id::GitReference;
use crate::core::workspace::find_manifest;
use crate::core::{Dependency, Package, PackageId, SourceId, Summary};
use crate::sources::Source;
use crate::util::hash::sha256_str;

/// A source for git dependencies.
pub struct GitSource {
    /// Remote repository URL
    remote: Url,

    /// Git reference (branch, tag, rev)
    reference: GitReference,

    /// Local checkout path
    checkout_path: PathBuf,

    /// Source ID
    source_id: SourceId,

    /// Cached package
    package: Option<Package>,

    /// Resolved commit hash
    precise: Option<String>,
}

impl GitSource {
    /// Create a new git source.
    pub fn new(
        remote: Url,
        reference: GitReference,
        cache_dir: &Path,
        source_id: SourceId,
    ) -> Self {
        // Create a unique directory name for this repo + reference
        let dir_name = format!(
            "{}-{}",
            sanitize_url_for_path(&remote),
            sha256_str(&format!("{:?}", reference))[..8].to_string()
        );

        let checkout_path = cache_dir.join("git").join(dir_name);

        GitSource {
            remote,
            reference,
            checkout_path,
            source_id,
            package: None,
            precise: None,
        }
    }

    /// Clone or update the repository.
    fn fetch(&mut self) -> Result<()> {
        if self.checkout_path.exists() {
            // Update existing checkout
            self.update()?;
        } else {
            // Clone fresh
            self.clone()?;
        }

        // Checkout the right reference
        self.checkout()?;

        Ok(())
    }

    fn clone(&self) -> Result<()> {
        tracing::info!("Cloning {}", self.remote);

        std::fs::create_dir_all(self.checkout_path.parent().unwrap())?;

        Repository::clone(self.remote.as_str(), &self.checkout_path)
            .with_context(|| format!("failed to clone {}", self.remote))?;

        Ok(())
    }

    fn update(&self) -> Result<()> {
        tracing::info!("Updating {}", self.remote);

        let repo = Repository::open(&self.checkout_path)
            .with_context(|| "failed to open git repository")?;

        // Fetch from remote
        let mut remote = repo.find_remote("origin")?;
        remote.fetch(&["refs/heads/*:refs/heads/*"], None, None)?;

        Ok(())
    }

    fn checkout(&mut self) -> Result<()> {
        let repo = Repository::open(&self.checkout_path)?;

        let commit = match &self.reference {
            GitReference::DefaultBranch => {
                // Find default branch
                let head = repo.head()?;
                head.peel_to_commit()?
            }
            GitReference::Branch(branch) => {
                let branch_ref = repo.find_branch(branch, git2::BranchType::Local)?;
                branch_ref.get().peel_to_commit()?
            }
            GitReference::Tag(tag) => {
                let tag_ref = repo.find_reference(&format!("refs/tags/{}", tag))?;
                tag_ref.peel_to_commit()?
            }
            GitReference::Rev(rev) => {
                let oid = git2::Oid::from_str(rev)?;
                repo.find_commit(oid)?
            }
        };

        // Store the precise commit hash
        self.precise = Some(commit.id().to_string());

        // Hard reset to the commit
        repo.reset(commit.as_object(), ResetType::Hard, None)?;

        Ok(())
    }

    /// Load the package from the checkout.
    fn load(&mut self) -> Result<&Package> {
        if self.package.is_none() {
            let manifest_path = find_manifest(&self.checkout_path).map_err(|err| {
                anyhow::anyhow!("{}\n  while loading git repository: {}", err, self.remote)
            })?;

            // Create package with precise source ID
            let precise_source = if let Some(ref precise) = self.precise {
                self.source_id.with_precise(precise)
            } else {
                self.source_id
            };

            let manifest = crate::core::Manifest::load(&manifest_path)?;
            let package =
                Package::with_source_id(manifest, self.checkout_path.clone(), precise_source)?;

            self.package = Some(package);
        }

        Ok(self.package.as_ref().unwrap())
    }

    /// Get the resolved commit hash.
    pub fn precise(&self) -> Option<&str> {
        self.precise.as_deref()
    }
}

impl Source for GitSource {
    fn name(&self) -> &str {
        "git"
    }

    fn supports(&self, dep: &Dependency) -> bool {
        // Check if the source IDs match (ignoring precise)
        dep.source_id().url() == self.source_id.url()
    }

    fn query(&mut self, dep: &Dependency) -> Result<Vec<Summary>> {
        if !self.supports(dep) {
            return Ok(vec![]);
        }

        // Ensure we have the repo
        self.fetch()?;

        let package = self.load()?;

        // Check if the package name matches
        if package.name() != dep.name() {
            return Ok(vec![]);
        }

        // Check if version matches (if specified)
        if !dep.matches_version(package.version()) {
            return Ok(vec![]);
        }

        let summary = package.summary()?;
        Ok(vec![summary])
    }

    fn ensure_ready(&mut self) -> Result<()> {
        self.fetch()
    }

    fn get_package_path(&self, _pkg_id: PackageId) -> Result<&Path> {
        Ok(&self.checkout_path)
    }

    fn load_package(&mut self, pkg_id: PackageId) -> Result<Package> {
        self.fetch()?;
        let package = self.load()?;

        // We need to check name and version, but source ID might differ
        // due to precise commit hash
        if package.name() != pkg_id.name() || package.version() != pkg_id.version() {
            bail!(
                "package mismatch: expected {} {}, found {} {}",
                pkg_id.name(),
                pkg_id.version(),
                package.name(),
                package.version()
            );
        }

        Ok(package.clone())
    }

    fn is_cached(&self, _pkg_id: PackageId) -> bool {
        self.checkout_path.exists() && find_manifest(&self.checkout_path).is_ok()
    }
}

/// Sanitize a URL for use as a directory name.
fn sanitize_url_for_path(url: &Url) -> String {
    let mut name = String::new();

    if let Some(host) = url.host_str() {
        name.push_str(host);
    }

    let path = url.path().trim_matches('/');
    if !path.is_empty() {
        name.push('-');
        name.push_str(&path.replace('/', "-"));
    }

    // Remove .git suffix
    if name.ends_with(".git") {
        name.truncate(name.len() - 4);
    }

    name
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_sanitize_url() {
        let url = Url::parse("https://github.com/user/repo.git").unwrap();
        assert_eq!(sanitize_url_for_path(&url), "github.com-user-repo");

        let url2 = Url::parse("https://gitlab.com/org/project").unwrap();
        assert_eq!(sanitize_url_for_path(&url2), "gitlab.com-org-project");
    }

    #[test]
    fn test_sanitize_url_deep_path() {
        let url = Url::parse("https://github.com/org/team/project/repo.git").unwrap();
        let result = sanitize_url_for_path(&url);
        assert_eq!(result, "github.com-org-team-project-repo");
    }

    #[test]
    fn test_sanitize_url_no_path() {
        let url = Url::parse("https://example.com").unwrap();
        let result = sanitize_url_for_path(&url);
        assert_eq!(result, "example.com");
    }

    #[test]
    fn test_sanitize_url_trailing_slashes() {
        let url = Url::parse("https://github.com/user/repo/").unwrap();
        let result = sanitize_url_for_path(&url);
        assert_eq!(result, "github.com-user-repo");
    }

    #[test]
    fn test_sanitize_url_without_git_suffix() {
        let url = Url::parse("https://github.com/user/repo").unwrap();
        assert_eq!(sanitize_url_for_path(&url), "github.com-user-repo");
    }

    #[test]
    fn test_sanitize_url_ssh_style() {
        // SSH URLs are typically converted to https style
        let url = Url::parse("https://gitlab.com/myorg/myproject.git").unwrap();
        assert_eq!(sanitize_url_for_path(&url), "gitlab.com-myorg-myproject");
    }

    #[test]
    fn test_git_reference_default() {
        let reference = GitReference::default();
        assert_eq!(reference, GitReference::DefaultBranch);
    }

    #[test]
    fn test_git_reference_branch() {
        let reference = GitReference::Branch("main".to_string());
        match reference {
            GitReference::Branch(b) => assert_eq!(b, "main"),
            _ => panic!("Expected Branch variant"),
        }
    }

    #[test]
    fn test_git_reference_tag() {
        let reference = GitReference::Tag("v1.0.0".to_string());
        match reference {
            GitReference::Tag(t) => assert_eq!(t, "v1.0.0"),
            _ => panic!("Expected Tag variant"),
        }
    }

    #[test]
    fn test_git_reference_rev() {
        let reference = GitReference::Rev("abc123def456789012345678901234567890abcd".to_string());
        match reference {
            GitReference::Rev(r) => {
                assert_eq!(r.len(), 40);
                assert!(r.starts_with("abc123"));
            }
            _ => panic!("Expected Rev variant"),
        }
    }

    #[test]
    fn test_git_source_creation() {
        let cache_dir = TempDir::new().unwrap();
        let url = Url::parse("https://github.com/user/repo.git").unwrap();
        let reference = GitReference::Tag("v1.0.0".to_string());
        let source_id = SourceId::for_git(&url, reference.clone()).unwrap();

        let git_source = GitSource::new(url.clone(), reference, cache_dir.path(), source_id);

        assert_eq!(git_source.name(), "git");
        assert!(git_source.precise().is_none()); // Not resolved yet
    }

    #[test]
    fn test_git_source_checkout_path() {
        let cache_dir = TempDir::new().unwrap();
        let url = Url::parse("https://github.com/user/repo.git").unwrap();
        let reference = GitReference::Branch("main".to_string());
        let source_id = SourceId::for_git(&url, reference.clone()).unwrap();

        let git_source = GitSource::new(url, reference, cache_dir.path(), source_id);

        // The checkout path should be in the cache directory
        let checkout_path = &git_source.checkout_path;
        assert!(checkout_path.starts_with(cache_dir.path()));
        assert!(checkout_path.to_string_lossy().contains("git"));
    }

    #[test]
    fn test_git_source_is_cached_false() {
        let cache_dir = TempDir::new().unwrap();
        let url = Url::parse("https://github.com/user/repo.git").unwrap();
        let reference = GitReference::DefaultBranch;
        let source_id = SourceId::for_git(&url, reference.clone()).unwrap();

        let git_source = GitSource::new(url, reference, cache_dir.path(), source_id);

        // Dummy package ID for is_cached check
        let tmp = TempDir::new().unwrap();
        let pkg_source = SourceId::for_path(tmp.path()).unwrap();
        let pkg_id = crate::core::PackageId::new("repo", semver::Version::new(1, 0, 0), pkg_source);

        // Should not be cached initially
        assert!(!git_source.is_cached(pkg_id));
    }

    #[test]
    fn test_git_source_different_refs_different_paths() {
        let cache_dir = TempDir::new().unwrap();
        let url = Url::parse("https://github.com/user/repo.git").unwrap();

        let source_id1 = SourceId::for_git(&url, GitReference::Branch("main".to_string())).unwrap();
        let source_id2 = SourceId::for_git(&url, GitReference::Tag("v1.0.0".to_string())).unwrap();

        let git_source1 = GitSource::new(
            url.clone(),
            GitReference::Branch("main".to_string()),
            cache_dir.path(),
            source_id1,
        );
        let git_source2 = GitSource::new(
            url,
            GitReference::Tag("v1.0.0".to_string()),
            cache_dir.path(),
            source_id2,
        );

        // Different references should result in different checkout paths
        assert_ne!(git_source1.checkout_path, git_source2.checkout_path);
    }

    #[test]
    fn test_git_source_name() {
        let cache_dir = TempDir::new().unwrap();
        let url = Url::parse("https://github.com/user/repo.git").unwrap();
        let reference = GitReference::DefaultBranch;
        let source_id = SourceId::for_git(&url, reference.clone()).unwrap();

        let git_source = GitSource::new(url, reference, cache_dir.path(), source_id);

        assert_eq!(git_source.name(), "git");
    }

    #[test]
    fn test_git_reference_equality() {
        assert_eq!(GitReference::DefaultBranch, GitReference::DefaultBranch);
        assert_eq!(
            GitReference::Branch("main".to_string()),
            GitReference::Branch("main".to_string())
        );
        assert_ne!(
            GitReference::Branch("main".to_string()),
            GitReference::Branch("develop".to_string())
        );
        assert_ne!(
            GitReference::DefaultBranch,
            GitReference::Tag("v1".to_string())
        );
    }

    #[test]
    fn test_sanitize_url_various_hosts() {
        let urls = vec![
            ("https://github.com/user/repo.git", "github.com-user-repo"),
            ("https://gitlab.com/user/repo.git", "gitlab.com-user-repo"),
            (
                "https://bitbucket.org/user/repo.git",
                "bitbucket.org-user-repo",
            ),
            (
                "https://codeberg.org/user/repo.git",
                "codeberg.org-user-repo",
            ),
            (
                "https://git.example.com/path/to/repo.git",
                "git.example.com-path-to-repo",
            ),
        ];

        for (url_str, expected) in urls {
            let url = Url::parse(url_str).unwrap();
            assert_eq!(
                sanitize_url_for_path(&url),
                expected,
                "Failed for URL: {}",
                url_str
            );
        }
    }

    #[test]
    fn test_git_source_supports_matching_source() {
        let cache_dir = TempDir::new().unwrap();
        let url = Url::parse("https://github.com/user/repo.git").unwrap();
        let reference = GitReference::DefaultBranch;
        let source_id = SourceId::for_git(&url, reference.clone()).unwrap();

        let git_source = GitSource::new(url.clone(), reference, cache_dir.path(), source_id);

        // Create a matching dependency
        let dep = crate::core::Dependency::new("repo", source_id);

        assert!(git_source.supports(&dep));
    }

    #[test]
    fn test_git_source_supports_different_source() {
        let cache_dir = TempDir::new().unwrap();
        let url1 = Url::parse("https://github.com/user/repo.git").unwrap();
        let url2 = Url::parse("https://github.com/other/repo.git").unwrap();
        let reference = GitReference::DefaultBranch;
        let source_id1 = SourceId::for_git(&url1, reference.clone()).unwrap();
        let source_id2 = SourceId::for_git(&url2, reference.clone()).unwrap();

        let git_source = GitSource::new(url1, reference.clone(), cache_dir.path(), source_id1);

        // Create a dependency with different source
        let dep = crate::core::Dependency::new("repo", source_id2);

        assert!(!git_source.supports(&dep));
    }
}
