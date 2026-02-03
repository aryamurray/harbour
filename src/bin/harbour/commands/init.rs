//! `harbour init` command

use std::path::PathBuf;

use anyhow::Result;

use crate::cli::InitArgs;
use harbour::ops::harbour_new::{init_project, NewOptions};


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
    // File System Tests (using tempfile)
    // =========================================================================

}
