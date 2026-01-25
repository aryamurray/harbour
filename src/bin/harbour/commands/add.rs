//! `harbour add` command

use anyhow::{bail, Result};

use crate::cli::AddArgs;
use crate::GlobalOptions;
use harbour::ops::harbour_add::{add_dependency, AddOptions, AddResult, SourceKind};
use harbour::util::{GlobalContext, Status};

pub fn execute(args: AddArgs, global_opts: &GlobalOptions) -> Result<()> {
    let shell = &global_opts.shell;

    // Validate arguments - can't specify both path and git
    if args.path.is_some() && args.git.is_some() {
        shell.error("cannot specify both --path and --git");
        bail!("cannot specify both --path and --git");
    }

    // If neither path nor git specified, it's a registry dependency
    // The version defaults to "*" if not specified

    let ctx = GlobalContext::new()?;

    let manifest_path = ctx.find_manifest()?;

    let opts = AddOptions {
        name: args.name.clone(),
        path: args.path,
        git: args.git,
        branch: args.branch,
        tag: args.tag,
        rev: args.rev,
        version: args.version,
        optional: args.optional,
        dry_run: args.dry_run,
        offline: global_opts.offline,
    };

    let result = add_dependency(&manifest_path, &opts)?;

    // Format source for display
    fn format_source(source: &SourceKind) -> &'static str {
        match source {
            SourceKind::Registry(_) => "registry",
            SourceKind::Git(_) => "git",
            SourceKind::Path(_) => "path",
        }
    }

    // Output result based on what happened
    match result {
        AddResult::Added { name, version, source } => {
            if args.dry_run {
                shell.status(Status::Info, format!("Would add {} v{} ({})", name, version, format_source(&source)));
            } else {
                shell.status(Status::Added, format!("{} v{} ({})", name, version, format_source(&source)));
            }
        }
        AddResult::Updated { name, from, to, source } => {
            if args.dry_run {
                shell.status(Status::Info, format!("Would update {} v{} -> v{} ({})", name, from, to, format_source(&source)));
            } else {
                shell.status(Status::Updated, format!("{} v{} -> v{} ({})", name, from, to, format_source(&source)));
            }
        }
        AddResult::AlreadyPresent { name, version } => {
            shell.status(Status::Skipped, format!("{} v{} (already in dependencies)", name, version));
        }
        AddResult::NotFound { name, looked_in } => {
            let registries = looked_in
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            shell.error(format!("package `{}` not found in registries: {}", name, registries));
            bail!("package `{}` not found", name);
        }
    }

    Ok(())
}
