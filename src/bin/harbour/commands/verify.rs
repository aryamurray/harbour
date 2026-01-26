//! `harbour verify` command

use anyhow::{Context, Result};

use crate::cli::VerifyArgs;
use harbour::ops::verify::{
    format_result_for_output, verify, OutputFormat, VerifyLinkage, VerifyOptions,
};
use harbour::util::GlobalContext;

/// Computes the shim path for GitHub Actions annotations.
///
/// This is used when verifying packages from a local registry path.
pub fn compute_shim_path(package: &str, version: Option<&str>, has_registry_path: bool) -> Option<String> {
    if !has_registry_path {
        return None;
    }

    let first_char = package.chars().next().unwrap_or('_');
    let version = version.unwrap_or("latest");
    Some(format!("index/{}/{}/{}.toml", first_char, package, version))
}

pub fn execute(args: VerifyArgs, verbose: bool) -> Result<()> {
    let linkage: VerifyLinkage = args
        .linkage
        .parse()
        .with_context(|| format!("invalid linkage: {}", args.linkage))?;

    let output_format: OutputFormat = args
        .output_format
        .parse()
        .with_context(|| format!("invalid output format: {}", args.output_format))?;

    // Compute shim path for GitHub Actions annotations (if using local registry)
    let shim_path = args.registry_path.as_ref().map(|_| {
        let first_char = args.package.chars().next().unwrap_or('_');
        let version = args.version.as_deref().unwrap_or("latest");
        format!("index/{}/{}/{}.toml", first_char, args.package, version)
    });

    let options = VerifyOptions {
        package: args.package,
        version: args.version,
        linkage,
        platform: args.platform,
        output_dir: args.output,
        skip_harness: args.skip_harness,
        verbose,
        output_format,
        target_triple: args.target_triple,
        registry_path: args.registry_path,
    };

    // Create GlobalContext for the verify operation
    let ctx = GlobalContext::new().context("failed to create global context")?;

    let result = verify(options, &ctx)?;

    // Print the formatted result based on output format
    let output = format_result_for_output(&result, output_format, verbose, shim_path.as_deref());
    print!("{}", output);

    // Exit with error code if verification failed
    if !result.passed {
        std::process::exit(1);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::VerifyArgs;
    use clap::Parser;
    use std::path::PathBuf;

    /// Helper to parse VerifyArgs from command-line strings.
    fn parse_verify_args(args: &[&str]) -> VerifyArgs {
        #[derive(Parser)]
        struct TestCli {
            #[command(flatten)]
            verify: VerifyArgs,
        }
        let cli = TestCli::parse_from(args);
        cli.verify
    }

    // =========================================================================
    // VerifyArgs Default Values Tests
    // =========================================================================

    #[test]
    fn test_verify_args_with_package_only() {
        let args = parse_verify_args(&["test", "zlib"]);

        assert_eq!(args.package, "zlib");
        assert!(args.version.is_none());
        assert_eq!(args.linkage, "auto");
        assert!(args.platform.is_none());
        assert!(args.output.is_none());
        assert!(!args.skip_harness);
        assert_eq!(args.output_format, "human");
        assert!(args.target_triple.is_none());
        assert!(args.registry_path.is_none());
    }

    // =========================================================================
    // Package and Version Tests
    // =========================================================================

    #[test]
    fn test_verify_with_version() {
        let args = parse_verify_args(&["test", "zlib", "--version", "1.3.1"]);
        assert_eq!(args.package, "zlib");
        assert_eq!(args.version, Some("1.3.1".to_string()));
    }

    #[test]
    fn test_verify_different_packages() {
        let args1 = parse_verify_args(&["test", "openssl"]);
        let args2 = parse_verify_args(&["test", "curl"]);
        let args3 = parse_verify_args(&["test", "libpng"]);

        assert_eq!(args1.package, "openssl");
        assert_eq!(args2.package, "curl");
        assert_eq!(args3.package, "libpng");
    }

    // =========================================================================
    // Linkage Tests
    // =========================================================================

    #[test]
    fn test_verify_linkage_auto() {
        let args = parse_verify_args(&["test", "zlib"]);
        assert_eq!(args.linkage, "auto");
    }

    #[test]
    fn test_verify_linkage_static() {
        let args = parse_verify_args(&["test", "zlib", "--linkage", "static"]);
        assert_eq!(args.linkage, "static");
    }

    #[test]
    fn test_verify_linkage_shared() {
        let args = parse_verify_args(&["test", "zlib", "--linkage", "shared"]);
        assert_eq!(args.linkage, "shared");
    }

    #[test]
    fn test_verify_linkage_both() {
        let args = parse_verify_args(&["test", "zlib", "--linkage", "both"]);
        assert_eq!(args.linkage, "both");
    }

    // =========================================================================
    // Platform Tests
    // =========================================================================

    #[test]
    fn test_verify_platform() {
        let args = parse_verify_args(&["test", "zlib", "--platform", "linux"]);
        assert_eq!(args.platform, Some("linux".to_string()));
    }

    #[test]
    fn test_verify_platform_windows() {
        let args = parse_verify_args(&["test", "zlib", "--platform", "windows"]);
        assert_eq!(args.platform, Some("windows".to_string()));
    }

    // =========================================================================
    // Output Directory Tests
    // =========================================================================

    #[test]
    fn test_verify_output_short_flag() {
        let args = parse_verify_args(&["test", "zlib", "-o", "/tmp/verify_output"]);
        assert_eq!(args.output, Some(PathBuf::from("/tmp/verify_output")));
    }

    #[test]
    fn test_verify_output_long_flag() {
        let args = parse_verify_args(&["test", "zlib", "--output", "./artifacts"]);
        assert_eq!(args.output, Some(PathBuf::from("./artifacts")));
    }

    // =========================================================================
    // Skip Harness Tests
    // =========================================================================

    #[test]
    fn test_verify_skip_harness() {
        let args = parse_verify_args(&["test", "zlib", "--skip-harness"]);
        assert!(args.skip_harness);
    }

    #[test]
    fn test_verify_run_harness_by_default() {
        let args = parse_verify_args(&["test", "zlib"]);
        assert!(!args.skip_harness);
    }

    // =========================================================================
    // Output Format Tests
    // =========================================================================

    #[test]
    fn test_verify_output_format_human() {
        let args = parse_verify_args(&["test", "zlib", "--output-format", "human"]);
        assert_eq!(args.output_format, "human");
    }

    #[test]
    fn test_verify_output_format_json() {
        let args = parse_verify_args(&["test", "zlib", "--output-format", "json"]);
        assert_eq!(args.output_format, "json");
    }

    #[test]
    fn test_verify_output_format_github() {
        let args = parse_verify_args(&["test", "zlib", "--output-format", "github"]);
        assert_eq!(args.output_format, "github");
    }

    // =========================================================================
    // Target Triple Tests
    // =========================================================================

    #[test]
    fn test_verify_target_triple() {
        let args = parse_verify_args(&["test", "zlib", "--target-triple", "x86_64-unknown-linux-gnu"]);
        assert_eq!(args.target_triple, Some("x86_64-unknown-linux-gnu".to_string()));
    }

    #[test]
    fn test_verify_target_triple_freebsd() {
        let args = parse_verify_args(&["test", "zlib", "--target-triple", "x86_64-unknown-freebsd"]);
        assert_eq!(args.target_triple, Some("x86_64-unknown-freebsd".to_string()));
    }

    // =========================================================================
    // Registry Path Tests
    // =========================================================================

    #[test]
    fn test_verify_registry_path() {
        let args = parse_verify_args(&["test", "zlib", "--registry-path", "/path/to/registry"]);
        assert_eq!(args.registry_path, Some(PathBuf::from("/path/to/registry")));
    }

    // =========================================================================
    // Combined Flags Tests
    // =========================================================================

    #[test]
    fn test_verify_complex_invocation() {
        let args = parse_verify_args(&[
            "test",
            "openssl",
            "--version", "3.0.0",
            "--linkage", "static",
            "--platform", "linux",
            "-o", "/tmp/out",
            "--skip-harness",
            "--output-format", "json",
        ]);

        assert_eq!(args.package, "openssl");
        assert_eq!(args.version, Some("3.0.0".to_string()));
        assert_eq!(args.linkage, "static");
        assert_eq!(args.platform, Some("linux".to_string()));
        assert_eq!(args.output, Some(PathBuf::from("/tmp/out")));
        assert!(args.skip_harness);
        assert_eq!(args.output_format, "json");
    }

    // =========================================================================
    // compute_shim_path Tests
    // =========================================================================

    #[test]
    fn test_compute_shim_path_no_registry() {
        let result = compute_shim_path("zlib", Some("1.3.1"), false);
        assert!(result.is_none());
    }

    #[test]
    fn test_compute_shim_path_with_registry() {
        let result = compute_shim_path("zlib", Some("1.3.1"), true);
        assert_eq!(result, Some("index/z/zlib/1.3.1.toml".to_string()));
    }

    #[test]
    fn test_compute_shim_path_latest_version() {
        let result = compute_shim_path("openssl", None, true);
        assert_eq!(result, Some("index/o/openssl/latest.toml".to_string()));
    }

    #[test]
    fn test_compute_shim_path_single_char_package() {
        let result = compute_shim_path("a", Some("1.0"), true);
        assert_eq!(result, Some("index/a/a/1.0.toml".to_string()));
    }

    #[test]
    fn test_compute_shim_path_empty_package() {
        let result = compute_shim_path("", Some("1.0"), true);
        assert_eq!(result, Some("index/_//1.0.toml".to_string()));
    }

    // =========================================================================
    // VerifyLinkage Parsing Tests
    // =========================================================================

    #[test]
    fn test_parse_verify_linkage_auto() {
        let linkage: VerifyLinkage = "auto".parse().unwrap();
        assert_eq!(linkage, VerifyLinkage::Auto);
    }

    #[test]
    fn test_parse_verify_linkage_static() {
        let linkage: VerifyLinkage = "static".parse().unwrap();
        assert_eq!(linkage, VerifyLinkage::Static);
    }

    #[test]
    fn test_parse_verify_linkage_shared() {
        let linkage: VerifyLinkage = "shared".parse().unwrap();
        assert_eq!(linkage, VerifyLinkage::Shared);
    }

    #[test]
    fn test_parse_verify_linkage_both() {
        let linkage: VerifyLinkage = "both".parse().unwrap();
        assert_eq!(linkage, VerifyLinkage::Both);
    }

    #[test]
    fn test_parse_verify_linkage_invalid() {
        let result: Result<VerifyLinkage, _> = "invalid".parse();
        assert!(result.is_err());
    }

    // =========================================================================
    // OutputFormat Parsing Tests
    // =========================================================================

    #[test]
    fn test_parse_output_format_human() {
        let format: OutputFormat = "human".parse().unwrap();
        assert_eq!(format, OutputFormat::Human);
    }

    #[test]
    fn test_parse_output_format_json() {
        let format: OutputFormat = "json".parse().unwrap();
        assert_eq!(format, OutputFormat::Json);
    }

    #[test]
    fn test_parse_output_format_github() {
        let format: OutputFormat = "github".parse().unwrap();
        assert_eq!(format, OutputFormat::Github);
    }

    #[test]
    fn test_parse_output_format_gha_alias() {
        let format: OutputFormat = "gha".parse().unwrap();
        assert_eq!(format, OutputFormat::Github);
    }

    #[test]
    fn test_parse_output_format_invalid() {
        let result: Result<OutputFormat, _> = "invalid_format".parse();
        assert!(result.is_err());
    }

    // =========================================================================
    // VerifyOptions Construction Tests
    // =========================================================================

    #[test]
    fn test_verify_options_from_args() {
        let args = parse_verify_args(&[
            "test",
            "zlib",
            "--version", "1.3.1",
            "--linkage", "static",
        ]);

        let linkage: VerifyLinkage = args.linkage.parse().unwrap();
        let output_format: OutputFormat = args.output_format.parse().unwrap();

        let opts = VerifyOptions {
            package: args.package.clone(),
            version: args.version.clone(),
            linkage,
            platform: args.platform,
            output_dir: args.output,
            skip_harness: args.skip_harness,
            verbose: false,
            output_format,
            target_triple: args.target_triple,
            registry_path: args.registry_path,
        };

        assert_eq!(opts.package, "zlib");
        assert_eq!(opts.version, Some("1.3.1".to_string()));
        assert_eq!(opts.linkage, VerifyLinkage::Static);
        assert!(!opts.skip_harness);
        assert_eq!(opts.output_format, OutputFormat::Human);
    }
}
