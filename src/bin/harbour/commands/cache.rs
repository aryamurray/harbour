//! `harbour cache` command
//!
//! Manage the Harbour cache (registry indices, fetched sources, build artifacts).

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

use crate::cli::{CacheArgs, CacheCommands};
use harbour::util::fs::remove_dir_all_if_exists;
use harbour::util::GlobalContext;

pub fn execute(args: CacheArgs) -> Result<()> {
    match args.command {
        CacheCommands::List => list_cache(),
        CacheCommands::Clean(clean_args) => clean_cache(clean_args),
        CacheCommands::Path => show_path(),
        CacheCommands::Size => show_size(),
    }
}

/// List cached items.
fn list_cache() -> Result<()> {
    let ctx = GlobalContext::new()?;
    let cache_dir = ctx.cache_dir();

    println!("Cache directory: {}", cache_dir.display());
    println!();

    // Registry indices
    let registry_dir = cache_dir.join("registry");
    println!("Registry indices:");
    if registry_dir.exists() {
        list_directory_entries(&registry_dir, "  ")?;
    } else {
        println!("  (none)");
    }
    println!();

    // Fetched sources
    let sources_dir = cache_dir.join("registry-src");
    println!("Fetched sources:");
    if sources_dir.exists() {
        list_directory_entries(&sources_dir, "  ")?;
    } else {
        println!("  (none)");
    }
    println!();

    // Git cache
    let git_dir = cache_dir.join("git");
    println!("Git cache:");
    if git_dir.exists() {
        list_directory_entries(&git_dir, "  ")?;
    } else {
        println!("  (none)");
    }
    println!();

    // Build artifacts (project-local)
    let build_dir = ctx.project_harbour_dir().join("target");
    println!("Build artifacts (project-local):");
    if build_dir.exists() {
        let size = dir_size(&build_dir)?;
        println!("  {} ({})", build_dir.display(), format_size(size));
    } else {
        println!("  (none)");
    }

    Ok(())
}

/// List directory entries with their sizes.
fn list_directory_entries(dir: &Path, prefix: &str) -> Result<()> {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .with_context(|| format!("failed to read directory: {}", dir.display()))?
        .filter_map(|e| e.ok())
        .collect();

    entries.sort_by_key(|e| e.file_name());

    if entries.is_empty() {
        println!("{}(empty)", prefix);
        return Ok(());
    }

    for entry in entries {
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if path.is_dir() {
            let size = dir_size(&path)?;
            println!("{}{} ({})", prefix, name_str, format_size(size));
        } else {
            let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            println!("{}{} ({})", prefix, name_str, format_size(size));
        }
    }

    Ok(())
}

/// Clean cache.
fn clean_cache(args: crate::cli::CacheCleanArgs) -> Result<()> {
    let ctx = GlobalContext::new()?;
    let cache_dir = ctx.cache_dir();

    // If no specific flags, clean everything
    let clean_all = !args.registry && !args.sources && !args.builds;

    let mut cleaned_something = false;

    // Clean registry indices
    if clean_all || args.registry {
        let registry_dir = cache_dir.join("registry");
        if registry_dir.exists() {
            remove_dir_all_if_exists(&registry_dir)?;
            eprintln!("     Removed {}", registry_dir.display());
            cleaned_something = true;
        }
    }

    // Clean fetched sources
    if clean_all || args.sources {
        let sources_dir = cache_dir.join("registry-src");
        if sources_dir.exists() {
            remove_dir_all_if_exists(&sources_dir)?;
            eprintln!("     Removed {}", sources_dir.display());
            cleaned_something = true;
        }

        // Also clean git cache
        let git_dir = cache_dir.join("git");
        if git_dir.exists() {
            remove_dir_all_if_exists(&git_dir)?;
            eprintln!("     Removed {}", git_dir.display());
            cleaned_something = true;
        }
    }

    // Clean build artifacts
    if clean_all || args.builds {
        let build_dir = ctx.project_harbour_dir().join("target");
        if build_dir.exists() {
            remove_dir_all_if_exists(&build_dir)?;
            eprintln!("     Removed {}", build_dir.display());
            cleaned_something = true;
        }
    }

    if !cleaned_something {
        eprintln!("     Nothing to clean");
    }

    Ok(())
}

/// Show cache directory path.
fn show_path() -> Result<()> {
    let ctx = GlobalContext::new()?;
    println!("{}", ctx.cache_dir().display());
    Ok(())
}

/// Show cache disk usage.
fn show_size() -> Result<()> {
    let ctx = GlobalContext::new()?;
    let cache_dir = ctx.cache_dir();

    let mut total_size: u64 = 0;

    println!("Cache disk usage:");
    println!();

    // Registry indices
    let registry_dir = cache_dir.join("registry");
    if registry_dir.exists() {
        let size = dir_size(&registry_dir)?;
        total_size += size;
        println!("  Registry indices:  {}", format_size(size));
    } else {
        println!("  Registry indices:  0 B");
    }

    // Fetched sources
    let sources_dir = cache_dir.join("registry-src");
    if sources_dir.exists() {
        let size = dir_size(&sources_dir)?;
        total_size += size;
        println!("  Fetched sources:   {}", format_size(size));
    } else {
        println!("  Fetched sources:   0 B");
    }

    // Git cache
    let git_dir = cache_dir.join("git");
    if git_dir.exists() {
        let size = dir_size(&git_dir)?;
        total_size += size;
        println!("  Git cache:         {}", format_size(size));
    } else {
        println!("  Git cache:         0 B");
    }

    // Build artifacts (project-local, not counted in total cache)
    let build_dir = ctx.project_harbour_dir().join("target");
    if build_dir.exists() {
        let size = dir_size(&build_dir)?;
        println!("  Build artifacts:   {} (project-local)", format_size(size));
    } else {
        println!("  Build artifacts:   0 B (project-local)");
    }

    println!();
    println!("  Total (global):    {}", format_size(total_size));

    Ok(())
}

/// Calculate the total size of a directory recursively.
fn dir_size(path: &Path) -> Result<u64> {
    let mut size: u64 = 0;

    if path.is_file() {
        return Ok(fs::metadata(path).map(|m| m.len()).unwrap_or(0));
    }

    if !path.is_dir() {
        return Ok(0);
    }

    for entry in
        fs::read_dir(path).with_context(|| format!("failed to read: {}", path.display()))?
    {
        let entry = entry?;
        let path = entry.path();

        if path.is_file() {
            size += fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        } else if path.is_dir() {
            size += dir_size(&path)?;
        }
    }

    Ok(size)
}

/// Format a size in bytes to a human-readable string.
fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_size() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(1024), "1.00 KB");
        assert_eq!(format_size(1536), "1.50 KB");
        assert_eq!(format_size(1048576), "1.00 MB");
        assert_eq!(format_size(1073741824), "1.00 GB");
    }
}
