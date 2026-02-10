//! `harbour update` command

use anyhow::Result;

use crate::cli::UpdateArgs;
use crate::GlobalOptions;
use harbour::core::abi::TargetTriple;
use harbour::core::Workspace;
use harbour::ops::harbour_update::{update, UpdateOptions};
use harbour::sources::SourceCache;
use harbour::util::config::load_config;
use harbour::util::{GlobalContext, Status, VcpkgIntegration};

pub fn execute(args: UpdateArgs, global_opts: &GlobalOptions) -> Result<()> {
    let shell = &global_opts.shell;
    let ctx = GlobalContext::new()?;

    let manifest_path = ctx.find_manifest()?;

    let ws = Workspace::new(&manifest_path, &ctx)?;
    let config = load_config(
        &ctx.config_path(),
        &ctx.project_harbour_dir().join("config.toml"),
    );
    let vcpkg = VcpkgIntegration::from_config(&config.vcpkg, &TargetTriple::host(), false);
    let mut source_cache = SourceCache::new_with_vcpkg(ctx.cache_dir(), vcpkg);

    let opts = UpdateOptions {
        packages: args.packages,
        aggressive: false,
        dry_run: args.dry_run,
    };

    let resolve = update(&ws, &mut source_cache, &opts)?;

    let pkg_count = resolve.len();
    if args.dry_run {
        shell.status(Status::Info, format!("Would update {} packages", pkg_count));
    } else {
        shell.status(Status::Updated, format!("{} packages", pkg_count));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::UpdateArgs;
    use clap::Parser;

    /// Helper to parse UpdateArgs from command-line strings.
    fn parse_update_args(args: &[&str]) -> UpdateArgs {
        #[derive(Parser)]
        struct TestCli {
            #[command(flatten)]
            update: UpdateArgs,
        }
        let cli = TestCli::parse_from(args);
        cli.update
    }

    // =========================================================================
    // UpdateArgs Default Values Tests
    // =========================================================================

    #[test]
    fn test_update_args_defaults() {
        let args = parse_update_args(&["test"]);

        assert!(args.packages.is_empty());
        assert!(!args.dry_run);
    }

    // =========================================================================
    // Package Selection Tests
    // =========================================================================

    #[test]
    fn test_update_single_package() {
        let args = parse_update_args(&["test", "zlib"]);
        assert_eq!(args.packages, vec!["zlib"]);
    }

    #[test]
    fn test_update_multiple_packages() {
        let args = parse_update_args(&["test", "zlib", "openssl", "curl"]);
        assert_eq!(args.packages, vec!["zlib", "openssl", "curl"]);
    }

    #[test]
    fn test_update_all_packages() {
        // No packages specified means update all
        let args = parse_update_args(&["test"]);
        assert!(args.packages.is_empty());
    }

    // =========================================================================
    // Dry Run Tests
    // =========================================================================

    #[test]
    fn test_update_dry_run() {
        let args = parse_update_args(&["test", "--dry-run"]);
        assert!(args.dry_run);
    }

    #[test]
    fn test_update_dry_run_with_packages() {
        let args = parse_update_args(&["test", "--dry-run", "zlib", "openssl"]);
        assert!(args.dry_run);
        assert_eq!(args.packages, vec!["zlib", "openssl"]);
    }

    // =========================================================================
    // UpdateOptions Construction Tests
    // =========================================================================

    #[test]
    fn test_update_options_from_args_all() {
        let args = parse_update_args(&["test"]);

        let opts = UpdateOptions {
            packages: args.packages.clone(),
            aggressive: false,
            dry_run: args.dry_run,
        };

        assert!(opts.packages.is_empty());
        assert!(!opts.aggressive);
        assert!(!opts.dry_run);
    }

    #[test]
    fn test_update_options_from_args_specific() {
        let args = parse_update_args(&["test", "zlib", "--dry-run"]);

        let opts = UpdateOptions {
            packages: args.packages.clone(),
            aggressive: false,
            dry_run: args.dry_run,
        };

        assert_eq!(opts.packages, vec!["zlib"]);
        assert!(!opts.aggressive);
        assert!(opts.dry_run);
    }

    // =========================================================================
    // Edge Cases Tests
    // =========================================================================

    #[test]
    fn test_update_package_name_with_version_suffix() {
        // Package names might have version-like suffixes in some cases
        let args = parse_update_args(&["test", "boost-1.80"]);
        assert_eq!(args.packages, vec!["boost-1.80"]);
    }

    #[test]
    fn test_update_package_name_with_special_chars() {
        let args = parse_update_args(&["test", "json-c", "lib_xml2"]);
        assert_eq!(args.packages, vec!["json-c", "lib_xml2"]);
    }
}
