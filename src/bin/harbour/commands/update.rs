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
    use crate::cli::{Cli, Commands};
    use clap::Parser;

    /// `harbour update` has no end-to-end test, so this is the only check
    /// that the documented invocations parse: bare, and with packages.
    #[test]
    fn test_update_documented_invocations_parse() {
        let parse = |argv: &[&str]| match Cli::try_parse_from(argv) {
            Ok(Cli {
                command: Commands::Update(args),
                ..
            }) => args,
            Ok(_) => panic!("{argv:?} is not the update command"),
            Err(e) => panic!("{argv:?} must parse: {e}"),
        };

        let args = parse(&["harbour", "update"]);
        assert!(args.packages.is_empty());
        assert!(!args.dry_run);

        let args = parse(&["harbour", "update", "--dry-run", "zlib", "openssl"]);
        assert!(args.dry_run);
        assert_eq!(args.packages, vec!["zlib", "openssl"]);
    }
}
