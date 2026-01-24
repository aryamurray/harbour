//! Dependency specification.
//!
//! A Dependency describes what a package requires from another package,
//! including version constraints and source information.

use std::path::PathBuf;

use semver::VersionReq;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::core::source_id::{GitReference, SourceId, SourceKind};
use crate::util::context::DEFAULT_REGISTRY_URL;
use crate::util::InternedString;

/// A dependency specification.
#[derive(Debug, Clone)]
pub struct Dependency {
    /// Package name
    name: InternedString,

    /// Version requirement
    version_req: VersionReq,

    /// Where to find the package
    source_id: SourceId,

    /// Whether this is optional
    optional: bool,

    /// Features to enable
    features: Vec<String>,

    /// Whether default features are enabled
    default_features: bool,
}

impl Dependency {
    /// Create a new dependency.
    pub fn new(name: impl Into<InternedString>, source_id: SourceId) -> Self {
        Dependency {
            name: name.into(),
            version_req: VersionReq::STAR,
            source_id,
            optional: false,
            features: Vec::new(),
            default_features: true,
        }
    }

    /// Create a dependency with a version requirement.
    pub fn with_version_req(mut self, req: VersionReq) -> Self {
        self.version_req = req;
        self
    }

    /// Set whether this dependency is optional.
    pub fn optional(mut self, optional: bool) -> Self {
        self.optional = optional;
        self
    }

    /// Set features to enable.
    pub fn with_features(mut self, features: Vec<String>) -> Self {
        self.features = features;
        self
    }

    /// Set whether default features are enabled.
    pub fn with_default_features(mut self, enabled: bool) -> Self {
        self.default_features = enabled;
        self
    }

    /// Get the package name.
    pub fn name(&self) -> InternedString {
        self.name
    }

    /// Get the version requirement.
    pub fn version_req(&self) -> &VersionReq {
        &self.version_req
    }

    /// Get the source ID.
    pub fn source_id(&self) -> SourceId {
        self.source_id
    }

    /// Check if this is an optional dependency.
    pub fn is_optional(&self) -> bool {
        self.optional
    }

    /// Get the features to enable.
    pub fn features(&self) -> &[String] {
        &self.features
    }

    /// Check if default features are enabled.
    pub fn uses_default_features(&self) -> bool {
        self.default_features
    }

    /// Check if a version matches this dependency's requirement.
    pub fn matches_version(&self, version: &semver::Version) -> bool {
        self.version_req.matches(version)
    }

    /// Check if this is a path dependency.
    pub fn is_path(&self) -> bool {
        self.source_id.is_path()
    }

    /// Check if this is a git dependency.
    pub fn is_git(&self) -> bool {
        self.source_id.is_git()
    }

    /// Check if this is a registry dependency.
    pub fn is_registry(&self) -> bool {
        self.source_id.is_registry()
    }
}

/// Dependency specification as it appears in Harbor.toml.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DependencySpec {
    /// Simple version string: `foo = "1.0"`
    Simple(String),

    /// Detailed specification
    Detailed(DetailedDependencySpec),
}

/// Detailed dependency specification.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DetailedDependencySpec {
    /// Version requirement
    #[serde(default)]
    pub version: Option<String>,

    /// Path to local dependency
    #[serde(default)]
    pub path: Option<PathBuf>,

    /// Git repository URL
    #[serde(default)]
    pub git: Option<String>,

    /// Git branch
    #[serde(default)]
    pub branch: Option<String>,

    /// Git tag
    #[serde(default)]
    pub tag: Option<String>,

    /// Git revision
    #[serde(default)]
    pub rev: Option<String>,

    /// Registry URL (uses default registry if not specified)
    #[serde(default)]
    pub registry: Option<String>,

    /// Whether this dependency is optional
    #[serde(default)]
    pub optional: Option<bool>,

    /// Features to enable
    #[serde(default)]
    pub features: Option<Vec<String>>,

    /// Whether to use default features
    #[serde(default)]
    pub default_features: Option<bool>,
}

impl DependencySpec {
    /// Convert to a Dependency given the package name and manifest directory.
    pub fn to_dependency(
        &self,
        name: &str,
        manifest_dir: &std::path::Path,
    ) -> anyhow::Result<Dependency> {
        match self {
            DependencySpec::Simple(version) => {
                // Simple version string uses the default registry
                let version_req: VersionReq = version.parse()?;

                // Validate package name for registry deps
                crate::sources::registry::validate_package_name(name)?;

                let registry_url = Url::parse(DEFAULT_REGISTRY_URL)?;
                let source_id = SourceId::for_registry(&registry_url)?;

                Ok(Dependency::new(name, source_id).with_version_req(version_req))
            }
            DependencySpec::Detailed(spec) => spec.to_dependency(name, manifest_dir),
        }
    }
}

impl DetailedDependencySpec {
    /// Convert to a Dependency.
    pub fn to_dependency(
        &self,
        name: &str,
        manifest_dir: &std::path::Path,
    ) -> anyhow::Result<Dependency> {
        let source_id = if let Some(ref path) = self.path {
            // Path dependency
            let full_path = if path.is_absolute() {
                path.clone()
            } else {
                manifest_dir.join(path)
            };
            SourceId::for_path(&full_path)?
        } else if let Some(ref git_url) = self.git {
            // Git dependency
            let url = Url::parse(git_url)?;
            let reference = if let Some(ref branch) = self.branch {
                GitReference::Branch(branch.clone())
            } else if let Some(ref tag) = self.tag {
                GitReference::Tag(tag.clone())
            } else if let Some(ref rev) = self.rev {
                GitReference::Rev(rev.clone())
            } else {
                GitReference::DefaultBranch
            };
            SourceId::for_git(&url, reference)?
        } else if self.registry.is_some() || self.version.is_some() {
            // Registry dependency (explicit registry or version-only implies registry)
            crate::sources::registry::validate_package_name(name)?;

            let registry_url = if let Some(ref url) = self.registry {
                Url::parse(url)?
            } else {
                Url::parse(DEFAULT_REGISTRY_URL)?
            };
            SourceId::for_registry(&registry_url)?
        } else {
            anyhow::bail!(
                "dependency `{}` must specify `path`, `git`, `registry`, or `version`",
                name
            );
        };

        let version_req = if let Some(ref v) = self.version {
            v.parse()?
        } else {
            VersionReq::STAR
        };

        let mut dep = Dependency::new(name, source_id).with_version_req(version_req);

        if let Some(opt) = self.optional {
            dep = dep.optional(opt);
        }

        if let Some(ref features) = self.features {
            dep = dep.with_features(features.clone());
        }

        if let Some(default_features) = self.default_features {
            dep = dep.with_default_features(default_features);
        }

        Ok(dep)
    }
}

impl std::fmt::Display for Dependency {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name)?;
        if self.version_req != VersionReq::STAR {
            write!(f, " {}", self.version_req)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_dependency_creation() {
        let tmp = TempDir::new().unwrap();
        let source = SourceId::for_path(tmp.path()).unwrap();
        let dep = Dependency::new("mylib", source)
            .with_version_req("^1.0".parse().unwrap())
            .optional(true);

        assert_eq!(dep.name().as_str(), "mylib");
        assert!(dep.is_optional());
        assert!(dep.is_path());
    }

    #[test]
    fn test_dependency_spec_detailed() {
        let tmp = TempDir::new().unwrap();
        let spec = DetailedDependencySpec {
            path: Some(PathBuf::from(".")),
            version: Some("^1.0".to_string()),
            optional: Some(true),
            ..Default::default()
        };

        let dep = spec.to_dependency("test", tmp.path()).unwrap();
        assert_eq!(dep.name().as_str(), "test");
        assert!(dep.is_optional());
    }

    #[test]
    fn test_dependency_spec_git() {
        let tmp = TempDir::new().unwrap();
        let spec = DetailedDependencySpec {
            git: Some("https://github.com/user/repo".to_string()),
            tag: Some("v1.0".to_string()),
            ..Default::default()
        };

        let dep = spec.to_dependency("test", tmp.path()).unwrap();
        assert!(dep.is_git());
        assert_eq!(
            dep.source_id().git_reference(),
            Some(&GitReference::Tag("v1.0".to_string()))
        );
    }
}
