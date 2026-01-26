//! `harbour clean` command

use anyhow::Result;

use crate::cli::CleanArgs;
use harbour::util::fs::remove_dir_all_if_exists;
use harbour::util::GlobalContext;

/// Determines which directories to clean based on the provided arguments.
///
/// Returns a tuple of (clean_target, clean_all).
pub fn determine_clean_scope(target_only: bool, clean_all: bool) -> (bool, bool) {
    if clean_all {
        // Clean everything (including cache)
        (true, true)
    } else {
        // Clean target directory (default behavior)
        (true, false)
    }
}

pub fn execute(args: CleanArgs) -> Result<()> {
    let ctx = GlobalContext::new()?;

    let harbour_dir = ctx.project_harbour_dir();

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
    // determine_clean_scope Tests
    // =========================================================================

    #[test]
    fn test_determine_clean_scope_default() {
        let (clean_target, clean_all) = determine_clean_scope(false, false);
        assert!(clean_target);
        assert!(!clean_all);
    }

    #[test]
    fn test_determine_clean_scope_target_only() {
        let (clean_target, clean_all) = determine_clean_scope(true, false);
        assert!(clean_target);
        assert!(!clean_all);
    }

    #[test]
    fn test_determine_clean_scope_all() {
        let (clean_target, clean_all) = determine_clean_scope(false, true);
        assert!(clean_target);
        assert!(clean_all);
    }

    #[test]
    fn test_determine_clean_scope_both_flags() {
        let (clean_target, clean_all) = determine_clean_scope(true, true);
        assert!(clean_target);
        assert!(clean_all);
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
