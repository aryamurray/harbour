//! `harbour build` command

use std::time::Instant;

use anyhow::Result;

use crate::cli::{BuildArgs, MessageFormat};
use crate::GlobalOptions;
use harbour::builder::events::BuildEvent;
use harbour::builder::shim::{BackendId, LinkagePreference, TargetTriple};
use harbour::core::target::CppStandard;
use harbour::core::Workspace;
use harbour::ops::harbour_build::{build, BuildOptions};
use harbour::sources::SourceCache;
use harbour::util::config::load_config;
use harbour::util::{GlobalContext, Status};

pub fn execute(args: BuildArgs, global_opts: &GlobalOptions) -> Result<()> {
    let shell = &global_opts.shell;
    let start = Instant::now();
    let is_json = args.message_format == MessageFormat::Json;

    let ctx = GlobalContext::new()?;

    let manifest_path = ctx.find_manifest()?;

    let profile = if args.release { "release" } else { "debug" };
    let ws = Workspace::new(&manifest_path, &ctx)?.with_profile(profile);

    let mut source_cache = SourceCache::new(ctx.cache_dir());

    // Load configuration (global + project)
    let config = load_config(
        &ctx.config_path(),
        &ctx.project_harbour_dir().join("config.toml"),
    );

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
        Some(
            b.parse::<BackendId>()
                .map_err(|e| anyhow::anyhow!("invalid backend: {}", e))?,
        )
    } else {
        config.backend()
    };

    // Parse --linkage flag to LinkagePreference (CLI overrides config if not "auto" default)
    let linkage = if args.linkage != "auto" {
        args.linkage
            .parse::<LinkagePreference>()
            .map_err(|e| anyhow::anyhow!("invalid linkage: {}", e))?
    } else {
        config
            .linkage()
            .unwrap_or(LinkagePreference::Auto { prefer: vec![] })
    };

    // Parse --target-triple flag to TargetTriple
    let target_triple = args.target_triple.clone().map(TargetTriple::new);
    let target_triple_display = args.target_triple;

    // Jobs: CLI > config > None (auto-detect)
    let jobs = args.jobs.or(config.build.jobs);

    // Emit compile commands: enabled by default, can be disabled with --no-compile-commands
    let emit_compile_commands = !args.no_compile_commands;

    let opts = BuildOptions {
        release: args.release,
        packages: args.package,
        targets: args.target,
        emit_compile_commands,
        emit_plan: args.plan,
        jobs,
        verbose: shell.is_verbose(),
        cpp_std,
        backend,
        linkage,
        ffi: args.ffi,
        target_triple,
        locked: global_opts.locked,
    };

    // Emit build started event in JSON mode
    if is_json {
        let event = BuildEvent::started(profile, "native");
        println!("{}", event.to_json());
    }

    let result = build(&ws, &mut source_cache, &opts)?;

    let elapsed = start.elapsed();
    let duration_ms = elapsed.as_millis() as u64;

    if !args.plan {
        if is_json {
            // Emit artifact events
            for artifact in &result.artifacts {
                let event = BuildEvent::artifact(
                    format!(
                        "{} v{}",
                        ws.root_package().name(),
                        ws.root_package().version()
                    ),
                    &artifact.target,
                    vec![artifact.path.clone()],
                );
                println!("{}", event.to_json());
            }

            // Emit finished event
            let event = BuildEvent::BuildFinished {
                success: true,
                duration_ms,
                targets_built: Some(result.artifacts.len() as u64),
            };
            println!("{}", event.to_json());
        } else {
            // Human-readable output
            let target_triple_str = target_triple_display
                .as_ref()
                .map(|t| t.as_str())
                .unwrap_or("native");

            for artifact in &result.artifacts {
                shell.status(
                    Status::Finished,
                    format!(
                        "{} [{}] {} in {:.2}s",
                        profile,
                        target_triple_str,
                        artifact.path.display(),
                        elapsed.as_secs_f64()
                    ),
                );
            }
        }
    }

    Ok(())
}
