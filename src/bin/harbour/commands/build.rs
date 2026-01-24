//! `harbour build` command

use anyhow::{bail, Result};

use crate::cli::BuildArgs;
use harbour::core::Workspace;
use harbour::ops::harbour_build::{build, BuildOptions};
use harbour::sources::SourceCache;
use harbour::util::GlobalContext;

pub fn execute(args: BuildArgs) -> Result<()> {
    let ctx = GlobalContext::new()?;

    let manifest_path = ctx.find_manifest()?;

    let profile = if args.release { "release" } else { "debug" };
    let ws = Workspace::new(&manifest_path, &ctx)?.with_profile(profile);

    let mut source_cache = SourceCache::new(ctx.cache_dir());

    let opts = BuildOptions {
        release: args.release,
        targets: args.target,
        emit_compile_commands: args.emit_compile_commands,
        emit_plan: args.plan,
        jobs: args.jobs,
        verbose: false,
    };

    let result = build(&ws, &mut source_cache, &opts)?;

    if !args.plan {
        for artifact in &result.artifacts {
            eprintln!(
                "    Finished `{}` -> {}",
                artifact.target,
                artifact.path.display()
            );
        }
    }

    Ok(())
}
