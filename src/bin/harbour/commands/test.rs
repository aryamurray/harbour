//! `harbour test` command

use std::process::Command;

use anyhow::Result;

use crate::cli::TestArgs;
use harbour::builder::shim::LinkagePreference;
use harbour::core::target::TargetKind;
use harbour::core::Workspace;
use harbour::ops::harbour_build::{build, BuildOptions};
use harbour::sources::SourceCache;
use harbour::util::GlobalContext;

/// Check if a target name matches test patterns.
fn is_test_target(name: &str) -> bool {
    let name_lower = name.to_lowercase();
    name_lower == "test"
        || name_lower == "tests"
        || name_lower.ends_with("_test")
        || name_lower.ends_with("_tests")
        || name_lower.starts_with("test_")
}

pub fn execute(args: TestArgs) -> Result<()> {
    let ctx = GlobalContext::new()?;

    let manifest_path = ctx.find_manifest()?;

    let profile = if args.release { "release" } else { "debug" };
    let ws = Workspace::new(&manifest_path, &ctx)?.with_profile(profile);

    let mut source_cache = SourceCache::new(ctx.cache_dir());

    // Discover test targets
    let root_pkg = ws.root_package();
    let test_targets: Vec<String> = if args.targets.is_empty() {
        // Auto-discover test targets
        root_pkg
            .targets()
            .iter()
            .filter(|t| t.kind == TargetKind::Exe && is_test_target(&t.name))
            .map(|t| t.name.to_string())
            .collect()
    } else {
        // Use specified targets
        args.targets.clone()
    };

    if test_targets.is_empty() {
        println!("No test targets found.");
        println!();
        println!("help: Test targets are executables with names matching:");
        println!("      *_test, *_tests, test_*, test, tests");
        println!();
        println!("Example Harbor.toml:");
        println!("  [targets.unit_test]");
        println!("  kind = \"exe\"");
        println!("  sources = [\"tests/**/*.c\"]");
        return Ok(());
    }

    println!("Building {} test target(s)...", test_targets.len());
    println!();

    // Build test targets
    let opts = BuildOptions {
        release: args.release,
        packages: vec![],
        targets: test_targets.clone(),
        emit_compile_commands: false,
        emit_plan: false,
        jobs: args.jobs,
        verbose: false,
        cpp_std: None,
        backend: None,
        linkage: LinkagePreference::Auto { prefer: vec![] },
        ffi: false,
        target_triple: None,
        locked: false,
    };

    let result = build(&ws, &mut source_cache, &opts)?;

    println!("Running tests:");
    println!();

    let mut passed = 0;
    let mut failed = 0;
    let mut failed_tests: Vec<String> = Vec::new();

    for artifact in &result.artifacts {
        if test_targets.contains(&artifact.target.to_string()) {
            print!("  {} ... ", artifact.target);

            // Run the test executable
            let status = Command::new(&artifact.path).status();

            match status {
                Ok(exit_status) if exit_status.success() => {
                    println!("ok");
                    passed += 1;
                }
                Ok(_) => {
                    println!("FAILED");
                    failed += 1;
                    failed_tests.push(artifact.target.to_string());
                }
                Err(e) => {
                    println!("FAILED (could not execute: {})", e);
                    failed += 1;
                    failed_tests.push(artifact.target.to_string());
                }
            }
        }
    }

    println!();

    if failed == 0 {
        println!(
            "test result: ok. {} passed; {} failed",
            passed, failed
        );
        Ok(())
    } else {
        println!(
            "test result: FAILED. {} passed; {} failed",
            passed, failed
        );
        println!();
        println!("failing tests:");
        for name in &failed_tests {
            println!("    {}", name);
        }
        anyhow::bail!("some tests failed")
    }
}
