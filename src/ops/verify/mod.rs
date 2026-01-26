//! CI-grade package verification.
//!
//! The `verify` command performs comprehensive validation of a package
//! including build, install, and consumer test harness verification.
//!
//! ## Usage
//!
//! ```bash
//! harbour verify zlib                    # Verify with default settings
//! harbour verify zlib --linkage static   # Verify static build
//! harbour verify zlib --linkage shared   # Verify shared build
//! harbour verify zlib --platform linux   # Verify for specific platform
//! harbour verify zlib --output-format github  # GitHub Actions output
//! ```
//!
//! ## Verification Steps
//!
//! 1. Resolve package from registry
//! 2. Fetch source (git or tarball)
//! 3. Verify checksums
//! 4. Apply patches
//! 5. Build with specified backend (via standard harbour_build::build())
//! 6. Verify install artifacts
//! 7. Run consumer test harness (if configured)
//!
//! ## Output Formats
//!
//! - `human`: Default human-readable output
//! - `json`: Machine-readable JSON output
//! - `github`: GitHub Actions annotations with job summary

mod build;
mod format;
mod harness;
mod resolve;
mod types;

use std::time::{Duration, Instant};

use anyhow::Result;

use crate::util::context::GlobalContext;

// Re-export public types
pub use self::format::{format_result, format_result_for_output};
pub use self::harness::{generate_harness, generate_harness_c, generate_harness_cxx};
pub use self::types::{
    OutputFormat, OutputFormatParseError, VerifyLinkage, VerifyLinkageParseError, VerifyOptions,
    VerifyResult, VerifyStep,
};

// Internal imports
use self::build::build_package;
use self::harness::{run_harness_test, verify_artifacts};
use self::resolve::{apply_patches, fetch_source, resolve_package};

/// Verify a package according to the options.
///
/// The GlobalContext provides access to configuration, cache directories, and
/// registry defaults.
pub fn verify(options: VerifyOptions, ctx: &GlobalContext) -> Result<VerifyResult> {
    let start = Instant::now();
    let version_str = options
        .version
        .clone()
        .unwrap_or_else(|| "latest".to_string());
    let mut result = VerifyResult::new(&options.package, &version_str);
    result.linkage = format!("{:?}", options.linkage).to_lowercase();
    result.platform = options.platform.clone();
    result.target_triple = options.target_triple.clone();

    // Validate inputs
    if options.package.is_empty() {
        result.add_step(VerifyStep::fail(
            "Resolve",
            "No package specified",
            Duration::ZERO,
        ));
        result.total_duration = start.elapsed();
        return Ok(result);
    }

    // Step 1: Resolve package from registry
    let resolve_start = Instant::now();
    let verify_ctx = match resolve_package(&options, ctx) {
        Ok(vctx) => {
            result.version = vctx.shim.package.version.clone();
            result.add_step(VerifyStep::pass(
                "Resolve",
                format!(
                    "Package {} v{} found in registry",
                    vctx.shim.package.name, vctx.shim.package.version
                ),
                resolve_start.elapsed(),
            ));
            vctx
        }
        Err(e) => {
            result.add_step(VerifyStep::fail(
                "Resolve",
                format!("Failed to resolve package: {}", e),
                resolve_start.elapsed(),
            ));
            result.total_duration = start.elapsed();
            return Ok(result);
        }
    };

    // Step 2: Fetch source
    let fetch_start = Instant::now();
    let source_dir = match fetch_source(&verify_ctx) {
        Ok(dir) => {
            result.add_step(VerifyStep::pass(
                "Fetch",
                format!("Source fetched to {}", dir.display()),
                fetch_start.elapsed(),
            ));
            dir
        }
        Err(e) => {
            result.add_step(VerifyStep::fail(
                "Fetch",
                format!("Failed to fetch source: {}", e),
                fetch_start.elapsed(),
            ));
            result.total_duration = start.elapsed();
            return Ok(result);
        }
    };

    // Step 3: Apply patches (if any)
    let patch_start = Instant::now();
    if !verify_ctx.shim.patches.is_empty() {
        match apply_patches(&verify_ctx, &source_dir, &options) {
            Ok(_) => {
                result.add_step(VerifyStep::pass(
                    "Patches",
                    format!("Applied {} patches", verify_ctx.shim.patches.len()),
                    patch_start.elapsed(),
                ));
            }
            Err(e) => {
                result.add_step(VerifyStep::fail(
                    "Patches",
                    format!("Failed to apply patches: {}", e),
                    patch_start.elapsed(),
                ));
                result.total_duration = start.elapsed();
                return Ok(result);
            }
        }
    } else {
        result.add_step(VerifyStep::pass(
            "Patches",
            "No patches to apply",
            patch_start.elapsed(),
        ));
    }

    // Step 4: Build
    let build_start = Instant::now();
    match build_package(&verify_ctx, &source_dir, &options, ctx) {
        Ok(artifacts) => {
            result.artifacts = artifacts.clone();
            let artifact_list: Vec<String> = artifacts
                .iter()
                .map(|p| {
                    p.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string()
                })
                .collect();
            result.add_step(VerifyStep::pass(
                "Build",
                format!(
                    "Build completed ({:?} linkage). Artifacts: {}",
                    options.linkage,
                    artifact_list.join(", ")
                ),
                build_start.elapsed(),
            ));
        }
        Err(e) => {
            result.add_step(VerifyStep::fail(
                "Build",
                format!("Build failed: {}", e),
                build_start.elapsed(),
            ));
            result.total_duration = start.elapsed();
            return Ok(result);
        }
    }

    // Step 5: Verify artifacts
    let verify_start = Instant::now();
    match verify_artifacts(&result.artifacts) {
        Ok(_) => {
            result.add_step(VerifyStep::pass(
                "Artifacts",
                format!("Verified {} artifacts exist", result.artifacts.len()),
                verify_start.elapsed(),
            ));
        }
        Err(e) => {
            result.add_step(VerifyStep::fail(
                "Artifacts",
                format!("Artifact verification failed: {}", e),
                verify_start.elapsed(),
            ));
            result.total_duration = start.elapsed();
            return Ok(result);
        }
    }

    // Step 6: Harness test (if configured and not skipped)
    if !options.skip_harness {
        let harness_start = Instant::now();
        if let Some(harness_config) = verify_ctx.shim.harness() {
            match run_harness_test(
                harness_config,
                &verify_ctx,
                &source_dir,
                &result.artifacts,
                options.target_triple.as_deref(),
            ) {
                Ok(_) => {
                    result.add_step(VerifyStep::pass(
                        "Harness",
                        format!("Consumer test passed (tested {})", harness_config.header),
                        harness_start.elapsed(),
                    ));
                }
                Err(e) => {
                    result.add_step(VerifyStep::fail(
                        "Harness",
                        format!("Harness test failed: {}", e),
                        harness_start.elapsed(),
                    ));
                }
            }
        } else {
            result.add_step(
                VerifyStep::pass("Harness", "No harness configured", harness_start.elapsed())
                    .with_warning("Package has no harness test defined"),
            );
        }
    }

    result.total_duration = start.elapsed();
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_verify_empty_package() {
        let options = VerifyOptions::default();
        let ctx = GlobalContext::new().unwrap();
        let result = verify(options, &ctx).unwrap();
        assert!(!result.passed);
    }
}
