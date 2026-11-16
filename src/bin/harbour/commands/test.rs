//! `harbour test` command

use std::process::Command;

use anyhow::Result;

use crate::cli::TestArgs;
use harbour::builder::linkage::LinkagePreference;
use harbour::core::target::TargetKind;
use harbour::core::workspace::workspace_manifest_for;
use harbour::core::Workspace;
use harbour::ops::harbour_build::select_subject;
use harbour::ops::harbour_build::{build, BuildOptions};
use harbour::sources::SourceCache;
use harbour::util::config::load_config;
use harbour::util::GlobalContext;

/// Check if a target name matches test patterns.
///
/// Test targets are identified by name patterns:
/// - Exact matches: "test", "tests"
/// - Suffix matches: "*_test", "*_tests"
/// - Prefix matches: "test_*"
///
/// This is a public function for testability.
pub fn is_test_target(name: &str) -> bool {
    let name_lower = name.to_lowercase();
    if name_lower.is_empty() || name_lower.starts_with('_') {
        return false;
    }
    name_lower == "test"
        || name_lower == "tests"
        || name_lower.ends_with("_test")
        || name_lower.ends_with("_tests")
        || name_lower.starts_with("test_")
}

pub fn execute(args: TestArgs) -> Result<()> {
    let ctx = GlobalContext::new()?;

    let (manifest_path, containing_member) = workspace_manifest_for(&ctx.find_manifest()?)?;

    let profile = args.profile_name();
    let ws = Workspace::new(&manifest_path, &ctx)?.with_profile(profile.clone());
    // The package under test: `-p`, else the member the command was run from.
    // Its tests are discovered and it is the only thing built, so the flags
    // speak for it.
    let package = select_subject(&ws, containing_member, args.package.as_deref())?
        .name()
        .as_str()
        .to_string();
    let selection = harbour::ops::harbour_build::feature_selection_for(
        &ws,
        &args.feature_args.features,
        args.feature_args.no_default_features,
        args.feature_args.all_features,
        std::slice::from_ref(&package),
        None,
    )?;
    let ws = ws.with_feature_selection(selection);

    let config = load_config(
        &ctx.config_path(),
        &ctx.project_harbour_dir().join("config.toml"),
    );
    config.reject_removed_backend()?;
    let vcpkg = harbour::builder::resolution_vcpkg(&config.vcpkg, None, ws.is_release());
    let mut source_cache = SourceCache::new_with_vcpkg(ctx.cache_dir(), vcpkg)
        .with_default_registry(ctx.default_registry_url()?.as_str());

    // Discover test targets in that package, not the first member's.
    let root_pkg = ws.subject_package(Some(harbour::util::InternedString::new(&package)));
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
        println!("Example Harbour.toml:");
        println!("  [targets.unit_test]");
        println!("  kind = \"exe\"");
        println!("  sources = [\"tests/**/*.c\"]");
        return Ok(());
    }

    println!("Building {} test target(s)...", test_targets.len());
    println!();

    // Build test targets
    let opts = BuildOptions {
        packages: vec![package],
        targets: test_targets.clone(),
        emit_compile_commands: false,
        emit_plan: false,
        jobs: args.jobs,
        verbose: false,
        cpp_std: None,
        linkage: LinkagePreference::Auto { prefer: vec![] },
        ffi: false,
        target_triple: None,
        locked: false,
        vcpkg: config.vcpkg.clone(),
        strict_sources: args.strict_sources,
        dep_warnings: args.dep_warnings,
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
        println!("test result: ok. {} passed; {} failed", passed, failed);
        Ok(())
    } else {
        println!("test result: FAILED. {} passed; {} failed", passed, failed);
        println!();
        println!("failing tests:");
        for name in &failed_tests {
            println!("    {}", name);
        }
        anyhow::bail!("some tests failed")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::TestArgs;
    use clap::Parser;

    /// Helper to parse TestArgs from command-line strings.
    fn parse_test_args(args: &[&str]) -> TestArgs {
        #[derive(Parser)]
        struct TestCli {
            #[command(flatten)]
            test: TestArgs,
        }
        let cli = TestCli::parse_from(args);
        cli.test
    }

    #[test]
    fn test_test_complex_invocation() {
        let args = parse_test_args(&["test", "--release", "-j", "4", "unit_test", "smoke_test"]);

        assert!(args.release);
        assert_eq!(args.jobs, Some(4));
        assert_eq!(args.targets, vec!["unit_test", "smoke_test"]);
    }

    #[test]
    fn test_is_test_target_exact_test() {
        assert!(is_test_target("test"));
        assert!(is_test_target("Test"));
        assert!(is_test_target("TEST"));
    }

    #[test]
    fn test_is_test_target_exact_tests() {
        assert!(is_test_target("tests"));
        assert!(is_test_target("Tests"));
        assert!(is_test_target("TESTS"));
    }

    #[test]
    fn test_is_test_target_suffix_test() {
        assert!(is_test_target("unit_test"));
        assert!(is_test_target("integration_test"));
        assert!(is_test_target("Unit_Test"));
        assert!(is_test_target("UNIT_TEST"));
    }

    #[test]
    fn test_is_test_target_suffix_tests() {
        assert!(is_test_target("unit_tests"));
        assert!(is_test_target("integration_tests"));
        assert!(is_test_target("Unit_Tests"));
    }

    #[test]
    fn test_is_test_target_prefix_test() {
        assert!(is_test_target("test_main"));
        assert!(is_test_target("test_utils"));
        assert!(is_test_target("Test_Main"));
        assert!(is_test_target("TEST_UTILS"));
    }

    #[test]
    fn test_is_test_target_not_test() {
        assert!(!is_test_target("main"));
        assert!(!is_test_target("mylib"));
        assert!(!is_test_target("testing")); // Contains test but doesn't match patterns
        assert!(!is_test_target("contest")); // Contains test but doesn't match patterns
        assert!(!is_test_target("attest")); // Ends with test but not _test
        assert!(!is_test_target("protest")); // Ends with test but not _test
    }

    #[test]
    fn test_is_test_target_edge_cases() {
        // Empty string should not be a test target
        assert!(!is_test_target(""));

        // Just underscore
        assert!(!is_test_target("_test")); // Starts with underscore, not test_
        assert!(is_test_target("a_test")); // Valid _test suffix
    }

    #[test]
    fn test_is_test_target_common_patterns() {
        // Common naming conventions that should match
        assert!(is_test_target("unit_test"));
        assert!(is_test_target("unit_tests"));
        assert!(is_test_target("integration_test"));
        assert!(is_test_target("integration_tests"));
        assert!(is_test_target("e2e_test"));
        assert!(is_test_target("smoke_test"));
        assert!(is_test_target("test_runner"));
        assert!(is_test_target("test_suite"));

        // Common naming conventions that should NOT match
        assert!(!is_test_target("unittest")); // No underscore
        assert!(!is_test_target("testrunner")); // No underscore
    }

    #[test]
    fn test_is_test_target_case_insensitive() {
        // Verify case insensitivity
        assert!(is_test_target("TEST"));
        assert!(is_test_target("Test"));
        assert!(is_test_target("test"));
        assert!(is_test_target("UNIT_TEST"));
        assert!(is_test_target("Unit_Test"));
        assert!(is_test_target("unit_test"));
        assert!(is_test_target("TEST_MAIN"));
        assert!(is_test_target("Test_Main"));
        assert!(is_test_target("test_main"));
    }

    #[test]
    fn test_profile_selection() {
        assert_eq!(parse_test_args(&["test"]).profile_name(), "debug");
        assert_eq!(
            parse_test_args(&["test", "--release"]).profile_name(),
            "release"
        );
        assert_eq!(
            parse_test_args(&["test", "--release", "-j", "4"]).profile_name(),
            "release"
        );
    }
}
