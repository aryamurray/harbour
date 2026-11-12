//! `harbour run` command

use std::io::Write;
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::cli::RunArgs;
use crate::commands::test::is_test_target;
use crate::GlobalOptions;
use harbour::builder::shim::LinkagePreference;
use harbour::core::target::{Target, TargetKind, TargetTriple};
use harbour::core::workspace::workspace_manifest_for;
use harbour::core::Workspace;
use harbour::ops::harbour_build::{build, select_packages, BuildOptions};
use harbour::sources::SourceCache;
use harbour::util::config::load_config;
use harbour::util::{GlobalContext, Status};

/// Pick the executable to run from a package's targets.
///
/// Host targets are build-time tools for generators, not the package's
/// programs, so they are never candidates. Without a name, test-pattern
/// executables (the ones `harbour test` runs) are skipped too; naming one
/// explicitly is still allowed.
fn select_exe<'a>(
    package: &str,
    targets: &'a [Target],
    requested: Option<&str>,
) -> Result<&'a Target> {
    let exes: Vec<&Target> = targets
        .iter()
        .filter(|t| t.kind == TargetKind::Exe && !t.host)
        .collect();
    let list = |ts: &[&Target]| {
        ts.iter()
            .map(|t| format!("  {}", t.name))
            .collect::<Vec<_>>()
            .join("\n")
    };

    if let Some(name) = requested {
        return match exes.iter().find(|t| t.name.as_str() == name) {
            Some(t) => Ok(t),
            None if exes.is_empty() => bail!(
                "package `{package}` has no executable target, so `{name}` cannot be run\n\
                 hint: add a target with `kind = \"exe\"` to Harbour.toml"
            ),
            None => bail!(
                "`{name}` is not an executable target of package `{package}`\n\
                 available executable targets:\n{}",
                list(&exes)
            ),
        };
    }

    let candidates: Vec<&Target> = exes
        .iter()
        .copied()
        .filter(|t| !is_test_target(t.name.as_str()))
        .collect();
    match candidates.as_slice() {
        [one] => Ok(one),
        [] => bail!(
            "package `{package}` has no executable target to run\n\
             hint: add a target with `kind = \"exe\"` to Harbour.toml"
        ),
        many => bail!(
            "package `{package}` has several executable targets; name the one to run:\n{}\n\
             hint: harbour run <TARGET>",
            list(many)
        ),
    }
}

pub fn execute(args: RunArgs, global_opts: &GlobalOptions) -> Result<()> {
    let shell = &global_opts.shell;
    let ctx = GlobalContext::new()?;

    let (manifest_path, containing_member) = workspace_manifest_for(&ctx.find_manifest()?)?;

    let profile = args.profile_name();
    let ws = Workspace::new(&manifest_path, &ctx)?.with_profile(profile);

    // The package to run: `-p`, else the one the command was run from.
    let package = match &args.package {
        Some(name) => select_packages(&ws, std::slice::from_ref(name))?
            .first()
            .map(|p| p.name().as_str().to_string())
            .with_context(|| format!("package `{name}` not found"))?,
        None => ws
            .subject_package(containing_member)
            .name()
            .as_str()
            .to_string(),
    };

    let selection = harbour::ops::harbour_build::feature_selection_for(
        &ws,
        &args.feature_args.features,
        args.feature_args.no_default_features,
        args.feature_args.all_features,
        std::slice::from_ref(&package),
        None,
    )?;
    let ws = ws.with_feature_selection(selection);

    let target_triple = args.target_triple.as_deref().map(TargetTriple::parse);

    // A foreign binary cannot be exec'd; say so before spending a build on it.
    let effective = harbour::builder::toolchain::resolve_target(target_triple.as_ref());
    if !effective.is_host() {
        bail!(
            "cannot run a binary built for `{}` on this machine ({})\n\
             hint: use `harbour build --target-triple {}` to build it without running",
            effective,
            TargetTriple::host(),
            effective
        );
    }

    let exe_name = {
        let pkg = select_packages(&ws, std::slice::from_ref(&package))?[0];
        select_exe(&package, pkg.targets(), args.target.as_deref())?
            .name
            .to_string()
    };

    let config = load_config(
        &ctx.config_path(),
        &ctx.project_harbour_dir().join("config.toml"),
    );
    let vcpkg =
        harbour::builder::resolution_vcpkg(&config.vcpkg, target_triple.as_ref(), ws.is_release());
    let mut source_cache = SourceCache::new_with_vcpkg(ctx.cache_dir(), vcpkg)
        .with_default_registry(ctx.default_registry_url()?.as_str());

    let opts = BuildOptions {
        packages: vec![package.clone()],
        targets: vec![exe_name.clone()],
        emit_compile_commands: false,
        emit_plan: false,
        jobs: args.jobs.or(config.build.jobs),
        verbose: shell.is_verbose(),
        cpp_std: None,
        backend: None,
        linkage: LinkagePreference::Auto { prefer: vec![] },
        ffi: false,
        target_triple,
        locked: global_opts.locked,
        vcpkg: config.vcpkg.clone(),
    };

    let result = build(&ws, &mut source_cache, &opts)?;

    let artifact = result
        .artifacts
        .iter()
        .find(|a| a.target == exe_name)
        .with_context(|| format!("the build produced no executable for target `{exe_name}`"))?;

    shell.status(Status::Running, artifact.path.display());
    // Our own buffered output must not land after the child's.
    let _ = std::io::stdout().flush();

    let status = Command::new(&artifact.path)
        .args(&args.args)
        .status()
        .with_context(|| format!("could not execute `{}`", artifact.path.display()))?;

    if status.success() {
        return Ok(());
    }
    if let Some(code) = status.code() {
        std::process::exit(code);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            shell.error(format!(
                "`{}` was terminated by signal {sig}",
                artifact.path.display()
            ));
            std::process::exit(128 + sig);
        }
    }
    shell.error(format!(
        "`{}` did not exit successfully ({status})",
        artifact.path.display()
    ));
    std::process::exit(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::RunArgs;
    use clap::Parser;

    fn parse_run_args(args: &[&str]) -> RunArgs {
        #[derive(Parser)]
        struct TestCli {
            #[command(flatten)]
            run: RunArgs,
        }
        TestCli::parse_from(args).run
    }

    fn exe(name: &str, host: bool) -> Target {
        let mut t = Target::new(name, TargetKind::Exe);
        t.host = host;
        t
    }

    #[test]
    fn args_after_double_dash_are_the_programs() {
        let args = parse_run_args(&["run", "tool", "--release", "--", "-x", "--release"]);
        assert_eq!(args.target.as_deref(), Some("tool"));
        assert!(args.release);
        assert_eq!(args.args, vec!["-x", "--release"]);
    }

    #[test]
    fn no_target_and_no_args_by_default() {
        let args = parse_run_args(&["run"]);
        assert!(args.target.is_none());
        assert!(args.args.is_empty());
        assert_eq!(args.profile_name(), "debug");
    }

    #[test]
    fn the_only_non_test_exe_is_the_default() {
        let targets = [
            exe("app", false),
            exe("unit_test", false),
            exe("gen", true),
            Target::new("core", TargetKind::StaticLib),
        ];
        assert_eq!(
            select_exe("p", &targets, None).unwrap().name.as_str(),
            "app"
        );
    }

    #[test]
    fn several_exes_are_listed_and_none_is_an_error() {
        let targets = [exe("a", false), exe("b", false)];
        let err = select_exe("p", &targets, None).unwrap_err().to_string();
        assert!(err.contains("  a") && err.contains("  b"), "{err}");

        let libs = [Target::new("core", TargetKind::StaticLib)];
        let err = select_exe("p", &libs, None).unwrap_err().to_string();
        assert!(err.contains("no executable target"), "{err}");
    }

    #[test]
    fn a_named_target_must_be_an_exe_and_may_be_a_test_exe() {
        let targets = [
            exe("app", false),
            exe("unit_test", false),
            Target::new("core", TargetKind::StaticLib),
        ];
        assert_eq!(
            select_exe("p", &targets, Some("unit_test"))
                .unwrap()
                .name
                .as_str(),
            "unit_test"
        );
        let err = select_exe("p", &targets, Some("core"))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("not an executable") && err.contains("  app"),
            "{err}"
        );
    }
}
