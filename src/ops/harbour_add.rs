//! Implementation of `harbour add` and `harbour remove`.

use std::path::Path;

use anyhow::{bail, Context, Result};
use toml_edit::{value, Array, DocumentMut, InlineTable, Item, Table};

use crate::util::fs;

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
}

/// Add a dependency to Harbor.toml.
pub fn add_dependency(manifest_path: &Path, opts: &AddOptions) -> Result<()> {
    let content = fs::read_to_string(manifest_path)?;
    let mut doc: DocumentMut = content
        .parse()
        .with_context(|| "failed to parse Harbor.toml")?;

    // Ensure [dependencies] table exists
    if !doc.contains_key("dependencies") {
        doc["dependencies"] = Item::Table(Table::new());
    }

    // Build dependency value
    let dep_value = build_dependency_value(opts)?;

    // Add to dependencies
    doc["dependencies"][&opts.name] = dep_value;

    // Write back
    fs::write_string(manifest_path, &doc.to_string())?;

    Ok(())
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
        bail!(
            "dependency `{}` must specify either `path` or `git`",
            opts.name
        );
    }
}

/// Remove a dependency from Harbor.toml.
pub fn remove_dependency(manifest_path: &Path, name: &str) -> Result<()> {
    let content = fs::read_to_string(manifest_path)?;
    let mut doc: DocumentMut = content
        .parse()
        .with_context(|| "failed to parse Harbor.toml")?;

    // Check if dependencies table exists
    if !doc.contains_key("dependencies") {
        bail!("no dependencies in Harbor.toml");
    }

    // Check if dependency exists
    let deps = doc["dependencies"].as_table_mut().unwrap();
    if !deps.contains_key(name) {
        bail!("dependency `{}` not found in Harbor.toml", name);
    }

    // Remove the dependency
    deps.remove(name);

    // Write back
    fs::write_string(manifest_path, &doc.to_string())?;

    Ok(())
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

    #[test]
    fn test_add_path_dependency() {
        let tmp = TempDir::new().unwrap();
        let manifest_path = create_test_manifest(tmp.path());

        let opts = AddOptions {
            name: "mylib".to_string(),
            path: Some("../mylib".to_string()),
            git: None,
            branch: None,
            tag: None,
            rev: None,
            version: None,
            optional: false,
        };

        add_dependency(&manifest_path, &opts).unwrap();

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
            name: "zlib".to_string(),
            path: None,
            git: Some("https://github.com/madler/zlib".to_string()),
            branch: None,
            tag: Some("v1.3.1".to_string()),
            rev: None,
            version: None,
            optional: false,
        };

        add_dependency(&manifest_path, &opts).unwrap();

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

        remove_dependency(&manifest_path, "mylib").unwrap();

        let content = std::fs::read_to_string(&manifest_path).unwrap();
        assert!(!content.contains("mylib"));
    }
}
