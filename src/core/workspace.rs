//! Workspace - central configuration hub.
//!
//! A Workspace represents the root package and its configuration,
//! providing centralized access to paths and settings. Supports both
//! single-package projects and multi-package workspaces.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use glob::Pattern;
use thiserror::Error;

use crate::core::manifest::WorkspaceConfig;
use crate::core::{Manifest, Package, PackageId};
use crate::util::{GlobalContext, InternedString};

/// Canonical manifest filename (preferred).
pub const MANIFEST_NAME: &str = "Harbour.toml";

/// Alias manifest filename (for compatibility).
pub const MANIFEST_ALIAS: &str = "Harbor.toml";

/// Canonical lockfile filename.
pub const LOCKFILE_NAME: &str = "Harbour.lock";

/// Errors that can occur when finding a manifest.
#[derive(Debug, Error)]
pub enum ManifestError {
    /// Both canonical and alias manifest files exist.
    #[error("found both `{}` and `{}`\nhelp: delete one. `{MANIFEST_NAME}` is canonical.",
            .primary.display(), .alias.display())]
    AmbiguousManifest { primary: PathBuf, alias: PathBuf },

    /// No manifest file found in the directory.
    #[error("no manifest found in `{}`\nhelp: create `Harbour.toml`", .dir.display())]
    NotFound { dir: PathBuf },
}

/// Errors specific to workspace operations.
#[derive(Debug, Error)]
pub enum WorkspaceError {
    /// Nested workspace detected.
    #[error("nested workspace detected at `{path}`\nhelp: member directories cannot have their own [workspace] section")]
    NestedWorkspace { path: PathBuf },

    /// Root package has [package] but is not in members.
    #[error("root manifest has [package] but is not included in workspace members\nhelp: add `members = [\".\", ...]` to include the root package")]
    RootPackageNotInMembers,

    /// Empty members list.
    #[error("workspace has no members\nhelp: add member paths or globs to [workspace] members")]
    EmptyMembers,

    /// Duplicate package name.
    #[error("duplicate package name `{name}` found in workspace\n  first: {first}\n  second: {second}")]
    DuplicatePackageName {
        name: String,
        first: PathBuf,
        second: PathBuf,
    },
}

/// Find the manifest file in a directory.
///
/// Checks for `Harbour.toml` and `Harbor.toml`. Returns an error if both exist
/// (ambiguous) or neither exists (not found). Accepts the alias for compatibility
/// but requires only one to be present.
pub fn find_manifest(dir: &Path) -> Result<PathBuf, ManifestError> {
    let primary = dir.join(MANIFEST_NAME);
    let alias = dir.join(MANIFEST_ALIAS);

    // Use is_file() to avoid matching directories named Harbour.toml
    let primary_exists = primary.is_file();
    let alias_exists = alias.is_file();

    match (primary_exists, alias_exists) {
        (true, true) => Err(ManifestError::AmbiguousManifest { primary, alias }),
        (true, false) => Ok(primary),
        (false, true) => Ok(alias),
        (false, false) => Err(ManifestError::NotFound { dir: dir.to_path_buf() }),
    }
}

/// Find the lockfile in a directory.
///
/// Checks for `Harbour.lock` only (no alias - single source of truth for lockfile).
/// Returns the path if found, or None if it doesn't exist.
pub fn find_lockfile(dir: &Path) -> Option<PathBuf> {
    let lockfile = dir.join(LOCKFILE_NAME);
    if lockfile.is_file() {
        Some(lockfile)
    } else {
        None
    }
}

/// A workspace member package.
#[derive(Debug, Clone)]
pub struct WorkspaceMember {
    /// Path to the manifest file.
    pub manifest_path: PathBuf,

    /// Canonicalized directory (for deduplication and build).
    pub dir: PathBuf,

    /// Relative path from workspace root (original glob match).
    pub relative_path: String,

    /// The loaded package.
    pub package: Package,
}

impl WorkspaceMember {
    /// Get the package name.
    pub fn name(&self) -> InternedString {
        self.package.name()
    }
}

/// Information about the workspace root.
#[derive(Debug)]
pub struct WorkspaceRootInfo {
    /// Path to the workspace root directory.
    pub root_path: PathBuf,

    /// Path to the workspace manifest.
    pub manifest_path: PathBuf,

    /// If started from within a member, contains (member_name, member_path).
    pub containing_member: Option<(InternedString, PathBuf)>,
}

/// Find the workspace root starting from a directory.
///
/// Walks up the directory tree looking for a manifest with a `[workspace]` section.
/// Returns information about the workspace root and which member (if any) contains
/// the starting directory.
pub fn find_workspace_root(start: &Path) -> Result<Option<WorkspaceRootInfo>> {
    let start = start
        .canonicalize()
        .with_context(|| format!("failed to canonicalize path: {}", start.display()))?;

    let mut current = start.as_path();

    loop {
        if let Ok(manifest_path) = find_manifest(current) {
            let manifest = Manifest::load(&manifest_path)?;

            if manifest.is_workspace() {
                let root_path = current.to_path_buf();

                // Discover members to find which one contains `start`
                let containing_member = if let Some(ref ws_config) = manifest.workspace {
                    find_containing_member(&root_path, ws_config, &start)?
                } else {
                    None
                };

                return Ok(Some(WorkspaceRootInfo {
                    root_path,
                    manifest_path,
                    containing_member,
                }));
            }
        }

        // Move to parent directory
        match current.parent() {
            Some(parent) if parent != current => {
                current = parent;
            }
            _ => break, // Reached root or no more parents
        }
    }

    Ok(None)
}

/// Find which member contains the given path.
fn find_containing_member(
    workspace_root: &Path,
    config: &WorkspaceConfig,
    target_path: &Path,
) -> Result<Option<(InternedString, PathBuf)>> {
    let members = discover_members(workspace_root, config)?;

    for member in members {
        let member_dir = member.dir.canonicalize().unwrap_or(member.dir.clone());
        if target_path.starts_with(&member_dir) {
            return Ok(Some((member.name(), member.dir)));
        }
    }

    Ok(None)
}

/// Discover workspace members from glob patterns.
///
/// Iterates through member patterns, finds matching directories with manifests,
/// applies exclude patterns, deduplicates by canonical path, and validates
/// that members don't have their own `[workspace]` sections.
pub fn discover_members(
    workspace_root: &Path,
    config: &WorkspaceConfig,
) -> Result<Vec<WorkspaceMember>> {
    let mut members: Vec<WorkspaceMember> = Vec::new();
    let mut seen_dirs: HashSet<PathBuf> = HashSet::new();
    let mut seen_names: HashMap<InternedString, PathBuf> = HashMap::new();

    // Compile exclude patterns
    let exclude_patterns: Vec<Pattern> = config
        .exclude
        .iter()
        .filter_map(|p| Pattern::new(p).ok())
        .collect();

    // Process each member pattern
    for pattern in &config.members {
        let full_pattern = workspace_root.join(pattern);
        let pattern_str = full_pattern.to_string_lossy();

        // Use glob to find matching paths
        let matches = glob::glob(&pattern_str).with_context(|| {
            format!("invalid glob pattern in workspace members: {}", pattern)
        })?;

        for entry in matches {
            let path = entry.with_context(|| {
                format!("failed to read directory while expanding glob: {}", pattern)
            })?;

            // Skip if not a directory
            if !path.is_dir() {
                continue;
            }

            // Compute relative path from workspace root
            let relative_path = pathdiff::diff_paths(&path, workspace_root)
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_else(|| pattern.clone());

            // Check exclude patterns (match against relative path)
            if exclude_patterns.iter().any(|p| p.matches(&relative_path)) {
                tracing::debug!("excluding member by pattern: {}", relative_path);
                continue;
            }

            // Canonicalize for deduplication
            let canonical_dir = path
                .canonicalize()
                .unwrap_or_else(|_| path.clone());

            // Skip if already seen (handles symlinks and overlapping globs)
            if seen_dirs.contains(&canonical_dir) {
                continue;
            }
            seen_dirs.insert(canonical_dir.clone());

            // Find manifest in directory
            let manifest_path = match find_manifest(&path) {
                Ok(p) => p,
                Err(ManifestError::NotFound { .. }) => {
                    tracing::debug!("skipping member without manifest: {}", relative_path);
                    continue;
                }
                Err(e) => return Err(e.into()),
            };

            // Load and validate manifest
            let manifest = Manifest::load(&manifest_path)?;

            // Check for nested workspace
            if manifest.is_workspace() {
                return Err(WorkspaceError::NestedWorkspace {
                    path: path.clone(),
                }
                .into());
            }

            // Must have a package section
            if manifest.package.is_none() {
                anyhow::bail!(
                    "workspace member at `{}` has no [package] section",
                    path.display()
                );
            }

            // Create package
            let package = Package::new(manifest, path.clone())?;
            let name = package.name();

            // Check for duplicate package names
            if let Some(first_path) = seen_names.get(&name) {
                return Err(WorkspaceError::DuplicatePackageName {
                    name: name.to_string(),
                    first: first_path.clone(),
                    second: path.clone(),
                }
                .into());
            }
            seen_names.insert(name, path.clone());

            members.push(WorkspaceMember {
                manifest_path,
                dir: canonical_dir,
                relative_path,
                package,
            });
        }
    }

    // Sort by relative path for deterministic ordering
    members.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));

    Ok(members)
}

/// A workspace containing the root package/manifest and member packages.
#[derive(Debug)]
pub struct Workspace {
    /// Workspace root directory.
    root: PathBuf,

    /// The root manifest.
    root_manifest: Manifest,

    /// The root package (None for virtual workspaces).
    root_package: Option<Package>,

    /// Workspace members (discovered from [workspace] members).
    members: Vec<WorkspaceMember>,

    /// Index from package name to member index.
    member_by_name: HashMap<InternedString, usize>,

    /// Target directory for build outputs.
    target_dir: PathBuf,

    /// Current build profile.
    profile: String,
}

impl Workspace {
    /// Create a new workspace from a manifest path.
    pub fn new(manifest_path: &Path, _ctx: &GlobalContext) -> Result<Self> {
        let manifest = Manifest::load(manifest_path)?;
        let root = manifest_path
            .parent()
            .unwrap_or(Path::new("."))
            .to_path_buf();

        // Determine if this is a workspace or single package
        let (root_package, members, member_by_name) = if let Some(ref ws_config) = manifest.workspace
        {
            // This is a workspace - discover members
            let discovered = discover_members(&root, ws_config)?;

            // Check that members list is not empty
            if discovered.is_empty() && manifest.package.is_none() {
                return Err(WorkspaceError::EmptyMembers.into());
            }

            // Build name index
            let mut member_by_name = HashMap::new();
            for (idx, member) in discovered.iter().enumerate() {
                member_by_name.insert(member.name(), idx);
            }

            // Handle root package
            let root_package = if manifest.package.is_some() {
                // Check if root is in members (by checking if "." is in members or root is canonicalized in discovered)
                let root_canonical = root.canonicalize().unwrap_or_else(|_| root.clone());
                let root_in_members = discovered
                    .iter()
                    .any(|m| m.dir == root_canonical);

                if !root_in_members {
                    // Root has [package] but is not in members - this is an error per the plan
                    return Err(WorkspaceError::RootPackageNotInMembers.into());
                }

                // Root package is handled via members, so set to None to avoid duplication
                None
            } else {
                // Virtual workspace - no root package
                None
            };

            (root_package, discovered, member_by_name)
        } else {
            // Single package project - treat as workspace with one member
            let root_package = Package::new(manifest.clone(), root.clone())?;
            let name = root_package.name();

            let member = WorkspaceMember {
                manifest_path: manifest_path.to_path_buf(),
                dir: root.canonicalize().unwrap_or_else(|_| root.clone()),
                relative_path: ".".to_string(),
                package: root_package.clone(),
            };

            let mut member_by_name = HashMap::new();
            member_by_name.insert(name, 0);

            (Some(root_package), vec![member], member_by_name)
        };

        // Default target directory is .harbour/target in the workspace root
        let target_dir = root.join(".harbour").join("target");

        Ok(Workspace {
            root,
            root_manifest: manifest,
            root_package,
            members,
            member_by_name,
            target_dir,
            profile: "debug".to_string(),
        })
    }

    /// Create a workspace with a custom target directory.
    pub fn with_target_dir(mut self, target_dir: PathBuf) -> Self {
        self.target_dir = target_dir;
        self
    }

    /// Set the build profile.
    pub fn with_profile(mut self, profile: impl Into<String>) -> Self {
        self.profile = profile.into();
        self
    }

    /// Check if this is a virtual workspace (no root package).
    pub fn is_virtual(&self) -> bool {
        self.root_manifest.is_virtual_workspace()
    }

    /// Get all workspace members.
    pub fn members(&self) -> &[WorkspaceMember] {
        &self.members
    }

    /// Get a member by name.
    pub fn member(&self, name: &str) -> Option<&WorkspaceMember> {
        let name = InternedString::new(name);
        self.member_by_name
            .get(&name)
            .map(|&idx| &self.members[idx])
    }

    /// Get member names as strings.
    pub fn member_names(&self) -> Vec<&str> {
        self.members.iter().map(|m| m.name().as_str()).collect()
    }

    /// Get the default members (from default-members config or all members).
    pub fn default_members(&self) -> Vec<&WorkspaceMember> {
        if let Some(ref ws_config) = self.root_manifest.workspace {
            if let Some(ref default_patterns) = ws_config.default_members {
                // Match member relative paths against default-members patterns
                let patterns: Vec<Pattern> = default_patterns
                    .iter()
                    .filter_map(|p| Pattern::new(p).ok())
                    .collect();

                return self
                    .members
                    .iter()
                    .filter(|m| {
                        patterns.iter().any(|p| p.matches(&m.relative_path))
                            || default_patterns.contains(&m.relative_path)
                    })
                    .collect();
            }
        }

        // Default: all members
        self.members.iter().collect()
    }

    /// Get the workspace-level dependencies (for inheritance).
    pub fn workspace_dependencies(
        &self,
    ) -> Option<&HashMap<String, crate::core::dependency::DependencySpec>> {
        self.root_manifest
            .workspace
            .as_ref()
            .map(|ws| &ws.dependencies)
    }

    /// Get member paths as a map from name to path.
    pub fn member_paths(&self) -> HashMap<InternedString, PathBuf> {
        self.members
            .iter()
            .map(|m| (m.name(), m.dir.clone()))
            .collect()
    }

    /// Get the root package (for non-virtual workspaces with root not in members).
    /// Deprecated: prefer using members() instead.
    pub fn root_package(&self) -> &Package {
        // For backwards compatibility, return first member if no separate root package
        self.root_package
            .as_ref()
            .unwrap_or_else(|| &self.members[0].package)
    }

    /// Get the root package ID.
    pub fn root_package_id(&self) -> PackageId {
        self.root_package().package_id()
    }

    /// Get the workspace root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Get the root manifest.
    pub fn manifest(&self) -> &Manifest {
        &self.root_manifest
    }

    /// Get the target directory.
    pub fn target_dir(&self) -> &Path {
        &self.target_dir
    }

    /// Get the profile-specific output directory.
    pub fn output_dir(&self) -> PathBuf {
        self.target_dir.join(&self.profile)
    }

    /// Get the deps output directory.
    pub fn deps_dir(&self) -> PathBuf {
        self.output_dir().join("deps")
    }

    /// Get the build directory for a specific package.
    pub fn package_build_dir(&self, pkg_id: PackageId) -> PathBuf {
        self.deps_dir()
            .join(format!("{}-{}", pkg_id.name(), pkg_id.version()))
    }

    /// Get the current profile name.
    pub fn profile(&self) -> &str {
        &self.profile
    }

    /// Check if building in release mode.
    pub fn is_release(&self) -> bool {
        self.profile == "release"
    }

    /// Get the lockfile path.
    ///
    /// Returns existing lockfile if found (checking both Harbour.lock and Harbor.lock),
    /// otherwise returns the canonical path (Harbour.lock).
    pub fn lockfile_path(&self) -> PathBuf {
        find_lockfile(self.root()).unwrap_or_else(|| self.root().join(LOCKFILE_NAME))
    }

    /// Get the manifest path.
    pub fn manifest_path(&self) -> PathBuf {
        find_manifest(self.root()).unwrap_or_else(|_| self.root().join(MANIFEST_NAME))
    }

    /// Get the .harbour directory.
    pub fn harbour_dir(&self) -> PathBuf {
        self.root().join(".harbour")
    }

    /// Get the cache directory.
    pub fn cache_dir(&self) -> PathBuf {
        self.harbour_dir().join("cache")
    }

    /// Ensure the target directory exists.
    pub fn ensure_target_dir(&self) -> Result<()> {
        std::fs::create_dir_all(&self.target_dir).with_context(|| {
            format!(
                "failed to create target directory: {}",
                self.target_dir.display()
            )
        })?;
        Ok(())
    }

    /// Ensure the output directory exists.
    pub fn ensure_output_dir(&self) -> Result<()> {
        let output_dir = self.output_dir();
        std::fs::create_dir_all(&output_dir).with_context(|| {
            format!(
                "failed to create output directory: {}",
                output_dir.display()
            )
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_workspace(dir: &Path) -> PathBuf {
        let manifest_path = dir.join(MANIFEST_NAME);
        std::fs::write(
            &manifest_path,
            r#"
[package]
name = "testws"
version = "1.0.0"

[targets.testws]
kind = "exe"
sources = ["src/**/*.c"]
"#,
        )
        .unwrap();
        manifest_path
    }

    fn create_test_workspace_with_alias(dir: &Path) -> PathBuf {
        let manifest_path = dir.join(MANIFEST_ALIAS);
        std::fs::write(
            &manifest_path,
            r#"
[package]
name = "testws"
version = "1.0.0"

[targets.testws]
kind = "exe"
sources = ["src/**/*.c"]
"#,
        )
        .unwrap();
        manifest_path
    }

    fn create_multi_package_workspace(dir: &Path) -> PathBuf {
        // Create root workspace manifest
        let manifest_path = dir.join(MANIFEST_NAME);
        std::fs::write(
            &manifest_path,
            r#"
[workspace]
members = ["packages/*"]
exclude = ["packages/experimental"]

[workspace.dependencies]
common-dep = "1.0"
"#,
        )
        .unwrap();

        // Create package directories
        std::fs::create_dir_all(dir.join("packages/core")).unwrap();
        std::fs::create_dir_all(dir.join("packages/utils")).unwrap();
        std::fs::create_dir_all(dir.join("packages/experimental")).unwrap();

        // Create member manifests
        std::fs::write(
            dir.join("packages/core/Harbour.toml"),
            r#"
[package]
name = "core"
version = "1.0.0"

[targets.core]
kind = "staticlib"
sources = ["src/**/*.c"]
"#,
        )
        .unwrap();

        std::fs::write(
            dir.join("packages/utils/Harbour.toml"),
            r#"
[package]
name = "utils"
version = "1.0.0"

[targets.utils]
kind = "staticlib"
sources = ["src/**/*.c"]
"#,
        )
        .unwrap();

        std::fs::write(
            dir.join("packages/experimental/Harbour.toml"),
            r#"
[package]
name = "experimental"
version = "0.1.0"

[targets.experimental]
kind = "staticlib"
sources = ["src/**/*.c"]
"#,
        )
        .unwrap();

        manifest_path
    }

    #[test]
    fn test_workspace_creation() {
        let tmp = TempDir::new().unwrap();
        let manifest_path = create_test_workspace(tmp.path());
        let ctx = GlobalContext::with_cwd(tmp.path().to_path_buf()).unwrap();

        let ws = Workspace::new(&manifest_path, &ctx).unwrap();
        assert_eq!(ws.root_package().name().as_str(), "testws");
        assert_eq!(ws.profile(), "debug");
    }

    #[test]
    fn test_workspace_with_alias_manifest() {
        let tmp = TempDir::new().unwrap();
        let manifest_path = create_test_workspace_with_alias(tmp.path());
        let ctx = GlobalContext::with_cwd(tmp.path().to_path_buf()).unwrap();

        let ws = Workspace::new(&manifest_path, &ctx).unwrap();
        assert_eq!(ws.root_package().name().as_str(), "testws");
    }

    #[test]
    fn test_find_manifest_errors_when_both_exist() {
        let tmp = TempDir::new().unwrap();

        // Create both files
        std::fs::write(
            tmp.path().join(MANIFEST_NAME),
            "[package]\nname=\"a\"\nversion=\"1.0.0\"",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join(MANIFEST_ALIAS),
            "[package]\nname=\"b\"\nversion=\"1.0.0\"",
        )
        .unwrap();

        // Should error with AmbiguousManifest
        let result = find_manifest(tmp.path());
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ManifestError::AmbiguousManifest { .. }));
    }

    #[test]
    fn test_find_manifest_accepts_alias_when_alone() {
        let tmp = TempDir::new().unwrap();

        // Create only alias
        std::fs::write(
            tmp.path().join(MANIFEST_ALIAS),
            "[package]\nname=\"b\"\nversion=\"1.0.0\"",
        )
        .unwrap();

        // Should find the alias when it's the only manifest
        let found = find_manifest(tmp.path()).unwrap();
        assert!(found.ends_with(MANIFEST_ALIAS));
    }

    #[test]
    fn test_find_manifest_not_found() {
        let tmp = TempDir::new().unwrap();

        // No manifest files
        let result = find_manifest(tmp.path());
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ManifestError::NotFound { .. }));
    }

    #[test]
    fn test_workspace_paths() {
        let tmp = TempDir::new().unwrap();
        let manifest_path = create_test_workspace(tmp.path());
        let ctx = GlobalContext::with_cwd(tmp.path().to_path_buf()).unwrap();

        let ws = Workspace::new(&manifest_path, &ctx)
            .unwrap()
            .with_profile("release");

        assert!(ws.output_dir().ends_with("release"));
        assert!(ws.lockfile_path().ends_with(LOCKFILE_NAME));
    }

    #[test]
    fn test_multi_package_workspace() {
        let tmp = TempDir::new().unwrap();
        let manifest_path = create_multi_package_workspace(tmp.path());
        let ctx = GlobalContext::with_cwd(tmp.path().to_path_buf()).unwrap();

        let ws = Workspace::new(&manifest_path, &ctx).unwrap();

        // Should have 2 members (experimental is excluded)
        assert_eq!(ws.members().len(), 2);
        assert!(ws.is_virtual());

        // Check member names
        let names: Vec<&str> = ws.member_names();
        assert!(names.contains(&"core"));
        assert!(names.contains(&"utils"));
        assert!(!names.contains(&"experimental"));

        // Check member lookup
        assert!(ws.member("core").is_some());
        assert!(ws.member("utils").is_some());
        assert!(ws.member("experimental").is_none());

        // Default members should be all members
        assert_eq!(ws.default_members().len(), 2);
    }

    #[test]
    fn test_workspace_with_default_members() {
        let tmp = TempDir::new().unwrap();

        // Create workspace with default-members
        std::fs::write(
            tmp.path().join(MANIFEST_NAME),
            r#"
[workspace]
members = ["packages/*"]
default-members = ["packages/core"]
"#,
        )
        .unwrap();

        std::fs::create_dir_all(tmp.path().join("packages/core")).unwrap();
        std::fs::create_dir_all(tmp.path().join("packages/utils")).unwrap();

        std::fs::write(
            tmp.path().join("packages/core/Harbour.toml"),
            "[package]\nname = \"core\"\nversion = \"1.0.0\"",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("packages/utils/Harbour.toml"),
            "[package]\nname = \"utils\"\nversion = \"1.0.0\"",
        )
        .unwrap();

        let manifest_path = tmp.path().join(MANIFEST_NAME);
        let ctx = GlobalContext::with_cwd(tmp.path().to_path_buf()).unwrap();
        let ws = Workspace::new(&manifest_path, &ctx).unwrap();

        // All members should be discovered
        assert_eq!(ws.members().len(), 2);

        // But default members should only be core
        let defaults = ws.default_members();
        assert_eq!(defaults.len(), 1);
        assert_eq!(defaults[0].name().as_str(), "core");
    }

    #[test]
    fn test_nested_workspace_error() {
        let tmp = TempDir::new().unwrap();

        // Create root workspace
        std::fs::write(
            tmp.path().join(MANIFEST_NAME),
            r#"
[workspace]
members = ["nested"]
"#,
        )
        .unwrap();

        // Create nested workspace (should error)
        std::fs::create_dir_all(tmp.path().join("nested")).unwrap();
        std::fs::write(
            tmp.path().join("nested/Harbour.toml"),
            r#"
[package]
name = "nested"
version = "1.0.0"

[workspace]
members = []
"#,
        )
        .unwrap();

        let manifest_path = tmp.path().join(MANIFEST_NAME);
        let ctx = GlobalContext::with_cwd(tmp.path().to_path_buf()).unwrap();
        let result = Workspace::new(&manifest_path, &ctx);

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("nested workspace"));
    }

    #[test]
    fn test_duplicate_package_name_error() {
        let tmp = TempDir::new().unwrap();

        std::fs::write(
            tmp.path().join(MANIFEST_NAME),
            r#"
[workspace]
members = ["a", "b"]
"#,
        )
        .unwrap();

        std::fs::create_dir_all(tmp.path().join("a")).unwrap();
        std::fs::create_dir_all(tmp.path().join("b")).unwrap();

        // Both packages have the same name
        std::fs::write(
            tmp.path().join("a/Harbour.toml"),
            "[package]\nname = \"same-name\"\nversion = \"1.0.0\"",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("b/Harbour.toml"),
            "[package]\nname = \"same-name\"\nversion = \"2.0.0\"",
        )
        .unwrap();

        let manifest_path = tmp.path().join(MANIFEST_NAME);
        let ctx = GlobalContext::with_cwd(tmp.path().to_path_buf()).unwrap();
        let result = Workspace::new(&manifest_path, &ctx);

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("duplicate package name"));
    }
}
