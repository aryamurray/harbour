//! `harbour update` command

use anyhow::Result;

use crate::cli::UpdateArgs;
use crate::GlobalOptions;
use harbour::core::Workspace;
use harbour::ops::harbour_update::{update, UpdateOptions};
use harbour::sources::SourceCache;
use harbour::util::{GlobalContext, Status};

pub fn execute(args: UpdateArgs, global_opts: &GlobalOptions) -> Result<()> {
    let shell = &global_opts.shell;
    let ctx = GlobalContext::new()?;

    let manifest_path = ctx.find_manifest()?;

    let ws = Workspace::new(&manifest_path, &ctx)?;
    let mut source_cache = SourceCache::new(ctx.cache_dir());

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
