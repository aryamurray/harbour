//! `harbour new` command

use std::path::PathBuf;

use anyhow::Result;

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
    let path = args.path.unwrap_or_else(|| PathBuf::from(&args.name));

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
    use tempfile::TempDir;

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
        assert_eq!(
            args.path,
            Some(PathBuf::from("/home/user/projects/custom"))
        );
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
    // determine_project_path Tests
    // =========================================================================

    #[test]
    fn test_determine_project_path_default() {
        let name = "myproject";
        let path = None;
        let result = determine_project_path(name, &path);
        assert_eq!(result, PathBuf::from("myproject"));
    }

    #[test]
    fn test_determine_project_path_custom() {
        let name = "myproject";
        let path = Some(PathBuf::from("custom/path"));
        let result = determine_project_path(name, &path);
        assert_eq!(result, PathBuf::from("custom/path"));
    }

    #[test]
    fn test_determine_project_path_different_name_and_path() {
        let name = "actual-name";
        let path = Some(PathBuf::from("different-directory"));
        let result = determine_project_path(name, &path);
        assert_eq!(result, PathBuf::from("different-directory"));
    }

    // =========================================================================
    // validate_project_path Tests
    // =========================================================================

    #[test]
    fn test_validate_project_path_nonexistent() {
        let tmp = TempDir::new().unwrap();
        let new_path = tmp.path().join("nonexistent_project");
        let result = validate_project_path(&new_path);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_project_path_empty_dir() {
        let tmp = TempDir::new().unwrap();
        let empty_dir = tmp.path().join("empty_project");
        std::fs::create_dir(&empty_dir).unwrap();

        let result = validate_project_path(&empty_dir);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_project_path_nonempty_dir() {
        let tmp = TempDir::new().unwrap();
        let nonempty_dir = tmp.path().join("nonempty_project");
        std::fs::create_dir(&nonempty_dir).unwrap();
        std::fs::write(nonempty_dir.join("file.txt"), "content").unwrap();

        let result = validate_project_path(&nonempty_dir);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not empty"));
    }

    #[test]
    fn test_validate_project_path_is_file() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("some_file");
        std::fs::write(&file_path, "content").unwrap();

        let result = validate_project_path(&file_path);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("is a file"));
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
    // Integration with init module (shared validation)
    // =========================================================================

    #[test]
    fn test_new_uses_same_validation_rules_as_init() {
        // Using the validation function from init module
        use super::super::init::validate_package_name;

        let valid_names = vec!["myproject", "my-project", "my_project", "project123"];
        for name in valid_names {
            assert!(validate_package_name(name).is_ok(), "Should be valid: {}", name);
        }

        let invalid_names = vec!["-start", "_start", ".hidden", "with space"];
        for name in invalid_names {
            assert!(
                validate_package_name(name).is_err(),
                "Should be invalid: {}",
                name
            );
        }
    }

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
