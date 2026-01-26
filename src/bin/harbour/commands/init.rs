//! `harbour init` command

use std::path::PathBuf;

use anyhow::Result;

use crate::cli::InitArgs;
use harbour::ops::harbour_new::{init_project, NewOptions};

/// Determines the package name from the arguments or directory.
///
/// This is extracted for testability.
pub fn determine_package_name(name: &Option<String>, path: &PathBuf) -> String {
    name.clone().unwrap_or_else(|| {
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unnamed")
            .to_string()
    })
}

/// Validates a package name for common issues.
///
/// Returns Ok(()) if the name is valid, otherwise returns an error message.
pub fn validate_package_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() {
        return Err("package name cannot be empty");
    }

    if name.starts_with('-') || name.starts_with('_') {
        return Err("package name cannot start with a hyphen or underscore");
    }

    if name.starts_with('.') {
        return Err("package name cannot start with a dot");
    }

    // Check for invalid characters
    for c in name.chars() {
        if !c.is_alphanumeric() && c != '-' && c != '_' {
            return Err("package name contains invalid characters");
        }
    }

    Ok(())
}

pub fn execute(args: InitArgs) -> Result<()> {
    let path = args.path.unwrap_or_else(|| PathBuf::from("."));

    let name = args.name.unwrap_or_else(|| {
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unnamed")
            .to_string()
    });

    let opts = NewOptions {
        name: name.clone(),
        lib: args.lib,
        init: true,
    };

    init_project(&path, &opts)?;

    let kind = if args.lib { "library" } else { "binary" };
    eprintln!("     Initialized {} `{}` package", kind, name);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::InitArgs;
    use clap::Parser;
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// Helper to parse InitArgs from command-line strings.
    fn parse_init_args(args: &[&str]) -> InitArgs {
        #[derive(Parser)]
        struct TestCli {
            #[command(flatten)]
            init: InitArgs,
        }
        let cli = TestCli::parse_from(args);
        cli.init
    }

    // =========================================================================
    // InitArgs Default Values Tests
    // =========================================================================

    #[test]
    fn test_init_args_defaults() {
        let args = parse_init_args(&["test"]);

        assert!(args.name.is_none());
        assert!(!args.lib);
        assert!(args.path.is_none());
    }

    // =========================================================================
    // Package Name Tests
    // =========================================================================

    #[test]
    fn test_init_with_name() {
        let args = parse_init_args(&["test", "--name", "myproject"]);
        assert_eq!(args.name, Some("myproject".to_string()));
    }

    #[test]
    fn test_init_name_with_hyphen() {
        let args = parse_init_args(&["test", "--name", "my-project"]);
        assert_eq!(args.name, Some("my-project".to_string()));
    }

    #[test]
    fn test_init_name_with_underscore() {
        let args = parse_init_args(&["test", "--name", "my_project"]);
        assert_eq!(args.name, Some("my_project".to_string()));
    }

    // =========================================================================
    // Library Flag Tests
    // =========================================================================

    #[test]
    fn test_init_library() {
        let args = parse_init_args(&["test", "--lib"]);
        assert!(args.lib);
    }

    #[test]
    fn test_init_binary_by_default() {
        let args = parse_init_args(&["test"]);
        assert!(!args.lib);
    }

    // =========================================================================
    // Path Tests
    // =========================================================================

    #[test]
    fn test_init_with_path() {
        let args = parse_init_args(&["test", "mydir"]);
        assert_eq!(args.path, Some(PathBuf::from("mydir")));
    }

    #[test]
    fn test_init_with_relative_path() {
        let args = parse_init_args(&["test", "../otherdir"]);
        assert_eq!(args.path, Some(PathBuf::from("../otherdir")));
    }

    #[test]
    fn test_init_with_absolute_path() {
        let args = parse_init_args(&["test", "/home/user/projects/myproject"]);
        assert_eq!(
            args.path,
            Some(PathBuf::from("/home/user/projects/myproject"))
        );
    }

    #[test]
    fn test_init_with_current_dir() {
        let args = parse_init_args(&["test", "."]);
        assert_eq!(args.path, Some(PathBuf::from(".")));
    }

    // =========================================================================
    // Combined Flags Tests
    // =========================================================================

    #[test]
    fn test_init_library_with_name() {
        let args = parse_init_args(&["test", "--lib", "--name", "mylib"]);
        assert!(args.lib);
        assert_eq!(args.name, Some("mylib".to_string()));
    }

    #[test]
    fn test_init_library_with_path() {
        let args = parse_init_args(&["test", "--lib", "mydir"]);
        assert!(args.lib);
        assert_eq!(args.path, Some(PathBuf::from("mydir")));
    }

    #[test]
    fn test_init_all_options() {
        let args = parse_init_args(&["test", "--lib", "--name", "mylib", "mydir"]);
        assert!(args.lib);
        assert_eq!(args.name, Some("mylib".to_string()));
        assert_eq!(args.path, Some(PathBuf::from("mydir")));
    }

    // =========================================================================
    // determine_package_name Tests
    // =========================================================================

    #[test]
    fn test_determine_package_name_with_explicit_name() {
        let name = Some("myproject".to_string());
        let path = PathBuf::from("/some/path/different");
        let result = determine_package_name(&name, &path);
        assert_eq!(result, "myproject");
    }

    #[test]
    fn test_determine_package_name_from_path() {
        let name = None;
        let path = PathBuf::from("/home/user/myproject");
        let result = determine_package_name(&name, &path);
        assert_eq!(result, "myproject");
    }

    #[test]
    fn test_determine_package_name_from_relative_path() {
        let name = None;
        let path = PathBuf::from("./my-lib");
        let result = determine_package_name(&name, &path);
        assert_eq!(result, "my-lib");
    }

    #[test]
    fn test_determine_package_name_unnamed_fallback() {
        let name = None;
        // This path has no file_name (empty or root)
        let path = PathBuf::from("");
        let result = determine_package_name(&name, &path);
        assert_eq!(result, "unnamed");
    }

    // =========================================================================
    // validate_package_name Tests
    // =========================================================================

    #[test]
    fn test_validate_package_name_valid_simple() {
        assert!(validate_package_name("myproject").is_ok());
    }

    #[test]
    fn test_validate_package_name_valid_with_hyphen() {
        assert!(validate_package_name("my-project").is_ok());
    }

    #[test]
    fn test_validate_package_name_valid_with_underscore() {
        assert!(validate_package_name("my_project").is_ok());
    }

    #[test]
    fn test_validate_package_name_valid_with_numbers() {
        assert!(validate_package_name("project123").is_ok());
    }

    #[test]
    fn test_validate_package_name_empty() {
        let result = validate_package_name("");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "package name cannot be empty");
    }

    #[test]
    fn test_validate_package_name_starts_with_hyphen() {
        let result = validate_package_name("-myproject");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("cannot start with a hyphen or underscore"));
    }

    #[test]
    fn test_validate_package_name_starts_with_underscore() {
        let result = validate_package_name("_myproject");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("cannot start with a hyphen or underscore"));
    }

    #[test]
    fn test_validate_package_name_starts_with_dot() {
        let result = validate_package_name(".hidden");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("cannot start with a dot"));
    }

    #[test]
    fn test_validate_package_name_invalid_characters_space() {
        let result = validate_package_name("my project");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("invalid characters"));
    }

    #[test]
    fn test_validate_package_name_invalid_characters_special() {
        let result = validate_package_name("my@project!");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("invalid characters"));
    }

    // =========================================================================
    // NewOptions Construction Tests
    // =========================================================================

    #[test]
    fn test_new_options_for_binary() {
        let args = parse_init_args(&["test", "--name", "mybin"]);
        let path = args.path.unwrap_or_else(|| PathBuf::from("."));
        let name = determine_package_name(&args.name, &path);

        let opts = NewOptions {
            name: name.clone(),
            lib: args.lib,
            init: true,
        };

        assert_eq!(opts.name, "mybin");
        assert!(!opts.lib);
        assert!(opts.init);
    }

    #[test]
    fn test_new_options_for_library() {
        let args = parse_init_args(&["test", "--lib", "--name", "mylib"]);
        let path = args.path.unwrap_or_else(|| PathBuf::from("."));
        let name = determine_package_name(&args.name, &path);

        let opts = NewOptions {
            name: name.clone(),
            lib: args.lib,
            init: true,
        };

        assert_eq!(opts.name, "mylib");
        assert!(opts.lib);
        assert!(opts.init);
    }

    // =========================================================================
    // File System Tests (using tempfile)
    // =========================================================================

    #[test]
    fn test_init_in_temp_directory() {
        let tmp = TempDir::new().unwrap();
        let project_dir = tmp.path().join("test_project");
        std::fs::create_dir(&project_dir).unwrap();

        let name = None;
        let result = determine_package_name(&name, &project_dir);
        assert_eq!(result, "test_project");
    }

    #[test]
    fn test_init_name_from_nested_path() {
        let tmp = TempDir::new().unwrap();
        let nested_dir = tmp.path().join("a").join("b").join("my_nested_project");
        std::fs::create_dir_all(&nested_dir).unwrap();

        let name = None;
        let result = determine_package_name(&name, &nested_dir);
        assert_eq!(result, "my_nested_project");
    }
}
