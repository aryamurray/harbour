//! `harbour search` command

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use harbour::sources::registry::index::{list_index_files, read_index_file};
use harbour::sources::registry::IndexRecord;

use crate::cli::SearchArgs;

/// A search result for a package.
#[derive(Debug)]
struct PackageResult {
    name: String,
    version: String,
    category: Option<String>,
    /// Every version is yanked, so none is offered to a new resolution.
    all_yanked: bool,
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

        let yanked_str = if result.all_yanked {
            " (all versions yanked)"
        } else {
            ""
        };

        println!(
            "  {} v{}{}{}",
            result.name, result.version, category_str, yanked_str
        );
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
///
/// Packages come from the tier-1 `.idx` files, listed and read by the same
/// registry code `harbour add` resolves through, so what `search` offers and what
/// `add` can find are one set. An unreadable index file is reported and skipped:
/// one corrupt package must not hide every other result.
fn search_packages(index_path: &Path, query: &str) -> Result<Vec<PackageResult>> {
    let mut results = Vec::new();

    for (name, idx_path) in list_index_files(index_path)? {
        if !name.contains(query) {
            continue;
        }
        match package_info(index_path, &name, &idx_path) {
            Ok(Some(result)) => results.push(result),
            Ok(None) => {}
            Err(err) => eprintln!("warning: skipping `{name}`: {err:#}"),
        }
    }

    // `list_index_files` is sorted, but sort here too: the order printed is this
    // function's contract, not an accident of the helper.
    results.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(results)
}

/// What `search` shows for one package: the newest version a new resolution
/// would use.
///
/// Yanked versions are skipped (they are not offered to a new resolution). If
/// *every* version is yanked the newest is shown anyway and flagged, so a package
/// never looks like it does not exist.
fn package_info(index_root: &Path, name: &str, idx_path: &Path) -> Result<Option<PackageResult>> {
    let Some(records) = read_index_file(idx_path)? else {
        return Ok(None);
    };

    let mut versions: Vec<(semver::Version, &IndexRecord)> = records
        .iter()
        .filter_map(|r| semver::Version::parse(&r.version).ok().map(|v| (v, r)))
        .collect();
    if versions.is_empty() {
        return Ok(None);
    }
    versions.sort_by(|a, b| b.0.cmp(&a.0));

    let available = versions.iter().find(|(_, r)| r.is_available());
    let (version, record, all_yanked) = match available {
        Some((v, r)) => (v, *r, false),
        None => (&versions[0].0, versions[0].1, true),
    };

    Ok(Some(PackageResult {
        name: name.to_string(),
        version: version.to_string(),
        category: extract_category(&index_root.join(&record.shim)),
        all_yanked,
    }))
}

/// Extract the category from a tier-2 shim file, if the shim is present and has one.
fn extract_category(shim_path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(shim_path).ok()?;
    let value: toml::Value = toml::from_str(&content).ok()?;

    value
        .get("metadata")?
        .get("category")?
        .as_str()
        .map(|s| s.to_string())
}
