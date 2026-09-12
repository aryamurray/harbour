//! `harbour clean` command

use std::fs;
use std::path::Path;

use anyhow::Result;

use crate::cli::CleanArgs;
use harbour::util::fs::remove_dir_all_if_exists;
use harbour::util::GlobalContext;

pub fn execute(args: CleanArgs) -> Result<()> {
    let ctx = GlobalContext::new()?;

    let harbour_dir = ctx.project_harbour_dir();

    // Checked before `--all` and `--target` so that combining them is not
    // silently one-of: `--probes` is additive with either, and on its own it
    // is the only option that keeps compiled objects.
    if args.probes {
        let removed = remove_probe_caches(&ctx.target_dir())?;
        if removed == 0 {
            eprintln!("     No probe caches to remove");
        } else {
            eprintln!("     Removed {removed} probe cache(s); probes will be re-measured");
        }
        if !args.all && !args.target {
            return Ok(());
        }
    }

    if args.all {
        // Remove entire .harbour directory
        remove_dir_all_if_exists(&harbour_dir)?;
        eprintln!("     Removed {}", harbour_dir.display());
    } else if args.target {
        // Only remove target directory
        let target_dir = ctx.target_dir();
        remove_dir_all_if_exists(&target_dir)?;
        eprintln!("     Removed {}", target_dir.display());
    } else {
        // Default: remove target directory
        let target_dir = ctx.target_dir();
        remove_dir_all_if_exists(&target_dir)?;
        eprintln!("     Removed {}", target_dir.display());
    }

    Ok(())
}

/// Remove every `probe/` directory under the target tree, returning how many
/// were removed.
///
/// Found by walking rather than by reconstructing the paths, because there
/// is one per (triple, profile, package, target) and `probe_dir` in
/// `builder::probe` owns that layout. Rebuilding the same path arithmetic
/// here would be a second definition of where probe caches live, and would
/// drift the moment the layout changed -- which is the defect this codebase
/// has been audited for repeatedly. A directory named `probe` under the
/// target tree is Harbour's own, so matching on the name is sufficient and
/// stays correct if the nesting changes.
fn remove_probe_caches(target_dir: &Path) -> Result<usize> {
    if !target_dir.exists() {
        return Ok(0);
    }
    let mut found = Vec::new();
    collect_probe_dirs(target_dir, &mut found)?;
    // Sorted so the output is stable across runs and filesystems; readdir
    // order is not.
    found.sort();
    for dir in &found {
        remove_dir_all_if_exists(dir)?;
    }
    Ok(found.len())
}

fn collect_probe_dirs(dir: &Path, out: &mut Vec<std::path::PathBuf>) -> Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        // A directory that vanished or cannot be read is not a reason to
        // fail a clean.
        Err(_) => return Ok(()),
    };
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if path.file_name().is_some_and(|n| n == "probe") {
            out.push(path);
            // Do not descend: the whole tree is going.
            continue;
        }
        collect_probe_dirs(&path, out)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::CleanArgs;
    use clap::Parser;
    use tempfile::TempDir;

    /// Helper to parse CleanArgs from command-line strings.
    fn parse_clean_args(args: &[&str]) -> CleanArgs {
        #[derive(Parser)]
        struct TestCli {
            #[command(flatten)]
            clean: CleanArgs,
        }
        let cli = TestCli::parse_from(args);
        cli.clean
    }

    // =========================================================================
    // CleanArgs Default Values Tests
    // =========================================================================

    #[test]
    fn test_clean_args_defaults() {
        let args = parse_clean_args(&["test"]);

        assert!(!args.target);
        assert!(!args.all);
    }

    // =========================================================================
    // Target Flag Tests
    // =========================================================================

    #[test]
    fn test_clean_target_flag() {
        let args = parse_clean_args(&["test", "--target"]);
        assert!(args.target);
        assert!(!args.all);
    }

    // =========================================================================
    // All Flag Tests
    // =========================================================================

    #[test]
    fn test_clean_all_flag() {
        let args = parse_clean_args(&["test", "--all"]);
        assert!(args.all);
    }

    // =========================================================================
    // Combined Flags Tests
    // =========================================================================

    #[test]
    fn test_clean_both_flags() {
        // Both flags can be specified; --all takes precedence
        let args = parse_clean_args(&["test", "--target", "--all"]);
        assert!(args.target);
        assert!(args.all);
    }

    // =========================================================================
    // File System Tests (using tempfile)
    // =========================================================================

    #[test]
    fn test_clean_removes_directory() {
        let tmp = TempDir::new().unwrap();
        let target_dir = tmp.path().join(".harbour").join("target");
        std::fs::create_dir_all(&target_dir).unwrap();

        // Create a file inside
        std::fs::write(target_dir.join("some_artifact.o"), "binary content").unwrap();

        assert!(target_dir.exists());

        // Clean it
        remove_dir_all_if_exists(&target_dir).unwrap();

        assert!(!target_dir.exists());
    }

    #[test]
    fn test_clean_nonexistent_directory_succeeds() {
        let tmp = TempDir::new().unwrap();
        let nonexistent = tmp.path().join("does_not_exist");

        // Should not error even if directory doesn't exist
        let result = remove_dir_all_if_exists(&nonexistent);
        assert!(result.is_ok());
    }

    #[test]
    fn test_clean_nested_directories() {
        let tmp = TempDir::new().unwrap();
        let harbour_dir = tmp.path().join(".harbour");
        let target_dir = harbour_dir.join("target");
        let debug_dir = target_dir.join("debug");
        let release_dir = target_dir.join("release");

        std::fs::create_dir_all(&debug_dir).unwrap();
        std::fs::create_dir_all(&release_dir).unwrap();

        // Create files in both
        std::fs::write(debug_dir.join("main.o"), "debug obj").unwrap();
        std::fs::write(release_dir.join("main.o"), "release obj").unwrap();

        assert!(debug_dir.exists());
        assert!(release_dir.exists());

        // Clean entire target
        remove_dir_all_if_exists(&target_dir).unwrap();

        assert!(!target_dir.exists());
        assert!(harbour_dir.exists()); // Parent still exists
    }

    #[test]
    fn test_clean_all_removes_harbour_dir() {
        let tmp = TempDir::new().unwrap();
        let harbour_dir = tmp.path().join(".harbour");
        let target_dir = harbour_dir.join("target");
        let cache_dir = harbour_dir.join("cache");

        std::fs::create_dir_all(&target_dir).unwrap();
        std::fs::create_dir_all(&cache_dir).unwrap();

        std::fs::write(target_dir.join("artifact.o"), "obj").unwrap();
        std::fs::write(cache_dir.join("cached_dep.tar.gz"), "cached").unwrap();

        assert!(harbour_dir.exists());

        // Clean all
        remove_dir_all_if_exists(&harbour_dir).unwrap();

        assert!(!harbour_dir.exists());
    }

    // =========================================================================
    // Edge Cases Tests
    // =========================================================================

    #[test]
    fn test_clean_empty_directory() {
        let tmp = TempDir::new().unwrap();
        let empty_dir = tmp.path().join("empty");
        std::fs::create_dir(&empty_dir).unwrap();

        assert!(empty_dir.exists());

        remove_dir_all_if_exists(&empty_dir).unwrap();

        assert!(!empty_dir.exists());
    }
}
