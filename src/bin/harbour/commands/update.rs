//! `harbour update` command

use anyhow::Result;

use crate::cli::UpdateArgs;
use harbour::core::Workspace;
use harbour::ops::harbour_update::{update, UpdateOptions};
use harbour::sources::SourceCache;
use harbour::util::GlobalContext;

pub fn execute(args: UpdateArgs) -> Result<()> {
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

    if args.dry_run {
        eprintln!("Would update {} packages", resolve.len());
    } else {
        eprintln!("    Updating {} packages", resolve.len());
    }

    Ok(())
}
