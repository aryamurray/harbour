//! `harbour test` command

use std::process::Command;

use anyhow::Result;

use crate::cli::TestArgs;
use harbour::builder::shim::LinkagePreference;
use harbour::core::abi::TargetTriple;
use harbour::core::target::TargetKind;
use harbour::core::Workspace;
use harbour::ops::harbour_build::{build, BuildOptions};
use harbour::sources::SourceCache;
use harbour::util::config::load_config;
use harbour::util::GlobalContext;
use harbour::util::VcpkgIntegration;

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

    let manifest_path = ctx.find_manifest()?;

    let profile = if args.release { "release" } else { "debug" };
    let ws = Workspace::new(&manifest_path, &ctx)?.with_profile(profile);

    let config = load_config(
        &ctx.config_path(),
        &ctx.project_harbour_dir().join("config.toml"),
    );
    let vcpkg = VcpkgIntegration::from_config(&config.vcpkg, &TargetTriple::host(), args.release);
    let mut source_cache = SourceCache::new_with_vcpkg(ctx.cache_dir(), vcpkg);

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
        vcpkg: config.vcpkg.clone(),
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
    use harbour::util::config::VcpkgConfig;

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

    // =========================================================================
    // TestArgs Default Values Tests
    // =========================================================================

    #[test]
    fn test_test_args_defaults() {
        let args = parse_test_args(&["test"]);

        assert!(args.targets.is_empty());
        assert!(!args.release);
        assert!(args.jobs.is_none());
    }

    // =========================================================================
    // Target Selection Tests
    // =========================================================================

    #[test]
    fn test_test_single_target() {
        let args = parse_test_args(&["test", "unit_test"]);
        assert_eq!(args.targets, vec!["unit_test"]);
    }

    #[test]
    fn test_test_multiple_targets() {
        let args = parse_test_args(&["test", "unit_test", "integration_test", "e2e_test"]);
        assert_eq!(
            args.targets,
            vec!["unit_test", "integration_test", "e2e_test"]
        );
    }

    // =========================================================================
    // Release Flag Tests
    // =========================================================================

    #[test]
    fn test_test_release_short_flag() {
        let args = parse_test_args(&["test", "-r"]);
        assert!(args.release);
    }

    #[test]
    fn test_test_release_long_flag() {
        let args = parse_test_args(&["test", "--release"]);
        assert!(args.release);
    }

    #[test]
    fn test_test_debug_by_default() {
        let args = parse_test_args(&["test"]);
        assert!(!args.release);
    }

    // =========================================================================
    // Jobs/Parallelism Tests
    // =========================================================================

    #[test]
    fn test_test_jobs_short_flag() {
        let args = parse_test_args(&["test", "-j", "4"]);
        assert_eq!(args.jobs, Some(4));
    }

    #[test]
    fn test_test_jobs_long_flag() {
        let args = parse_test_args(&["test", "--jobs", "8"]);
        assert_eq!(args.jobs, Some(8));
    }

    // =========================================================================
    // Combined Flags Tests
    // =========================================================================

    #[test]
    fn test_test_complex_invocation() {
        let args = parse_test_args(&["test", "--release", "-j", "4", "unit_test", "smoke_test"]);

        assert!(args.release);
        assert_eq!(args.jobs, Some(4));
        assert_eq!(args.targets, vec!["unit_test", "smoke_test"]);
    }

    // =========================================================================
    // is_test_target Tests (Test Pattern Matching)
    // =========================================================================

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

    // =========================================================================
    // BuildOptions Construction Tests
    // =========================================================================

    #[test]
    fn test_build_options_for_tests() {
        let args = parse_test_args(&["test", "--release", "-j", "4"]);

        let opts = BuildOptions {
            release: args.release,
            packages: vec![],
            targets: vec!["unit_test".to_string()],
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
            vcpkg: VcpkgConfig::default(),
        };

        assert!(opts.release);
        assert_eq!(opts.jobs, Some(4));
        assert!(!opts.emit_compile_commands); // Tests don't need compile_commands.json
        assert!(!opts.ffi); // Tests don't need FFI
    }

    #[test]
    fn test_build_options_debug_mode() {
        let args = parse_test_args(&["test"]);

        let opts = BuildOptions {
            release: args.release,
            packages: vec![],
            targets: vec!["test".to_string()],
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
            vcpkg: VcpkgConfig::default(),
        };

        assert!(!opts.release); // Default is debug mode
    }

    // =========================================================================
    // Profile Selection Tests
    // =========================================================================

    #[test]
    fn test_profile_selection_debug() {
        let args = parse_test_args(&["test"]);
        let profile = if args.release { "release" } else { "debug" };
        assert_eq!(profile, "debug");
    }

    #[test]
    fn test_profile_selection_release() {
        let args = parse_test_args(&["test", "--release"]);
        let profile = if args.release { "release" } else { "debug" };
        assert_eq!(profile, "release");
    }
}
