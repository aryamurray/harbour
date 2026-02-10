//! Dependency specification.
//!
//! A Dependency describes what a package requires from another package,
//! including version constraints and source information.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use semver::VersionReq;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::core::source_id::{GitReference, SourceId};
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

    /// Vcpkg dependency (port name is the dependency key)
    #[serde(default)]
    pub vcpkg: Option<bool>,

    /// Vcpkg triplet override
    #[serde(default)]
    pub triplet: Option<String>,

    /// Vcpkg library name override
    #[serde(default)]
    pub libs: Option<Vec<String>>,

    /// Vcpkg features to enable (e.g., ["wayland", "x11"])
    #[serde(default)]
    pub vcpkg_features: Option<Vec<String>>,

    /// Vcpkg baseline commit for reproducibility
    #[serde(default)]
    pub vcpkg_baseline: Option<String>,

    /// Vcpkg registry name (references [vcpkg.registries.NAME] in config)
    #[serde(default)]
    pub vcpkg_registry: Option<String>,

    /// Whether this dependency is optional
    #[serde(default)]
    pub optional: Option<bool>,

    /// Features to enable
    #[serde(default)]
    pub features: Option<Vec<String>>,

    /// Whether to use default features
    #[serde(default)]
    pub default_features: Option<bool>,

    /// Inherit from [workspace.dependencies]
    #[serde(default)]
    pub workspace: Option<bool>,
}

impl DetailedDependencySpec {
    /// Check if this spec has an explicit source selector (path/git/registry).
    pub fn has_explicit_source(&self) -> bool {
        self.path.is_some()
            || self.git.is_some()
            || self.registry.is_some()
            || self.vcpkg == Some(true)
    }

    /// Validate that workspace = true is not combined with explicit sources.
    pub fn validate_workspace_field(&self, name: &str) -> anyhow::Result<()> {
        if self.workspace == Some(true) {
            if self.has_explicit_source() {
                anyhow::bail!(
                    "dependency `{}` cannot specify `workspace = true` with `path`, `git`, `registry`, or `vcpkg`",
                    name
                );
            }
            if self.version.is_some() {
                anyhow::bail!(
                    "dependency `{}` cannot specify `workspace = true` with `version`",
                    name
                );
            }
        }
        Ok(())
    }
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
        } else if self.vcpkg == Some(true) {
            SourceId::for_vcpkg(
                name,
                self.triplet.as_deref(),
                self.libs.as_deref(),
                self.vcpkg_features.as_deref(),
                self.vcpkg_baseline.as_deref(),
                self.vcpkg_registry.as_deref(),
            )?
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
                "dependency `{}` must specify `path`, `git`, `registry`, `vcpkg`, or `version`",
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

/// Resolve a dependency with workspace context.
///
/// Resolution order of precedence:
/// 1. Explicit source selector (path/git/registry) → use directly
/// 2. Name matches workspace member → implicit path dependency
/// 3. `workspace = true` → inherit from `[workspace.dependencies]`
/// 4. Else → registry lookup
///
/// Note: `version = "..."` does NOT skip local-first. Version is a constraint only, not a source selector.
pub fn resolve_dependency(
    name: &str,
    spec: &DependencySpec,
    workspace_deps: Option<&HashMap<String, DependencySpec>>,
    workspace_members: &HashMap<InternedString, PathBuf>,
    manifest_dir: &Path,
) -> anyhow::Result<Dependency> {
    match spec {
        DependencySpec::Simple(version) => {
            // Check if name matches a workspace member (local-first)
            let member_name = InternedString::new(name);
            if let Some(member_path) = workspace_members.get(&member_name) {
                // Implicit path dependency to sibling
                let source_id = SourceId::for_path(member_path)?;
                let version_req: VersionReq = version.parse()?;
                return Ok(Dependency::new(name, source_id).with_version_req(version_req));
            }

            // Otherwise, it's a registry dependency
            let version_req: VersionReq = version.parse()?;
            crate::sources::registry::validate_package_name(name)?;
            let registry_url = Url::parse(DEFAULT_REGISTRY_URL)?;
            let source_id = SourceId::for_registry(&registry_url)?;
            Ok(Dependency::new(name, source_id).with_version_req(version_req))
        }
        DependencySpec::Detailed(spec) => {
            resolve_detailed_dependency(name, spec, workspace_deps, workspace_members, manifest_dir)
        }
    }
}

/// Resolve a detailed dependency specification with workspace context.
fn resolve_detailed_dependency(
    name: &str,
    spec: &DetailedDependencySpec,
    workspace_deps: Option<&HashMap<String, DependencySpec>>,
    workspace_members: &HashMap<InternedString, PathBuf>,
    manifest_dir: &Path,
) -> anyhow::Result<Dependency> {
    // Validate workspace field constraints
    spec.validate_workspace_field(name)?;

    // 1. Explicit source selector takes precedence
    if spec.has_explicit_source() {
        return spec.to_dependency(name, manifest_dir);
    }

    // 2. Check if name matches workspace member (local-first) - only if no explicit source
    let member_name = InternedString::new(name);
    if let Some(member_path) = workspace_members.get(&member_name) {
        let source_id = SourceId::for_path(member_path)?;
        let version_req = if let Some(ref v) = spec.version {
            v.parse()?
        } else {
            VersionReq::STAR
        };

        let mut dep = Dependency::new(name, source_id).with_version_req(version_req);

        if let Some(opt) = spec.optional {
            dep = dep.optional(opt);
        }
        if let Some(ref features) = spec.features {
            dep = dep.with_features(features.clone());
        }
        if let Some(default_features) = spec.default_features {
            dep = dep.with_default_features(default_features);
        }

        return Ok(dep);
    }

    // 3. `workspace = true` → inherit from [workspace.dependencies]
    if spec.workspace == Some(true) {
        let ws_deps = workspace_deps.ok_or_else(|| {
            anyhow::anyhow!(
                "dependency `{}` specifies `workspace = true` but no workspace dependencies are defined",
                name
            )
        })?;

        let ws_spec = ws_deps.get(name).ok_or_else(|| {
            anyhow::anyhow!(
                "dependency `{}` specifies `workspace = true` but `{}` is not in [workspace.dependencies]",
                name,
                name
            )
        })?;

        // Resolve the workspace spec first
        let mut dep = resolve_dependency(name, ws_spec, None, workspace_members, manifest_dir)?;

        // Apply local overrides (features additive, optional additive only)
        if let Some(ref local_features) = spec.features {
            // Merge features (additive)
            let mut features: Vec<String> = dep.features().to_vec();
            for f in local_features {
                if !features.contains(f) {
                    features.push(f.clone());
                }
            }
            dep = dep.with_features(features);
        }

        if let Some(local_optional) = spec.optional {
            // Optional can only increase (false -> true allowed, true -> false not allowed)
            if local_optional && !dep.is_optional() {
                dep = dep.optional(true);
            } else if !local_optional && dep.is_optional() {
                anyhow::bail!(
                    "dependency `{}`: cannot override `optional = true` from workspace with `optional = false`",
                    name
                );
            }
        }

        return Ok(dep);
    }

    // 4. Else → registry lookup (via version or default)
    spec.to_dependency(name, manifest_dir)
}

/// Warn if a [workspace.dependencies] key matches a member name.
pub fn warn_workspace_dep_matches_member(
    workspace_deps: &HashMap<String, DependencySpec>,
    workspace_members: &HashMap<InternedString, PathBuf>,
) {
    for dep_name in workspace_deps.keys() {
        let name = InternedString::new(dep_name);
        if workspace_members.contains_key(&name) {
            tracing::warn!(
                "[workspace.dependencies] key `{}` matches a workspace member - \
                 this may cause unexpected behavior",
                dep_name
            );
        }
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

    #[test]
    fn test_resolve_local_first() {
        let tmp = TempDir::new().unwrap();

        // Set up workspace members
        let mut members = HashMap::new();
        members.insert(InternedString::new("sibling"), tmp.path().to_path_buf());

        // Dependency matching a member name should be local-first
        let spec = DependencySpec::Simple("1.0".to_string());
        let dep = resolve_dependency("sibling", &spec, None, &members, tmp.path()).unwrap();

        assert!(dep.is_path());
        assert_eq!(dep.name().as_str(), "sibling");
    }

    #[test]
    fn test_resolve_explicit_registry_overrides_local() {
        let tmp = TempDir::new().unwrap();

        // Set up workspace members
        let mut members = HashMap::new();
        members.insert(InternedString::new("sibling"), tmp.path().to_path_buf());

        // Explicit registry should override local-first
        let spec = DependencySpec::Detailed(DetailedDependencySpec {
            registry: Some("https://example.com/registry".to_string()),
            version: Some("1.0".to_string()),
            ..Default::default()
        });
        let dep = resolve_dependency("sibling", &spec, None, &members, tmp.path()).unwrap();

        assert!(dep.is_registry());
    }

    #[test]
    fn test_resolve_workspace_inheritance() {
        let tmp = TempDir::new().unwrap();
        let members = HashMap::new();

        // Set up workspace dependencies
        let mut ws_deps = HashMap::new();
        ws_deps.insert(
            "inherited".to_string(),
            DependencySpec::Detailed(DetailedDependencySpec {
                git: Some("https://github.com/user/inherited".to_string()),
                tag: Some("v2.0".to_string()),
                features: Some(vec!["feature1".to_string()]),
                ..Default::default()
            }),
        );

        // Member uses workspace = true
        let spec = DependencySpec::Detailed(DetailedDependencySpec {
            workspace: Some(true),
            features: Some(vec!["feature2".to_string()]),
            ..Default::default()
        });

        let dep =
            resolve_dependency("inherited", &spec, Some(&ws_deps), &members, tmp.path()).unwrap();

        assert!(dep.is_git());
        // Features should be merged
        assert!(dep.features().contains(&"feature1".to_string()));
        assert!(dep.features().contains(&"feature2".to_string()));
    }

    #[test]
    fn test_workspace_true_with_path_error() {
        let tmp = TempDir::new().unwrap();
        let members = HashMap::new();

        let spec = DependencySpec::Detailed(DetailedDependencySpec {
            workspace: Some(true),
            path: Some(PathBuf::from("../other")),
            ..Default::default()
        });

        let result = resolve_dependency("test", &spec, None, &members, tmp.path());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("cannot specify `workspace = true` with `path`"));
    }

    #[test]
    fn test_optional_can_only_increase() {
        let tmp = TempDir::new().unwrap();
        let members = HashMap::new();

        // Workspace dep is optional
        let mut ws_deps = HashMap::new();
        ws_deps.insert(
            "optdep".to_string(),
            DependencySpec::Detailed(DetailedDependencySpec {
                version: Some("1.0".to_string()),
                optional: Some(true),
                ..Default::default()
            }),
        );

        // Member tries to make it required (should fail)
        let spec = DependencySpec::Detailed(DetailedDependencySpec {
            workspace: Some(true),
            optional: Some(false),
            ..Default::default()
        });

        let result = resolve_dependency("optdep", &spec, Some(&ws_deps), &members, tmp.path());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("cannot override `optional = true`"));
    }
}
