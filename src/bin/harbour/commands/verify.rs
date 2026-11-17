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
        strict_sources: args.strict_sources,
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

    /// `--output-format` is a free string on the command line; this parse is
    /// what accepts or refuses it.
    #[test]
    fn test_parse_output_format() {
        for (text, expected) in [
            ("human", OutputFormat::Human),
            ("json", OutputFormat::Json),
            ("github", OutputFormat::Github),
            ("gha", OutputFormat::Github),
        ] {
            let format: OutputFormat = text.parse().unwrap();
            assert_eq!(format, expected, "{text}");
        }

        let result: Result<OutputFormat, _> = "invalid_format".parse();
        assert!(result.is_err());
    }
}
