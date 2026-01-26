//! Output formatting for verification results (human/JSON/GitHub).

use std::fmt::Write as _;

use super::types::{OutputFormat, VerifyResult};

/// Format verification result for display (human-readable).
pub fn format_result(result: &VerifyResult, verbose: bool) -> String {
    let mut output = String::new();

    writeln!(
        output,
        "Verify: {} v{} ({})",
        result.package, result.version, result.linkage
    )
    .unwrap();
    writeln!(output, "{}", "=".repeat(50)).unwrap();
    writeln!(output).unwrap();

    // Steps
    for step in &result.steps {
        let status = if step.passed { "[OK]" } else { "[FAIL]" };
        writeln!(output, "  {} {} ({:.2?})", status, step.name, step.duration).unwrap();

        if verbose || !step.passed {
            writeln!(output, "      {}", step.message).unwrap();
        }
        for warning in &step.warnings {
            writeln!(output, "      Warning: {}", warning).unwrap();
        }
    }

    writeln!(output).unwrap();

    // Summary
    let status = if result.passed { "PASSED" } else { "FAILED" };
    writeln!(
        output,
        "Result: {} ({}/{} steps passed)",
        status,
        result.passed_count(),
        result.steps.len()
    )
    .unwrap();
    writeln!(output, "Total time: {:.2?}", result.total_duration).unwrap();

    // Artifacts
    if !result.artifacts.is_empty() {
        writeln!(output, "\nArtifacts:").unwrap();
        for artifact in &result.artifacts {
            writeln!(output, "  - {}", artifact.display()).unwrap();
        }
    }

    // Warnings
    let warnings = result.warnings();
    if !warnings.is_empty() {
        writeln!(output, "\nWarnings:").unwrap();
        for warning in warnings {
            writeln!(output, "  - {}", warning).unwrap();
        }
    }

    output
}

/// Format verification result as JSON.
pub fn format_result_json(result: &VerifyResult) -> String {
    serde_json::to_string_pretty(result)
        .unwrap_or_else(|e| format!(r#"{{"error": "Failed to serialize result: {}"}}"#, e))
}

/// Format verification result for GitHub Actions.
///
/// Outputs:
/// - `::error::` and `::warning::` annotations for CI integration
/// - Job summary in markdown format
pub fn format_result_github_actions(result: &VerifyResult, shim_path: Option<&str>) -> String {
    let mut output = String::new();
    let file_ref = shim_path.unwrap_or("");

    // Output error/warning annotations for failed steps
    for step in &result.steps {
        if !step.passed {
            // Escape newlines for GitHub Actions annotation format
            let escaped_msg = step.message.replace('\n', "%0A").replace('\r', "");
            if file_ref.is_empty() {
                writeln!(output, "::error title={}::{}", step.name, escaped_msg).unwrap();
            } else {
                writeln!(
                    output,
                    "::error file={},title={}::{}",
                    file_ref, step.name, escaped_msg
                )
                .unwrap();
            }
        }

        // Output warnings
        for warning in &step.warnings {
            let escaped_warning = warning.replace('\n', "%0A").replace('\r', "");
            if file_ref.is_empty() {
                writeln!(output, "::warning title={}::{}", step.name, escaped_warning).unwrap();
            } else {
                writeln!(
                    output,
                    "::warning file={},title={}::{}",
                    file_ref, step.name, escaped_warning
                )
                .unwrap();
            }
        }
    }

    // Job summary in markdown format
    writeln!(output, "::group::Verification Summary").unwrap();
    writeln!(output).unwrap();
    writeln!(
        output,
        "## {} v{} ({})",
        result.package, result.version, result.linkage
    )
    .unwrap();
    writeln!(output).unwrap();

    // Platform info
    if let Some(platform) = &result.platform {
        writeln!(output, "**Platform:** {}", platform).unwrap();
    }
    if let Some(triple) = &result.target_triple {
        writeln!(output, "**Target Triple:** {}", triple).unwrap();
    }
    writeln!(output).unwrap();

    // Steps table
    writeln!(output, "| Step | Status | Duration |").unwrap();
    writeln!(output, "|------|--------|----------|").unwrap();
    for step in &result.steps {
        let status = if step.passed {
            ":white_check_mark:"
        } else {
            ":x:"
        };
        writeln!(
            output,
            "| {} | {} | {:.2?} |",
            step.name, status, step.duration
        )
        .unwrap();
    }
    writeln!(output).unwrap();

    // Overall result
    let overall_status = if result.passed { "PASSED" } else { "FAILED" };
    let overall_emoji = if result.passed {
        ":heavy_check_mark:"
    } else {
        ":x:"
    };
    writeln!(
        output,
        "**Result:** {} {} ({}/{} steps passed)",
        overall_emoji,
        overall_status,
        result.passed_count(),
        result.steps.len()
    )
    .unwrap();
    writeln!(output, "**Total time:** {:.2?}", result.total_duration).unwrap();

    // Artifacts
    if !result.artifacts.is_empty() {
        writeln!(output).unwrap();
        writeln!(output, "### Artifacts").unwrap();
        for artifact in &result.artifacts {
            if let Some(name) = artifact.file_name() {
                writeln!(output, "- `{}`", name.to_string_lossy()).unwrap();
            }
        }
    }

    // Warnings summary
    let warnings = result.warnings();
    if !warnings.is_empty() {
        writeln!(output).unwrap();
        writeln!(output, "### Warnings").unwrap();
        for warning in warnings {
            writeln!(output, "- {}", warning).unwrap();
        }
    }

    // Failed step details
    let failed_steps: Vec<_> = result.steps.iter().filter(|s| !s.passed).collect();
    if !failed_steps.is_empty() {
        writeln!(output).unwrap();
        writeln!(output, "### Failed Steps").unwrap();
        for step in failed_steps {
            writeln!(output).unwrap();
            writeln!(output, "<details>").unwrap();
            writeln!(output, "<summary>{}</summary>", step.name).unwrap();
            writeln!(output).unwrap();
            writeln!(output, "```").unwrap();
            writeln!(output, "{}", step.message).unwrap();
            writeln!(output, "```").unwrap();
            writeln!(output).unwrap();
            writeln!(output, "</details>").unwrap();
        }
    }

    writeln!(output, "::endgroup::").unwrap();

    output
}

/// Format the result according to the specified output format.
pub fn format_result_for_output(
    result: &VerifyResult,
    format: OutputFormat,
    verbose: bool,
    shim_path: Option<&str>,
) -> String {
    match format {
        OutputFormat::Human => format_result(result, verbose),
        OutputFormat::Json => format_result_json(result),
        OutputFormat::Github => format_result_github_actions(result, shim_path),
    }
}
