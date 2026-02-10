//! `harbour build` command

use std::time::Instant;

use anyhow::Result;

use crate::cli::{BuildArgs, MessageFormat};
use crate::GlobalOptions;
use harbour::builder::events::BuildEvent;
use harbour::builder::shim::{BackendId, LinkagePreference, TargetTriple};
use harbour::core::abi::TargetTriple as AbiTargetTriple;
use harbour::core::target::CppStandard;
use harbour::core::Workspace;
use harbour::ops::harbour_build::{build, BuildOptions};
use harbour::sources::SourceCache;
use harbour::util::config::load_config;
use harbour::util::VcpkgIntegration;
use harbour::util::{GlobalContext, Status};

pub fn execute(args: BuildArgs, global_opts: &GlobalOptions) -> Result<()> {
    let shell = &global_opts.shell;
    let start = Instant::now();
    let is_json = args.message_format == MessageFormat::Json;

    let ctx = GlobalContext::new()?;

    let manifest_path = ctx.find_manifest()?;

    let profile = if args.release { "release" } else { "debug" };
    let ws = Workspace::new(&manifest_path, &ctx)?.with_profile(profile);

    // Load configuration (global + project)
    let config = load_config(
        &ctx.config_path(),
        &ctx.project_harbour_dir().join("config.toml"),
    );

    let vcpkg =
        VcpkgIntegration::from_config(&config.vcpkg, &AbiTargetTriple::host(), args.release);
    let mut source_cache = SourceCache::new_with_vcpkg(ctx.cache_dir(), vcpkg);

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
        vcpkg: config.vcpkg.clone(),
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

    // =========================================================================
    // BuildArgs Default Values Tests
    // =========================================================================

    #[test]
    fn test_build_args_defaults() {
        let args = parse_build_args(&["test"]);

        assert!(!args.release);
        assert!(args.package.is_empty());
        assert!(args.target.is_empty());
        assert!(!args.no_compile_commands);
        assert!(!args.plan);
        assert!(args.jobs.is_none());
        assert!(args.std.is_none());
        assert!(args.backend.is_none());
        assert_eq!(args.linkage, "auto");
        assert!(!args.ffi);
        assert!(args.target_triple.is_none());
        assert_eq!(args.message_format, MessageFormat::Human);
    }

    // =========================================================================
    // Release Flag Tests
    // =========================================================================

    #[test]
    fn test_build_with_release_short_flag() {
        let args = parse_build_args(&["test", "-r"]);
        assert!(args.release);
    }

    #[test]
    fn test_build_with_release_long_flag() {
        let args = parse_build_args(&["test", "--release"]);
        assert!(args.release);
    }

    // =========================================================================
    // Package Selection Tests
    // =========================================================================

    #[test]
    fn test_build_single_package() {
        let args = parse_build_args(&["test", "-p", "mypackage"]);
        assert_eq!(args.package, vec!["mypackage"]);
    }

    #[test]
    fn test_build_multiple_packages() {
        let args = parse_build_args(&["test", "-p", "pkg1", "-p", "pkg2", "-p", "pkg3"]);
        assert_eq!(args.package, vec!["pkg1", "pkg2", "pkg3"]);
    }

    #[test]
    fn test_build_package_long_flag() {
        let args = parse_build_args(&["test", "--package", "mylib"]);
        assert_eq!(args.package, vec!["mylib"]);
    }

    // =========================================================================
    // Target Selection Tests
    // =========================================================================

    #[test]
    fn test_build_single_target() {
        let args = parse_build_args(&["test", "--target", "mybin"]);
        assert_eq!(args.target, vec!["mybin"]);
    }

    #[test]
    fn test_build_multiple_targets() {
        let args = parse_build_args(&["test", "--target", "bin1", "--target", "lib1"]);
        assert_eq!(args.target, vec!["bin1", "lib1"]);
    }

    // =========================================================================
    // Jobs/Parallelism Tests
    // =========================================================================

    #[test]
    fn test_build_jobs_short_flag() {
        let args = parse_build_args(&["test", "-j", "4"]);
        assert_eq!(args.jobs, Some(4));
    }

    #[test]
    fn test_build_jobs_long_flag() {
        let args = parse_build_args(&["test", "--jobs", "8"]);
        assert_eq!(args.jobs, Some(8));
    }

    #[test]
    fn test_build_jobs_single_threaded() {
        let args = parse_build_args(&["test", "-j", "1"]);
        assert_eq!(args.jobs, Some(1));
    }

    // =========================================================================
    // C++ Standard Tests
    // =========================================================================

    #[test]
    fn test_build_cpp_std_11() {
        let args = parse_build_args(&["test", "--std", "11"]);
        assert_eq!(args.std, Some("11".to_string()));
    }

    #[test]
    fn test_build_cpp_std_17() {
        let args = parse_build_args(&["test", "--std", "17"]);
        assert_eq!(args.std, Some("17".to_string()));
    }

    #[test]
    fn test_build_cpp_std_20() {
        let args = parse_build_args(&["test", "--std", "20"]);
        assert_eq!(args.std, Some("20".to_string()));
    }

    #[test]
    fn test_build_cpp_std_23() {
        let args = parse_build_args(&["test", "--std", "23"]);
        assert_eq!(args.std, Some("23".to_string()));
    }

    // =========================================================================
    // Backend Selection Tests
    // =========================================================================

    #[test]
    fn test_build_backend_native() {
        let args = parse_build_args(&["test", "--backend", "native"]);
        assert_eq!(args.backend, Some("native".to_string()));
    }

    #[test]
    fn test_build_backend_cmake() {
        let args = parse_build_args(&["test", "--backend", "cmake"]);
        assert_eq!(args.backend, Some("cmake".to_string()));
    }

    #[test]
    fn test_build_backend_meson() {
        let args = parse_build_args(&["test", "--backend", "meson"]);
        assert_eq!(args.backend, Some("meson".to_string()));
    }

    // =========================================================================
    // Linkage Preference Tests
    // =========================================================================

    #[test]
    fn test_build_linkage_static() {
        let args = parse_build_args(&["test", "--linkage", "static"]);
        assert_eq!(args.linkage, "static");
    }

    #[test]
    fn test_build_linkage_shared() {
        let args = parse_build_args(&["test", "--linkage", "shared"]);
        assert_eq!(args.linkage, "shared");
    }

    #[test]
    fn test_build_linkage_auto_default() {
        let args = parse_build_args(&["test"]);
        assert_eq!(args.linkage, "auto");
    }

    // =========================================================================
    // FFI Flag Tests
    // =========================================================================

    #[test]
    fn test_build_ffi_flag() {
        let args = parse_build_args(&["test", "--ffi"]);
        assert!(args.ffi);
    }

    #[test]
    fn test_build_no_ffi_by_default() {
        let args = parse_build_args(&["test"]);
        assert!(!args.ffi);
    }

    // =========================================================================
    // Target Triple (Cross-Compilation) Tests
    // =========================================================================

    #[test]
    fn test_build_target_triple_linux() {
        let args = parse_build_args(&["test", "--target-triple", "x86_64-unknown-linux-gnu"]);
        assert_eq!(
            args.target_triple,
            Some("x86_64-unknown-linux-gnu".to_string())
        );
    }

    #[test]
    fn test_build_target_triple_windows() {
        let args = parse_build_args(&["test", "--target-triple", "x86_64-pc-windows-msvc"]);
        assert_eq!(
            args.target_triple,
            Some("x86_64-pc-windows-msvc".to_string())
        );
    }

    #[test]
    fn test_build_target_triple_macos() {
        let args = parse_build_args(&["test", "--target-triple", "aarch64-apple-darwin"]);
        assert_eq!(args.target_triple, Some("aarch64-apple-darwin".to_string()));
    }

    // =========================================================================
    // Message Format Tests
    // =========================================================================

    #[test]
    fn test_build_message_format_human() {
        let args = parse_build_args(&["test", "--message-format", "human"]);
        assert_eq!(args.message_format, MessageFormat::Human);
    }

    #[test]
    fn test_build_message_format_json() {
        let args = parse_build_args(&["test", "--message-format", "json"]);
        assert_eq!(args.message_format, MessageFormat::Json);
    }

    // =========================================================================
    // Plan Flag Tests
    // =========================================================================

    #[test]
    fn test_build_plan_flag() {
        let args = parse_build_args(&["test", "--plan"]);
        assert!(args.plan);
    }

    // =========================================================================
    // Compile Commands Flag Tests
    // =========================================================================

    #[test]
    fn test_build_no_compile_commands() {
        let args = parse_build_args(&["test", "--no-compile-commands"]);
        assert!(args.no_compile_commands);
    }

    // =========================================================================
    // Combined Flags Tests
    // =========================================================================

    #[test]
    fn test_build_complex_invocation() {
        let args = parse_build_args(&[
            "test",
            "--release",
            "-p",
            "mylib",
            "--target",
            "mybin",
            "-j",
            "4",
            "--std",
            "17",
            "--backend",
            "native",
            "--linkage",
            "static",
        ]);

        assert!(args.release);
        assert_eq!(args.package, vec!["mylib"]);
        assert_eq!(args.target, vec!["mybin"]);
        assert_eq!(args.jobs, Some(4));
        assert_eq!(args.std, Some("17".to_string()));
        assert_eq!(args.backend, Some("native".to_string()));
        assert_eq!(args.linkage, "static");
    }
}
