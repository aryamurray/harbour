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
            shell.status(Status::Warning, format!("dependency `{}` not found in Harbour.toml", name));
        }
    }

    Ok(())
}
