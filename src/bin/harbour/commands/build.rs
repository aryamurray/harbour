//! `harbour build` command

use anyhow::Result;

use crate::cli::BuildArgs;
use harbour::builder::shim::{BackendId, LinkagePreference, TargetTriple};
use harbour::core::target::CppStandard;
use harbour::core::Workspace;
use harbour::ops::harbour_build::{build, BuildOptions};
use harbour::sources::SourceCache;
use harbour::util::config::load_config;
use harbour::util::GlobalContext;

pub fn execute(args: BuildArgs) -> Result<()> {
    let ctx = GlobalContext::new()?;

    let manifest_path = ctx.find_manifest()?;

    let profile = if args.release { "release" } else { "debug" };
    let ws = Workspace::new(&manifest_path, &ctx)?.with_profile(profile);

    let mut source_cache = SourceCache::new(ctx.cache_dir());

    // Load configuration (global + project)
    let config = load_config(&ctx.config_path(), &ctx.project_harbour_dir().join("config.toml"));

    // Parse --std flag to CppStandard (CLI overrides config)
    let cpp_std = args
        .std
        .as_ref()
        .or(config.build.cpp_std.as_ref())
        .map(|s| s.parse::<CppStandard>())
        .transpose()
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    // Parse --backend flag to BackendId (CLI overrides config)
    let backend = if let Some(ref b) = args.backend {
        Some(b.parse::<BackendId>().map_err(|e| anyhow::anyhow!("invalid backend: {}", e))?)
    } else {
        config.backend()
    };

    // Parse --linkage flag to LinkagePreference (CLI overrides config if not "auto" default)
    let linkage = if args.linkage != "auto" {
        args.linkage
            .parse::<LinkagePreference>()
            .map_err(|e| anyhow::anyhow!("invalid linkage: {}", e))?
    } else {
        config.linkage().unwrap_or(LinkagePreference::Auto { prefer: vec![] })
    };

    // Parse --target-triple flag to TargetTriple
    let target_triple = args.target_triple.map(TargetTriple::new);

    // Jobs: CLI > config > None (auto-detect)
    let jobs = args.jobs.or(config.build.jobs);

    // Emit compile commands: CLI flag OR config setting
    let emit_compile_commands = args.emit_compile_commands || config.build.emit_compile_commands;

    let opts = BuildOptions {
        release: args.release,
        packages: args.package,
        targets: args.target,
        emit_compile_commands,
        emit_plan: args.plan,
        jobs,
        verbose: false,
        cpp_std,
        backend,
        linkage,
        ffi: args.ffi,
        target_triple,
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
