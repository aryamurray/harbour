//! Git source - dependencies from git repositories.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use git2::{Repository, ResetType};
use url::Url;

use crate::core::source_id::GitReference;
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
            let manifest_path = self.checkout_path.join("Harbor.toml");
            if !manifest_path.exists() {
                bail!(
                    "no Harbor.toml found in git repository: {}",
                    self.remote
                );
            }

            // Create package with precise source ID
            let precise_source = if let Some(ref precise) = self.precise {
                self.source_id.with_precise(precise)
            } else {
                self.source_id
            };

            let manifest = crate::core::Manifest::load(&manifest_path)?;
            let package = Package::with_source_id(
                manifest,
                self.checkout_path.clone(),
                precise_source,
            )?;

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
        self.checkout_path.exists() && self.checkout_path.join("Harbor.toml").exists()
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

    #[test]
    fn test_sanitize_url() {
        let url = Url::parse("https://github.com/user/repo.git").unwrap();
        assert_eq!(sanitize_url_for_path(&url), "github.com-user-repo");

        let url2 = Url::parse("https://gitlab.com/org/project").unwrap();
        assert_eq!(sanitize_url_for_path(&url2), "gitlab.com-org-project");
    }
}
