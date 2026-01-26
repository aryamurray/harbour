//! `harbour remove` command

use anyhow::Result;

use crate::cli::RemoveArgs;
use crate::GlobalOptions;
use harbour::ops::harbour_add::{remove_dependency, RemoveOptions, RemoveResult};
use harbour::util::{GlobalContext, Status};

pub fn execute(args: RemoveArgs, global_opts: &GlobalOptions) -> Result<()> {
    let shell = &global_opts.shell;
    let ctx = GlobalContext::new()?;

    let manifest_path = ctx.find_manifest()?;

    let opts = RemoveOptions {
        dry_run: args.dry_run,
    };

    let result = remove_dependency(&manifest_path, &args.name, &opts)?;

    match result {
        RemoveResult::Removed { name, version } => {
            if args.dry_run {
                shell.status(Status::Info, format!("Would remove {} v{}", name, version));
            } else {
                shell.status(Status::Removed, format!("{} v{}", name, version));
            }
        }
        RemoveResult::NotFound { name } => {
            shell.status(
                Status::Warning,
                format!("dependency `{}` not found in Harbour.toml", name),
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::RemoveArgs;
    use clap::Parser;

    /// Helper to parse RemoveArgs from command-line strings.
    fn parse_remove_args(args: &[&str]) -> RemoveArgs {
        #[derive(Parser)]
        struct TestCli {
            #[command(flatten)]
            remove: RemoveArgs,
        }
        let cli = TestCli::parse_from(args);
        cli.remove
    }

    // =========================================================================
    // RemoveArgs Default Values Tests
    // =========================================================================

    #[test]
    fn test_remove_args_defaults() {
        let args = parse_remove_args(&["test", "zlib"]);

        assert_eq!(args.name, "zlib");
        assert!(!args.dry_run);
    }

    // =========================================================================
    // Package Name Tests
    // =========================================================================

    #[test]
    fn test_remove_simple_package_name() {
        let args = parse_remove_args(&["test", "openssl"]);
        assert_eq!(args.name, "openssl");
    }

    #[test]
    fn test_remove_package_name_with_hyphen() {
        let args = parse_remove_args(&["test", "my-package"]);
        assert_eq!(args.name, "my-package");
    }

    #[test]
    fn test_remove_package_name_with_underscore() {
        let args = parse_remove_args(&["test", "my_package"]);
        assert_eq!(args.name, "my_package");
    }

    #[test]
    fn test_remove_package_with_numbers() {
        let args = parse_remove_args(&["test", "lib123"]);
        assert_eq!(args.name, "lib123");
    }

    // =========================================================================
    // Dry Run Tests
    // =========================================================================

    #[test]
    fn test_remove_dry_run() {
        let args = parse_remove_args(&["test", "zlib", "--dry-run"]);
        assert!(args.dry_run);
    }

    #[test]
    fn test_remove_no_dry_run_by_default() {
        let args = parse_remove_args(&["test", "zlib"]);
        assert!(!args.dry_run);
    }

    // =========================================================================
    // RemoveOptions Construction Tests
    // =========================================================================

    #[test]
    fn test_remove_options_from_args() {
        let args = parse_remove_args(&["test", "openssl", "--dry-run"]);

        let opts = RemoveOptions {
            dry_run: args.dry_run,
        };

        assert!(opts.dry_run);
    }

    #[test]
    fn test_remove_options_no_dry_run() {
        let args = parse_remove_args(&["test", "openssl"]);

        let opts = RemoveOptions {
            dry_run: args.dry_run,
        };

        assert!(!opts.dry_run);
    }

    // =========================================================================
    // Multiple Invocations Test (order matters)
    // =========================================================================

    #[test]
    fn test_remove_different_packages() {
        let args1 = parse_remove_args(&["test", "zlib"]);
        let args2 = parse_remove_args(&["test", "openssl"]);
        let args3 = parse_remove_args(&["test", "libcurl"]);

        assert_eq!(args1.name, "zlib");
        assert_eq!(args2.name, "openssl");
        assert_eq!(args3.name, "libcurl");
    }
}
