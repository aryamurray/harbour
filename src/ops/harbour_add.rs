//! Implementation of `harbour add` and `harbour remove`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use toml_edit::{DocumentMut, InlineTable, Item, Table};

use crate::core::{Dependency, SourceId};
use crate::sources::SourceCache;
use crate::util::context::RegistryList;
use crate::util::fs;

/// Identifies a registry for error messages.
#[derive(Debug, Clone)]
pub struct RegistryId {
    /// Human-readable registry name (e.g., "curated", "default")
    pub name: String,
    /// Optional URL for the registry
    pub url: Option<String>,
}

impl std::fmt::Display for RegistryId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name)
    }
}

/// The kind of source for a dependency.
#[derive(Debug, Clone)]
pub enum SourceKind {
    /// Registry dependency with registry name
    Registry(String),
    /// Git repository with URL
    Git(String),
    /// Local path
    Path(PathBuf),
}

impl std::fmt::Display for SourceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SourceKind::Registry(name) => write!(f, "registry:{}", name),
            SourceKind::Git(_) => write!(f, "git"),
            SourceKind::Path(_) => write!(f, "path"),
        }
    }
}

/// Result of an add operation.
#[derive(Debug, Clone)]
pub enum AddResult {
    /// Successfully added a new dependency
    Added {
        name: String,
        version: String,
        source: SourceKind,
    },
    /// Updated an existing dependency to a new version
    Updated {
        name: String,
        from: String,
        to: String,
        source: SourceKind,
    },
    /// Dependency is already present with matching version
    AlreadyPresent { name: String, version: String },
    /// Package was not found in any registry
    NotFound {
        name: String,
        looked_in: Vec<RegistryId>,
    },
}

/// Result of a remove operation.
#[derive(Debug, Clone)]
pub enum RemoveResult {
    /// Successfully removed the dependency
    Removed { name: String, version: String },
    /// Dependency was not found in the manifest
    NotFound { name: String },
}

/// Options for adding a dependency.
#[derive(Debug, Clone)]
pub struct AddOptions {
    /// Package name
    pub name: String,

    /// Path dependency
    pub path: Option<String>,

    /// Git repository URL
    pub git: Option<String>,

    /// Git branch
    pub branch: Option<String>,

    /// Git tag
    pub tag: Option<String>,

    /// Git revision
    pub rev: Option<String>,

    /// Version requirement
    pub version: Option<String>,

    /// Add as optional dependency
    pub optional: bool,

    /// Dry run - don't actually modify files
    pub dry_run: bool,

    /// Offline mode - skip network validation
    pub offline: bool,
}

/// Add a dependency to Harbor.toml.
///
/// Returns an `AddResult` indicating what happened:
/// - `Added`: New dependency was added
/// - `Updated`: Existing dependency was updated
/// - `AlreadyPresent`: Dependency already exists with same version
/// - `NotFound`: Package not found in any registry (only for registry deps with validation)
pub fn add_dependency(manifest_path: &Path, opts: &AddOptions) -> Result<AddResult> {
    let content = fs::read_to_string(manifest_path)?;
    let mut doc: DocumentMut = content
        .parse()
        .with_context(|| "failed to parse Harbor.toml")?;

    // Determine the source kind
    let source = if let Some(ref path) = opts.path {
        SourceKind::Path(PathBuf::from(path))
    } else if let Some(ref git) = opts.git {
        SourceKind::Git(git.clone())
    } else {
        SourceKind::Registry("default".to_string())
    };

    // Check if dependency already exists
    let existing_version = get_existing_dependency(&doc, &opts.name);

    // Determine the version string (for display purposes)
    let version = opts.version.clone().unwrap_or_else(|| "*".to_string());

    // Check for already-present case
    if let Some(ref existing) = existing_version {
        if existing == &version {
            return Ok(AddResult::AlreadyPresent {
                name: opts.name.clone(),
                version,
            });
        }
    }

    // Ensure [dependencies] table exists
    if !doc.contains_key("dependencies") {
        doc["dependencies"] = Item::Table(Table::new());
    }

    // Build dependency value
    let dep_value = build_dependency_value(opts)?;

    // Don't modify files if dry run
    if opts.dry_run {
        if let Some(existing) = existing_version {
            return Ok(AddResult::Updated {
                name: opts.name.clone(),
                from: existing,
                to: version,
                source,
            });
        } else {
            return Ok(AddResult::Added {
                name: opts.name.clone(),
                version,
                source,
            });
        }
    }

    // Add to dependencies
    doc["dependencies"][&opts.name] = dep_value;

    // Write back
    fs::write_string(manifest_path, &doc.to_string())?;

    // Return appropriate result
    if let Some(existing) = existing_version {
        Ok(AddResult::Updated {
            name: opts.name.clone(),
            from: existing,
            to: version,
            source,
        })
    } else {
        Ok(AddResult::Added {
            name: opts.name.clone(),
            version,
            source,
        })
    }
}

/// Get the existing version of a dependency from the document, if any.
fn get_existing_dependency(doc: &DocumentMut, name: &str) -> Option<String> {
    let deps = doc.get("dependencies")?.as_table()?;
    let dep = deps.get(name)?;

    // Handle both string values and table values
    if let Some(version) = dep.as_str() {
        return Some(version.to_string());
    }

    if let Some(table) = dep.as_inline_table() {
        if let Some(version) = table.get("version") {
            if let Some(v) = version.as_str() {
                return Some(v.to_string());
            }
        }
        // For git/path deps without version, return a placeholder
        if table.get("git").is_some() || table.get("path").is_some() {
            return Some("*".to_string());
        }
    }

    None
}

/// Validate that a registry dependency exists.
///
/// This performs fast index-only validation without full resolution.
/// Returns the registries that were searched if not found.
pub fn validate_registry_dependency(
    name: &str,
    version: Option<&str>,
    registries: &RegistryList,
    cache_dir: &Path,
    offline: bool,
) -> Result<Option<(String, Vec<RegistryId>)>> {
    // In offline mode, skip validation entirely
    if offline {
        return Ok(None);
    }

    let mut looked_in = Vec::new();

    for registry in registries.enabled() {
        looked_in.push(RegistryId {
            name: registry.name.clone(),
            url: Some(registry.url.clone()),
        });

        // Try to find the package in this registry's index
        // This is a lightweight check - just verifying the shim file exists
        let registry_url = match url::Url::parse(&registry.url) {
            Ok(url) => url,
            Err(_) => continue,
        };

        let source_id = match SourceId::for_registry(&registry_url) {
            Ok(id) => id,
            Err(_) => continue,
        };

        let mut source_cache = SourceCache::new(cache_dir.to_path_buf());

        // Create dependency for query
        let version_req: semver::VersionReq = version
            .map(|v| format!("={}", v))
            .unwrap_or_else(|| "*".to_string())
            .parse()
            .unwrap_or(semver::VersionReq::STAR);

        let dep = Dependency::new(name, source_id).with_version_req(version_req);

        // Try to query - if we get results, the package exists
        match source_cache.query(&dep) {
            Ok(summaries) if !summaries.is_empty() => {
                // Found it!
                return Ok(None);
            }
            _ => continue,
        }
    }

    // Not found in any registry
    Ok(Some((name.to_string(), looked_in)))
}

/// Build the TOML value for a dependency.
fn build_dependency_value(opts: &AddOptions) -> Result<Item> {
    // If only path or git is specified without other options, use inline table
    let needs_table = opts.version.is_some()
        || opts.branch.is_some()
        || opts.tag.is_some()
        || opts.rev.is_some()
        || opts.optional;

    if let Some(ref path) = opts.path {
        if needs_table {
            let mut table = InlineTable::new();
            table.insert("path", path.clone().into());

            if let Some(ref version) = opts.version {
                table.insert("version", version.clone().into());
            }
            if opts.optional {
                table.insert("optional", true.into());
            }

            Ok(Item::Value(table.into()))
        } else {
            let mut table = InlineTable::new();
            table.insert("path", path.clone().into());
            Ok(Item::Value(table.into()))
        }
    } else if let Some(ref git) = opts.git {
        let mut table = InlineTable::new();
        table.insert("git", git.clone().into());

        if let Some(ref branch) = opts.branch {
            table.insert("branch", branch.clone().into());
        } else if let Some(ref tag) = opts.tag {
            table.insert("tag", tag.clone().into());
        } else if let Some(ref rev) = opts.rev {
            table.insert("rev", rev.clone().into());
        }

        if let Some(ref version) = opts.version {
            table.insert("version", version.clone().into());
        }
        if opts.optional {
            table.insert("optional", true.into());
        }

        Ok(Item::Value(table.into()))
    } else {
        // Registry dependency - just a version string or table
        // e.g., zlib = "1.3.1" or zlib = { version = "1.3.1", optional = true }
        let version = opts.version.clone().unwrap_or_else(|| "*".to_string());

        if opts.optional {
            let mut table = InlineTable::new();
            table.insert("version", version.into());
            table.insert("optional", true.into());
            Ok(Item::Value(table.into()))
        } else {
            // Simple version string: zlib = "1.3.1"
            Ok(Item::Value(version.into()))
        }
    }
}

/// Options for removing a dependency.
#[derive(Debug, Clone, Default)]
pub struct RemoveOptions {
    /// Dry run - don't actually modify files
    pub dry_run: bool,
}

/// Remove a dependency from Harbor.toml.
///
/// Returns a `RemoveResult` indicating what happened:
/// - `Removed`: Dependency was successfully removed
/// - `NotFound`: Dependency was not found in the manifest
pub fn remove_dependency(
    manifest_path: &Path,
    name: &str,
    opts: &RemoveOptions,
) -> Result<RemoveResult> {
    let content = fs::read_to_string(manifest_path)?;
    let mut doc: DocumentMut = content
        .parse()
        .with_context(|| "failed to parse Harbor.toml")?;

    // Check if dependencies table exists
    if !doc.contains_key("dependencies") {
        return Ok(RemoveResult::NotFound {
            name: name.to_string(),
        });
    }

    // Get the existing version before removing
    let existing_version = get_existing_dependency(&doc, name);

    // Check if dependency exists
    let deps = doc["dependencies"].as_table_mut().unwrap();
    if !deps.contains_key(name) {
        return Ok(RemoveResult::NotFound {
            name: name.to_string(),
        });
    }

    let version = existing_version.unwrap_or_else(|| "*".to_string());

    // Dry run - don't actually modify
    if opts.dry_run {
        return Ok(RemoveResult::Removed {
            name: name.to_string(),
            version,
        });
    }

    // Remove the dependency
    deps.remove(name);

    // Write back
    fs::write_string(manifest_path, &doc.to_string())?;

    Ok(RemoveResult::Removed {
        name: name.to_string(),
        version,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_manifest(dir: &Path) -> std::path::PathBuf {
        let manifest_path = dir.join("Harbor.toml");
        std::fs::write(
            &manifest_path,
            r#"[package]
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

    fn default_add_opts(name: &str) -> AddOptions {
        AddOptions {
            name: name.to_string(),
            path: None,
            git: None,
            branch: None,
            tag: None,
            rev: None,
            version: None,
            optional: false,
            dry_run: false,
            offline: true, // Skip network in tests
        }
    }

    #[test]
    fn test_add_path_dependency() {
        let tmp = TempDir::new().unwrap();
        let manifest_path = create_test_manifest(tmp.path());

        let opts = AddOptions {
            path: Some("../mylib".to_string()),
            ..default_add_opts("mylib")
        };

        let result = add_dependency(&manifest_path, &opts).unwrap();
        assert!(matches!(result, AddResult::Added { .. }));

        let content = std::fs::read_to_string(&manifest_path).unwrap();
        assert!(content.contains("[dependencies]"));
        assert!(content.contains("mylib"));
        assert!(content.contains("../mylib"));
    }

    #[test]
    fn test_add_git_dependency() {
        let tmp = TempDir::new().unwrap();
        let manifest_path = create_test_manifest(tmp.path());

        let opts = AddOptions {
            git: Some("https://github.com/madler/zlib".to_string()),
            tag: Some("v1.3.1".to_string()),
            ..default_add_opts("zlib")
        };

        let result = add_dependency(&manifest_path, &opts).unwrap();
        assert!(matches!(result, AddResult::Added { .. }));

        let content = std::fs::read_to_string(&manifest_path).unwrap();
        assert!(content.contains("zlib"));
        assert!(content.contains("github.com/madler/zlib"));
        assert!(content.contains("v1.3.1"));
    }

    #[test]
    fn test_remove_dependency() {
        let tmp = TempDir::new().unwrap();
        let manifest_path = tmp.path().join("Harbor.toml");
        std::fs::write(
            &manifest_path,
            r#"[package]
name = "test"
version = "1.0.0"

[dependencies]
mylib = { path = "../mylib" }
"#,
        )
        .unwrap();

        let result = remove_dependency(&manifest_path, "mylib", &RemoveOptions::default()).unwrap();
        assert!(matches!(result, RemoveResult::Removed { .. }));

        let content = std::fs::read_to_string(&manifest_path).unwrap();
        assert!(!content.contains("mylib"));
    }

    #[test]
    fn test_remove_dependency_not_found() {
        let tmp = TempDir::new().unwrap();
        let manifest_path = create_test_manifest(tmp.path());

        let result =
            remove_dependency(&manifest_path, "nonexistent", &RemoveOptions::default()).unwrap();
        assert!(matches!(result, RemoveResult::NotFound { .. }));
    }

    #[test]
    fn test_add_registry_dependency() {
        let tmp = TempDir::new().unwrap();
        let manifest_path = create_test_manifest(tmp.path());

        let opts = AddOptions {
            version: Some("1.3.1".to_string()),
            ..default_add_opts("zlib")
        };

        let result = add_dependency(&manifest_path, &opts).unwrap();
        assert!(matches!(result, AddResult::Added { .. }));

        let content = std::fs::read_to_string(&manifest_path).unwrap();
        assert!(content.contains("zlib"));
        assert!(content.contains("1.3.1"));
    }

    #[test]
    fn test_add_registry_dependency_wildcard() {
        let tmp = TempDir::new().unwrap();
        let manifest_path = create_test_manifest(tmp.path());

        // No version specified - should default to "*"
        let opts = default_add_opts("fmt");

        let result = add_dependency(&manifest_path, &opts).unwrap();
        assert!(matches!(result, AddResult::Added { .. }));

        let content = std::fs::read_to_string(&manifest_path).unwrap();
        assert!(content.contains("fmt"));
        assert!(content.contains("*") || content.contains("\"*\""));
    }

    #[test]
    fn test_add_already_present() {
        let tmp = TempDir::new().unwrap();
        let manifest_path = tmp.path().join("Harbor.toml");
        std::fs::write(
            &manifest_path,
            r#"[package]
name = "test"
version = "1.0.0"

[dependencies]
zlib = "1.3.1"
"#,
        )
        .unwrap();

        let opts = AddOptions {
            version: Some("1.3.1".to_string()),
            ..default_add_opts("zlib")
        };

        let result = add_dependency(&manifest_path, &opts).unwrap();
        assert!(matches!(result, AddResult::AlreadyPresent { .. }));
    }

    #[test]
    fn test_add_update_version() {
        let tmp = TempDir::new().unwrap();
        let manifest_path = tmp.path().join("Harbor.toml");
        std::fs::write(
            &manifest_path,
            r#"[package]
name = "test"
version = "1.0.0"

[dependencies]
zlib = "1.2.0"
"#,
        )
        .unwrap();

        let opts = AddOptions {
            version: Some("1.3.1".to_string()),
            ..default_add_opts("zlib")
        };

        let result = add_dependency(&manifest_path, &opts).unwrap();
        assert!(
            matches!(result, AddResult::Updated { from, to, .. } if from == "1.2.0" && to == "1.3.1")
        );
    }

    #[test]
    fn test_add_dry_run() {
        let tmp = TempDir::new().unwrap();
        let manifest_path = create_test_manifest(tmp.path());

        let opts = AddOptions {
            version: Some("1.3.1".to_string()),
            dry_run: true,
            ..default_add_opts("zlib")
        };

        let result = add_dependency(&manifest_path, &opts).unwrap();
        assert!(matches!(result, AddResult::Added { .. }));

        // File should NOT be modified
        let content = std::fs::read_to_string(&manifest_path).unwrap();
        assert!(!content.contains("zlib"));
    }
}
