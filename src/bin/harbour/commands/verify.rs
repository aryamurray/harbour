//! `harbour verify` command

use anyhow::{Context, Result};

use crate::cli::VerifyArgs;
use harbour::ops::verify::{
    format_result_for_output, verify, OutputFormat, VerifyLinkage, VerifyOptions,
};
use harbour::util::GlobalContext;

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
        format!(
            "index/{}/{}/{}.toml",
            first_char, args.package, version
        )
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
    let output = format_result_for_output(
        &result,
        output_format,
        verbose,
        shim_path.as_deref(),
    );
    print!("{}", output);

    // Exit with error code if verification failed
    if !result.passed {
        std::process::exit(1);
    }

    Ok(())
}
