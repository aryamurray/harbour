//! `harbour verify` command

use anyhow::{Context, Result};

use crate::cli::VerifyArgs;
use harbour::ops::verify::{format_result, verify, VerifyLinkage, VerifyOptions};

pub fn execute(args: VerifyArgs, verbose: bool) -> Result<()> {
    let linkage: VerifyLinkage = args
        .linkage
        .parse()
        .with_context(|| format!("invalid linkage: {}", args.linkage))?;

    let options = VerifyOptions {
        package: args.package,
        version: args.version,
        linkage,
        platform: args.platform,
        output_dir: args.output,
        skip_harness: args.skip_harness,
        verbose,
    };

    let result = verify(options)?;

    // Print the formatted result
    let output = format_result(&result, verbose);
    print!("{}", output);

    // Exit with error code if verification failed
    if !result.passed {
        std::process::exit(1);
    }

    Ok(())
}
