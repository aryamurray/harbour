//! `harbour remove` command

use anyhow::Result;

use crate::cli::RemoveArgs;
use crate::GlobalOptions;
use harbour::ops::harbour_add::{remove_dependency, RemoveOptions, RemoveResult};
use harbour::util::{GlobalContext, Status};

pub fn execute(args: RemoveArgs, global_opts: &GlobalOptions) -> Result<()> {
    let shell = &global_opts.shell;
    let ctx = GlobalContext::new()?;

    let manifest_path = harbour::ops::harbour_add::manifest_to_edit(&ctx, args.package.as_deref())?;

    let opts = RemoveOptions {
        dry_run: args.dry_run,
    };

    let result = remove_dependency(&manifest_path, &args.name, &opts)?;

    match result {
        RemoveResult::Removed {
            name,
            version,
            targets,
        } => {
            let from_targets = if targets.is_empty() {
                String::new()
            } else {
                let list: Vec<String> = targets.iter().map(|t| format!("`{t}`")).collect();
                format!(" (and from the deps of {})", list.join(", "))
            };
            if args.dry_run {
                shell.status(
                    Status::Info,
                    format!("Would remove {name} v{version}{from_targets}"),
                );
            } else {
                shell.status(Status::Removed, format!("{name} v{version}{from_targets}"));
                // The removed package's include directories should leave the
                // editor's view too.
                super::build::refresh_compile_commands(global_opts);
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
