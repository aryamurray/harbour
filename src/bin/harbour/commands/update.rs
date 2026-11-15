//! `harbour update` command

use anyhow::Result;

use crate::cli::UpdateArgs;
use crate::GlobalOptions;
use harbour::core::workspace::workspace_manifest_for;
use harbour::core::Workspace;
use harbour::ops::harbour_update::{update, PackageChange, UpdateOptions};
use harbour::sources::SourceCache;
use harbour::util::config::load_config;
use harbour::util::{GlobalContext, Status};

pub fn execute(args: UpdateArgs, global_opts: &GlobalOptions) -> Result<()> {
    let shell = &global_opts.shell;
    let ctx = GlobalContext::new()?;

    let (manifest_path, _) = workspace_manifest_for(&ctx.find_manifest()?)?;

    let ws = Workspace::new(&manifest_path, &ctx)?;
    let config = load_config(
        &ctx.config_path(),
        &ctx.project_harbour_dir().join("config.toml"),
    );
    let vcpkg = harbour::builder::resolution_vcpkg(&config.vcpkg, None, false);
    // Finding newer versions is the point of `update`, so every registry is
    // fetched rather than trusted for a few minutes as other commands do.
    let mut source_cache = SourceCache::new_with_vcpkg(ctx.cache_dir(), vcpkg)
        .with_default_registry(ctx.default_registry_url()?.as_str())
        .with_registry_max_age(std::time::Duration::ZERO);

    let opts = UpdateOptions {
        packages: args.packages,
        aggressive: false,
        dry_run: args.dry_run,
    };

    let report = update(&ws, &mut source_cache, &opts)?;

    if report.changes.is_empty() {
        shell.status(Status::Info, "Harbour.lock is already up to date");
        return Ok(());
    }
    for change in &report.changes {
        let (verb, what) = describe(change, args.dry_run);
        shell.status(verb, what);
    }
    if args.dry_run {
        shell.status(
            Status::Info,
            format!(
                "{} change{} not written (dry run)",
                report.changes.len(),
                if report.changes.len() == 1 { "" } else { "s" }
            ),
        );
    }

    Ok(())
}

/// The status line for one lockfile change.
fn describe(change: &PackageChange, dry_run: bool) -> (Status, String) {
    let (status, would, what) = match change {
        PackageChange::Added { name, version } => {
            (Status::Added, "add", format!("{name} v{version}"))
        }
        PackageChange::Updated { name, from, to } => (
            Status::Updated,
            "update",
            format!("{name} v{from} -> v{to}"),
        ),
        PackageChange::Removed { name, version } => {
            (Status::Removed, "remove", format!("{name} v{version}"))
        }
    };
    if dry_run {
        (Status::Info, format!("Would {would} {what}"))
    } else {
        (status, what)
    }
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
