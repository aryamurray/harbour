//! `harbour search` command

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::cli::SearchArgs;

/// A search result for a package.
#[derive(Debug)]
struct PackageResult {
    name: String,
    version: String,
    category: Option<String>,
}

pub fn execute(args: SearchArgs) -> Result<()> {
    let query = args.query.to_lowercase();

    // Determine registry path
    let registry_path = if let Some(path) = args.registry_path {
        path
    } else {
        // Use default registry location
        get_default_registry_path()?
    };

    // Verify registry exists
    let index_path = registry_path.join("index");
    if !index_path.exists() {
        bail!(
            "registry index not found at: {}\n\
             hint: specify --registry-path or ensure the default registry is configured",
            index_path.display()
        );
    }

    // Scan for packages matching the query
    let results = search_packages(&index_path, &query)?;

    if results.is_empty() {
        println!("No packages found matching '{}'", args.query);
        return Ok(());
    }

    // Display results
    println!(
        "Found {} package{} matching '{}':\n",
        results.len(),
        if results.len() == 1 { "" } else { "s" },
        args.query
    );

    for result in &results {
        let category_str = result
            .category
            .as_ref()
            .map(|c| format!(" [{}]", c))
            .unwrap_or_default();

        println!("  {} v{}{}", result.name, result.version, category_str);
    }

    Ok(())
}

/// Get the default registry path from the harbour cache.
fn get_default_registry_path() -> Result<PathBuf> {
    // Try to use GlobalContext to find the default registry
    let ctx = harbour::util::GlobalContext::new().context("failed to create global context")?;
    let cache_dir = ctx.cache_dir();

    // Check for any registry in the cache
    let registry_cache = cache_dir.join("registry");
    if registry_cache.exists() {
        // Find the first registry directory
        for entry in std::fs::read_dir(&registry_cache)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                let potential_index = entry.path().join("index");
                if potential_index.exists() {
                    // Return the registry root (parent of index)
                    return Ok(entry.path());
                }
            }
        }
    }

    bail!(
        "no registry configured\n\
         hint: use --registry-path to specify a local registry directory"
    );
}

/// Search for packages in the registry index.
fn search_packages(index_path: &Path, query: &str) -> Result<Vec<PackageResult>> {
    let mut results = Vec::new();

    // Scan all letter directories
    for letter_entry in std::fs::read_dir(index_path)? {
        let letter_entry = letter_entry?;
        let letter_path = letter_entry.path();

        if !letter_path.is_dir() {
            continue;
        }

        // Scan all package directories
        for pkg_entry in std::fs::read_dir(&letter_path)? {
            let pkg_entry = pkg_entry?;
            let pkg_path = pkg_entry.path();

            if !pkg_path.is_dir() {
                continue;
            }

            let pkg_name = pkg_entry.file_name().to_string_lossy().to_string();

            // Check if package name matches query
            if pkg_name.contains(query) {
                // Find the latest version (or any version)
                if let Some(result) = get_package_info(&pkg_path, &pkg_name)? {
                    results.push(result);
                }
            }
        }
    }

    // Sort by package name
    results.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(results)
}

/// Get package info from the package directory.
/// Returns the latest version's information.
fn get_package_info(pkg_path: &Path, pkg_name: &str) -> Result<Option<PackageResult>> {
    let mut versions: Vec<(semver::Version, PathBuf)> = Vec::new();

    // Collect all version files
    for entry in std::fs::read_dir(pkg_path)? {
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

    if versions.is_empty() {
        return Ok(None);
    }

    // Sort by version (newest first)
    versions.sort_by(|a, b| b.0.cmp(&a.0));

    // Get the latest version
    let (latest_version, shim_path) = &versions[0];

    // Try to parse the shim to get metadata
    let category = extract_category(shim_path);

    Ok(Some(PackageResult {
        name: pkg_name.to_string(),
        version: latest_version.to_string(),
        category,
    }))
}

/// Extract the category from a shim file.
fn extract_category(shim_path: &Path) -> Option<String> {
    // Read and parse the shim file
    let content = std::fs::read_to_string(shim_path).ok()?;

    // Use a simple approach - parse as TOML and extract metadata.category
    let value: toml::Value = toml::from_str(&content).ok()?;

    value
        .get("metadata")?
        .get("category")?
        .as_str()
        .map(|s| s.to_string())
}
