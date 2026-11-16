//! `harbour build` command

use std::time::Instant;

use anyhow::Result;

use crate::cli::{BuildArgs, MessageFormat};
use crate::GlobalOptions;
use harbour::builder::events::BuildEvent;
use harbour::builder::linkage::LinkagePreference;
use harbour::core::target::{CppStandard, TargetTriple};
use harbour::core::workspace::workspace_manifest_for;
use harbour::core::Workspace;
use harbour::ops::harbour_build::{build, BuildOptions};
use harbour::sources::SourceCache;
use harbour::util::config::{load_config, BACKEND_SELECTION_REMOVED};
use harbour::util::{GlobalContext, Status};

pub fn execute(args: BuildArgs, global_opts: &GlobalOptions) -> Result<()> {
    let shell = &global_opts.shell;
    let start = Instant::now();
    let is_json = args.message_format == MessageFormat::Json;

    let ctx = GlobalContext::new()?;

    // Walk up past the nearest manifest to the workspace that owns it, so
    // that `harbour build` inside a member can see the `[workspace]` table
    // one directory up (#133).
    //
    // The containing member is still *not* turned into a default `-p`
    // selection, but the reason has changed. It used to be that the default
    // would be a lie: selection never reached `BuildPlan`, so it would have
    // printed "Building packages: other" over a build of `app` (#143). That
    // is fixed -- `-p` now selects what it names -- so the default would
    // work, and this is where it belongs. It is left out only because
    // changing what a bare `harbour build` compiles is a separate decision
    // from making `-p` honest, and it deserves its own change.
    let (manifest_path, _containing_member) = workspace_manifest_for(&ctx.find_manifest()?)?;

    let profile = args.profile_name();
    let ws = Workspace::new(&manifest_path, &ctx)?.with_profile(profile.clone());
    // The flags speak for the packages being built (`-p`, or the default
    // members). Set before anything resolves: the selection decides which
    // optional dependencies are in the graph, and the lockfile hash includes it.
    let selection = harbour::ops::harbour_build::feature_selection_for(
        &ws,
        &args.feature_args.features,
        args.feature_args.no_default_features,
        args.feature_args.all_features,
        &args.package,
        None,
    )?;
    let ws = ws.with_feature_selection(selection);

    // Load configuration (global + project)
    let config = load_config(
        &ctx.config_path(),
        &ctx.project_harbour_dir().join("config.toml"),
    );

    config.reject_removed_backend()?;

    // Parse --target-triple flag to TargetTriple
    let target_triple = args.target_triple.as_deref().map(TargetTriple::parse);

    let vcpkg =
        harbour::builder::resolution_vcpkg(&config.vcpkg, target_triple.as_ref(), ws.is_release());
    let mut source_cache = SourceCache::new_with_vcpkg(ctx.cache_dir(), vcpkg)
        .with_default_registry(ctx.default_registry_url()?.as_str());

    // Parse --std flag to CppStandard (CLI overrides config)
    let cpp_std = args
        .std
        .as_ref()
        .or(config.build.cpp_std.as_ref())
        .map(|s| s.parse::<CppStandard>())
        .transpose()
        .map_err(|e| anyhow::anyhow!("{}", e))?;

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

    let target_triple_display = args.target_triple;

    // Jobs: CLI > config > None (auto-detect)
    let jobs = args.jobs.or(config.build.jobs);

    // Emit compile commands: enabled by default, can be disabled with --no-compile-commands
    let emit_compile_commands = !args.no_compile_commands;

    let opts = BuildOptions {
        packages: args.package,
        targets: args.target,
        emit_compile_commands,
        emit_plan: args.plan,
        jobs,
        verbose: shell.is_verbose(),
        cpp_std,
        linkage,
        ffi: args.ffi,
        target_triple,
        locked: global_opts.locked,
        vcpkg: config.vcpkg.clone(),
        strict_sources: args.strict_sources,
        dep_warnings: args.dep_warnings,
    };

    // What the build is for, resolved the way the build resolves it (the flag,
    // else `toolchain.target`, else the host) so this label cannot say `native`
    // over a build that wrote an archive for another architecture.
    let target_label = target_label(
        target_triple_display.as_deref(),
        &harbour::builder::toolchain::resolve_target(opts.target_triple.as_ref()),
    );

    // Emit build started event in JSON mode
    if is_json {
        let event = BuildEvent::started(&profile, &target_label);
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
            for artifact in &result.artifacts {
                shell.status(
                    Status::Finished,
                    format!(
                        "{} [{}] {} in {:.2}s",
                        profile,
                        target_label,
                        artifact.path.display(),
                        elapsed.as_secs_f64()
                    ),
                );
            }
        }
    }

    Ok(())
}

/// The error for `harbour build --backend`, which no longer exists. Every
/// value is refused, `native` and `custom` included: the flag selected nothing
/// about how the build ran, and accepting it would keep advertising a choice
/// that is not there.
pub fn backend_flag_removed() -> anyhow::Error {
    anyhow::anyhow!(
        "`--backend` has been removed\n\
         hint: run the command again without `--backend`\n\
         {BACKEND_SELECTION_REMOVED}"
    )
}

/// Regenerate `compile_commands.json` after the manifest's dependencies changed.
///
/// Called by `harbour add` and `harbour remove`, so an editor's language
/// server sees the new set of include directories without a separate
/// `harbour build`. It resolves and fetches as a default `harbour build`
/// would (default members, default features, host target, config's vcpkg),
/// but compiles nothing. The manifest edit has already succeeded by the time
/// this runs, so a failure (no network, a package that does not build here,
/// ...) is a warning, not an error.
pub fn refresh_compile_commands(global_opts: &GlobalOptions) {
    if let Err(e) = try_refresh_compile_commands(global_opts) {
        global_opts.shell.status(
            Status::Warning,
            format!(
                "could not update compile_commands.json: {e:#}\n\
                 hint: run `harbour build` to update it"
            ),
        );
    }
}

fn try_refresh_compile_commands(global_opts: &GlobalOptions) -> Result<()> {
    let ctx = GlobalContext::new()?;
    let (manifest_path, _) = workspace_manifest_for(&ctx.find_manifest()?)?;
    let ws = Workspace::new(&manifest_path, &ctx)?;
    let selection =
        harbour::ops::harbour_build::feature_selection_for(&ws, &[], false, false, &[], None)?;
    let ws = ws.with_feature_selection(selection);

    let config = load_config(
        &ctx.config_path(),
        &ctx.project_harbour_dir().join("config.toml"),
    );
    config.reject_removed_backend()?;
    let vcpkg = harbour::builder::resolution_vcpkg(&config.vcpkg, None, ws.is_release());
    let mut source_cache = SourceCache::new_with_vcpkg(ctx.cache_dir(), vcpkg)
        .with_default_registry(ctx.default_registry_url()?.as_str());

    let cpp_std = config
        .build
        .cpp_std
        .as_ref()
        .map(|s| s.parse::<CppStandard>())
        .transpose()
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let opts = BuildOptions {
        linkage: config
            .linkage()
            .unwrap_or(LinkagePreference::Auto { prefer: vec![] }),
        cpp_std,
        locked: global_opts.locked,
        vcpkg: config.vcpkg.clone(),
        verbose: global_opts.shell.is_verbose(),
        ..BuildOptions::default()
    };
    harbour::ops::harbour_build::write_compile_commands(&ws, &mut source_cache, &opts)
}

/// The name a build reports for its target: `--target-triple` as typed, else the
/// effective target when it is not the host, else `native`.
///
/// `effective` comes from `resolve_target`, the one resolution the toolchain, the
/// ABI identity and the output directory also use, so the label cannot disagree
/// with what was built. An explicit flag is shown verbatim even when it names the
/// host, as it always was.
fn target_label(explicit: Option<&str>, effective: &TargetTriple) -> String {
    match explicit {
        Some(flag) => flag.to_string(),
        None if effective.is_host() => "native".to_string(),
        None => effective.as_str().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::BuildArgs;
    use clap::Parser;

    /// Helper to parse BuildArgs from command-line strings.
    fn parse_build_args(args: &[&str]) -> BuildArgs {
        #[derive(Parser)]
        struct TestCli {
            #[command(flatten)]
            build: BuildArgs,
        }
        let cli = TestCli::parse_from(args);
        cli.build
    }

    #[test]
    fn test_build_multiple_packages() {
        let args = parse_build_args(&["test", "-p", "pkg1", "-p", "pkg2", "-p", "pkg3"]);
        assert_eq!(args.package, vec!["pkg1", "pkg2", "pkg3"]);
    }

    #[test]
    fn test_build_multiple_targets() {
        let args = parse_build_args(&["test", "--target", "bin1", "--target", "lib1"]);
        assert_eq!(args.target, vec!["bin1", "lib1"]);
    }

    /// The flag still parses -- with any value, or none -- so that it reaches
    /// the rejection rather than clap's "unexpected argument"; and it is
    /// noticed only where it was typed.
    #[test]
    fn test_build_backend_flag_still_parses_and_is_noticed() {
        let used = |argv: &[&str]| {
            let matches = crate::cli::command()
                .try_get_matches_from(argv)
                .unwrap_or_else(|e| panic!("{argv:?} must parse: {e}"));
            crate::cli::uses_removed_backend_flag(&matches)
        };

        assert!(!used(&["harbour", "build"]));
        assert!(!used(&["harbour", "build", "--release"]));
        assert!(!used(&["harbour", "tree"]));
        for value in ["native", "custom", "cmake", "meson"] {
            assert!(used(&["harbour", "build", "--backend", value]));
            assert!(used(&["harbour", "build", &format!("--backend={value}")]));
            assert!(used(&[
                "harbour",
                "build",
                "-r",
                "--backend",
                value,
                "-j",
                "2"
            ]));
        }
        assert!(used(&["harbour", "build", "--backend"]));
        assert!(used(&["harbour", "build", "--backend", "--release"]));
    }

    #[test]
    fn test_build_backend_flag_error_says_what_replaced_it() {
        let err = backend_flag_removed().to_string();
        assert!(err.contains("`--backend` has been removed"), "{err}");
        assert!(err.contains("always builds natively"), "{err}");
        assert!(err.contains("type = \"custom\""), "{err}");
    }

    /// What help and completions are generated from does not have the flag.
    #[test]
    fn test_build_backend_flag_is_not_in_the_declared_command() {
        use clap::CommandFactory;
        let declared = crate::cli::Cli::command();
        let build = declared.find_subcommand("build").unwrap();
        assert!(build.get_arguments().all(|a| a.get_id() != "backend"));
    }

    /// README documents `--linkage=auto` as the default.
    #[test]
    fn test_build_linkage_auto_default() {
        let args = parse_build_args(&["test"]);
        assert_eq!(args.linkage, "auto");
    }

    #[test]
    fn the_label_is_native_for_a_host_build_and_the_target_otherwise() {
        let host = TargetTriple::host();
        let other = TargetTriple::parse("riscv64gc-unknown-linux-gnu");
        assert_eq!(target_label(None, &host), "native");
        assert_eq!(target_label(None, &other), "riscv64gc-unknown-linux-gnu");
        // The flag is shown as typed, whatever it resolves to.
        assert_eq!(
            target_label(Some("aarch64-apple-darwin"), &host),
            "aarch64-apple-darwin"
        );
        assert_eq!(
            target_label(Some("riscv64gc-unknown-linux-gnu"), &other),
            "riscv64gc-unknown-linux-gnu"
        );
    }
}
