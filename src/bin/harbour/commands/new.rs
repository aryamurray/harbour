//! `harbour new` command

use std::path::PathBuf;

use anyhow::{bail, Result};

use super::init::validate_package_name;
use crate::cli::NewArgs;
use harbour::ops::harbour_new::{new_project, NewOptions};

/// Determines the output path for a new project.
///
/// If a path is explicitly specified, uses that. Otherwise, creates a directory
/// with the same name as the project.
pub fn determine_project_path(name: &str, path: &Option<PathBuf>) -> PathBuf {
    path.clone().unwrap_or_else(|| PathBuf::from(name))
}

/// Validates that the target directory doesn't already exist or is empty.
///
/// Returns Ok(()) if the path is suitable for a new project.
pub fn validate_project_path(path: &PathBuf) -> Result<(), String> {
    if path.exists() {
        if path.is_file() {
            return Err(format!(
                "destination `{}` already exists and is a file",
                path.display()
            ));
        }

        // Check if directory is non-empty
        if let Ok(entries) = std::fs::read_dir(path) {
            if entries.count() > 0 {
                return Err(format!(
                    "destination `{}` already exists and is not empty",
                    path.display()
                ));
            }
        }
    }

    Ok(())
}

pub fn execute(args: NewArgs) -> Result<()> {
    let path = determine_project_path(&args.name, &args.path);

    if let Err(msg) = validate_package_name(&args.name) {
        bail!(msg);
    }

    if let Err(msg) = validate_project_path(&path) {
        bail!(msg);
    }

    let opts = NewOptions {
        name: args.name.clone(),
        lib: args.lib,
        init: false,
    };

    new_project(&path, &opts)?;

    let kind = if args.lib { "library" } else { "binary" };
    eprintln!("     Created {} `{}` package", kind, args.name);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::NewArgs;
    use clap::Parser;
    use std::path::PathBuf;

    /// Helper to parse NewArgs from command-line strings.
    fn parse_new_args(args: &[&str]) -> NewArgs {
        #[derive(Parser)]
        struct TestCli {
            #[command(flatten)]
            new: NewArgs,
        }
        let cli = TestCli::parse_from(args);
        cli.new
    }

    // =========================================================================
    // NewArgs Default Values Tests
    // =========================================================================

    #[test]
    fn test_new_args_with_name_only() {
        let args = parse_new_args(&["test", "myproject"]);

        assert_eq!(args.name, "myproject");
        assert!(!args.lib);
        assert!(args.path.is_none());
    }

    // =========================================================================
    // Package Name Tests (Required Argument)
    // =========================================================================

    #[test]
    fn test_new_simple_name() {
        let args = parse_new_args(&["test", "hello"]);
        assert_eq!(args.name, "hello");
    }

    #[test]
    fn test_new_name_with_hyphen() {
        let args = parse_new_args(&["test", "my-app"]);
        assert_eq!(args.name, "my-app");
    }

    #[test]
    fn test_new_name_with_underscore() {
        let args = parse_new_args(&["test", "my_app"]);
        assert_eq!(args.name, "my_app");
    }

    #[test]
    fn test_new_name_with_numbers() {
        let args = parse_new_args(&["test", "app2025"]);
        assert_eq!(args.name, "app2025");
    }

    // =========================================================================
    // Library Flag Tests
    // =========================================================================

    #[test]
    fn test_new_library() {
        let args = parse_new_args(&["test", "mylib", "--lib"]);
        assert!(args.lib);
    }

    #[test]
    fn test_new_binary_by_default() {
        let args = parse_new_args(&["test", "mybin"]);
        assert!(!args.lib);
    }

    // =========================================================================
    // Path Tests
    // =========================================================================

    #[test]
    fn test_new_with_custom_path() {
        let args = parse_new_args(&["test", "myproject", "--path", "custom/location"]);
        assert_eq!(args.name, "myproject");
        assert_eq!(args.path, Some(PathBuf::from("custom/location")));
    }

    #[test]
    fn test_new_with_absolute_path() {
        let args = parse_new_args(&["test", "myproject", "--path", "/home/user/projects/custom"]);
        assert_eq!(args.path, Some(PathBuf::from("/home/user/projects/custom")));
    }

    #[test]
    fn test_new_with_relative_path() {
        let args = parse_new_args(&["test", "myproject", "--path", "../sibling"]);
        assert_eq!(args.path, Some(PathBuf::from("../sibling")));
    }

    // =========================================================================
    // Combined Flags Tests
    // =========================================================================

    #[test]
    fn test_new_library_with_custom_path() {
        let args = parse_new_args(&["test", "mylib", "--lib", "--path", "libs/mylib"]);
        assert_eq!(args.name, "mylib");
        assert!(args.lib);
        assert_eq!(args.path, Some(PathBuf::from("libs/mylib")));
    }

    // =========================================================================
    // NewOptions Construction Tests
    // =========================================================================

    #[test]
    fn test_new_options_for_binary() {
        let args = parse_new_args(&["test", "mybin"]);

        let opts = NewOptions {
            name: args.name.clone(),
            lib: args.lib,
            init: false,
        };

        assert_eq!(opts.name, "mybin");
        assert!(!opts.lib);
        assert!(!opts.init); // new command sets init to false
    }

    #[test]
    fn test_new_options_for_library() {
        let args = parse_new_args(&["test", "mylib", "--lib"]);

        let opts = NewOptions {
            name: args.name.clone(),
            lib: args.lib,
            init: false,
        };

        assert_eq!(opts.name, "mylib");
        assert!(opts.lib);
        assert!(!opts.init);
    }

    // =========================================================================
    // Edge Cases Tests
    // =========================================================================

    #[test]
    fn test_new_single_char_name() {
        let args = parse_new_args(&["test", "a"]);
        assert_eq!(args.name, "a");
    }

    #[test]
    fn test_new_long_name() {
        let long_name = "a".repeat(100);
        let args = parse_new_args(&["test", &long_name]);
        assert_eq!(args.name, long_name);
    }

    #[test]
    fn test_new_name_with_version_like_suffix() {
        let args = parse_new_args(&["test", "mylib-v2"]);
        assert_eq!(args.name, "mylib-v2");
    }

    // =========================================================================
    // =========================================================================
    // Difference from init command Tests
    // =========================================================================

    #[test]
    fn test_new_vs_init_options_difference() {
        // new command: init = false, name is required
        let new_args = parse_new_args(&["test", "myproject"]);
        let new_opts = NewOptions {
            name: new_args.name.clone(),
            lib: new_args.lib,
            init: false,
        };

        // This distinguishes new from init
        assert!(!new_opts.init);
        assert!(!new_opts.name.is_empty());
    }
}
