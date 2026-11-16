//! CI-grade package verification.
//!
//! `harbour verify <pkg>` answers one question: *can a consumer use this
//! package?* It answers it by being one. A throwaway project depending on
//! `<pkg>` is written, resolved against the registry, fetched through the
//! ordinary source cache (which is where a shim's patches are applied, once,
//! and kept), built with [`crate::ops::harbour_build::build`], and linked
//! against a harness executable which is run unless the target is foreign.
//!
//! Nothing in that chain is verify's own. An earlier version fetched, patched
//! and built the package itself, and the copies drifted: it applied a shim's
//! patch a second time on every run (so it could not be run twice), threw away
//! the manifest the patch had just added in favour of one it generated (so
//! `zstd` and `openssl` built no sources and `curl` built the wrong ones), and
//! so verified something nobody consumes. Whatever `harbour add <pkg> &&
//! harbour build` does, `verify` now does, because it *is* that.
//!
//! ## Usage
//!
//! ```bash
//! harbour verify zlib                    # Verify with default settings
//! harbour verify zlib --linkage shared   # Verify shared build
//! harbour verify zlib --registry-path .  # Verify against a registry checkout
//! harbour verify zlib --format json      # Machine-readable result
//! ```
//!
//! ## Stages
//!
//! 1. **Resolve**: the consumer's dependency on the package, from the index.
//! 2. **Fetch**: the source, checksum-verified and patched, via the source cache.
//! 3. **Build**: the consumer, which builds the package's default library
//!    target and its siblings (prebuild generators, host tools and probes
//!    included).
//! 4. **Artifacts**: every library the consumer links exists, is not empty and
//!    exports something.
//! 5. **Harness**: the shim's `[metadata.harness]` if it has one (`declared`),
//!    else every public header that compiles on its own plus a link
//!    (`default`). Run, unless cross-compiling.
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

use crate::core::target::TargetTriple;
use crate::util::context::GlobalContext;

// Re-export public types
pub use self::format::{format_result, format_result_for_output};
pub use self::harness::{generate_harness, generate_harness_c, generate_harness_cxx};
pub use self::types::{
    BuildStats, Failure, HarnessReport, OutputFormat, OutputFormatParseError, RejectedHeader,
    TranslationUnits, VerifyLinkage, VerifyLinkageParseError, VerifyOptions, VerifyResult,
    VerifyStep,
};

use self::build::{
    build_consumer, consumer_include_dirs, default_library_closure, library_artifacts,
    translation_units, Consumer, HARNESS_TARGET,
};
use self::harness::{
    declared_sources, discover_headers, failed_header_unit, first_compiler_error, header_unit,
    is_cxx, linkable_symbol, main_source, placeholder_source, run_harness, verify_artifacts,
    Header,
};
use self::resolve::{load_shim, select_registry, source_cache, version_requirement};

/// Verify a package according to the options.
///
/// The GlobalContext provides access to configuration, cache directories, and
/// registry defaults. A failed stage is recorded in the result, not returned as
/// an error: the result is the report.
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
    result.triple = crate::builder::toolchain::resolve_target(
        options
            .target_triple
            .as_deref()
            .map(TargetTriple::parse)
            .as_ref(),
    )
    .as_str()
    .to_string();

    if options.package.is_empty() {
        result.add_step(VerifyStep::fail(
            "Resolve",
            "No package specified",
            Duration::ZERO,
        ));
    } else {
        stages(&options, ctx, &mut result);
    }

    result.total_duration = start.elapsed();
    Ok(result)
}

/// Run the stages in order, stopping at the first failure. Each stage records
/// its own step; this function only decides whether to continue.
fn stages(options: &VerifyOptions, ctx: &GlobalContext, result: &mut VerifyResult) {
    let package_name = options.package.as_str();

    // ---- Resolve -------------------------------------------------------
    let started = Instant::now();
    let setup = (|| -> Result<_> {
        let registry = select_registry(options, ctx)?;
        let consumer = Consumer::create(options)?;
        let requirement = version_requirement(options.version.as_deref());
        // Resolution happens in a project of its own: the real one is written
        // once the package's language is known.
        let probe = consumer.nested("resolve");
        probe.write_manifest(package_name, &requirement, false)?;
        probe.write_harness(&[placeholder_source(false)])?;
        let mut cache = source_cache(&registry, ctx, options.target_triple.as_deref());
        let ws = probe.workspace(ctx)?;
        let resolved = crate::ops::resolve::resolve_workspace(&ws, &mut cache).map_err(|e| {
            let hint = if registry.local.is_some() {
                "\nhint: a registry resolves from its tier-1 index, not its shims; if you have \
                 just added or edited a shim, run `harbour registry index` in the registry \
                 checkout"
            } else {
                ""
            };
            anyhow::anyhow!("{e:#}{hint}")
        })?;
        let pkg_id = resolved
            .topological_order()
            .into_iter()
            .find(|id| id.name().as_str() == package_name)
            .ok_or_else(|| anyhow::anyhow!("package `{package_name}` is not in the resolution"))?;
        if !pkg_id.source_id().is_registry() {
            anyhow::bail!(
                "package `{package_name}` was not found in the registry ({}); it resolved from \
                 {} instead, which is not what verification covers",
                registry.url,
                pkg_id.source_id()
            );
        }
        let version = pkg_id.version().to_string();
        let shim = load_shim(&registry, ctx, package_name, &version)?;
        Ok((registry, consumer, cache, pkg_id, version, shim))
    })();
    let (registry, consumer, mut cache, pkg_id, version, shim) = match setup {
        Ok(setup) => setup,
        Err(e) => {
            result.add_step(VerifyStep::fail(
                "Resolve",
                format!("Failed to resolve package: {e:#}"),
                started.elapsed(),
            ));
            return;
        }
    };
    result.version = version.clone();
    result.add_step(VerifyStep::pass(
        "Resolve",
        format!(
            "Package {package_name} v{version} found in registry {}",
            registry.url
        ),
        started.elapsed(),
    ));

    // ---- Fetch ---------------------------------------------------------
    let started = Instant::now();
    let fetched = (|| -> Result<_> {
        let package = cache.load_package(pkg_id)?;
        let closure_names: Vec<String> = default_library_closure(&package)?
            .iter()
            .map(|t| t.name.to_string())
            .collect();
        Ok((package, closure_names))
    })();
    let (package, closure_names) = match fetched {
        Ok(fetched) => fetched,
        Err(e) => {
            result.add_step(VerifyStep::fail(
                "Fetch",
                format!("Failed to fetch source: {e:#}"),
                started.elapsed(),
            ));
            return;
        }
    };
    let closure = default_library_closure(&package).expect("checked above");
    let cxx = closure.first().is_some_and(|t| is_cxx(t));
    let patches = shim.as_ref().map_or(0, |s| s.patches.len());
    result.add_step(VerifyStep::pass(
        "Fetch",
        format!(
            "Source at {} ({patches} patch(es) applied); default library target(s): {}",
            package.root().display(),
            closure_names.join(", ")
        ),
        started.elapsed(),
    ));

    // ---- Build ---------------------------------------------------------
    let started = Instant::now();
    let requirement = version_requirement(options.version.as_deref());
    let prepared = (|| -> Result<_> {
        consumer.write_manifest(package_name, &requirement, cxx)?;
        consumer.write_harness(&[placeholder_source(cxx)])?;
        consumer.workspace(ctx)
    })();
    let ws = match prepared {
        Ok(ws) => ws,
        Err(e) => {
            result.add_step(VerifyStep::fail(
                "Build",
                format!("Build failed: {e:#}"),
                started.elapsed(),
            ));
            return;
        }
    };
    let outcome = match build_consumer(&ws, &mut cache, options, ctx) {
        Ok(outcome) => outcome,
        Err(e) => {
            result.add_step(VerifyStep::fail(
                "Build",
                format!("Build failed: {e:#}"),
                started.elapsed(),
            ));
            return;
        }
    };
    let units = translation_units(&ws, package_name);
    result.translation_units = TranslationUnits {
        count: units.len(),
        files: units,
    };
    result.build = BuildStats {
        compiled: outcome.compiled,
        up_to_date: outcome.skipped,
    };
    result.artifacts = outcome.artifacts.iter().map(|a| a.path.clone()).collect();
    let mut build_step = VerifyStep::pass(
        "Build",
        format!(
            "Build completed ({:?} linkage): {} translation unit(s) in {package_name}, \
             {} compiled and {} up to date across the graph",
            options.linkage, result.translation_units.count, outcome.compiled, outcome.skipped
        ),
        started.elapsed(),
    );
    let all_prebuilt = closure
        .iter()
        .all(|t| t.kind.is_prebuilt() || t.kind.is_header_only());
    if result.translation_units.count == 0 && !all_prebuilt {
        build_step = build_step.with_warning(format!(
            "{package_name} recorded no translation units in the lockfile; its libraries are \
             built from prebuilt objects or nothing"
        ));
    }
    result.add_step(build_step);

    // ---- Artifacts -----------------------------------------------------
    let started = Instant::now();
    let apple = {
        let triple = result.triple.as_str();
        triple.contains("apple") || triple.contains("darwin")
    };
    let checked = (|| -> Result<_> {
        let libraries = library_artifacts(&closure, &outcome.artifacts)?;
        let mut paths: Vec<_> = libraries.iter().map(|a| a.path.clone()).collect();
        // A prebuilt target's "artifact" is what it lists for this platform.
        // The build above already refused a missing file; here they are held
        // to the same "exists and is not empty" as a library Harbour built.
        let platform = crate::core::surface::TargetPlatform::for_target(
            &crate::builder::toolchain::resolve_target(
                options
                    .target_triple
                    .as_deref()
                    .map(TargetTriple::parse)
                    .as_ref(),
            ),
        );
        for target in closure.iter().filter(|t| t.kind.is_prebuilt()) {
            let files = target.resolved_prebuilt(&platform, &Default::default());
            paths.extend(
                files
                    .libs
                    .iter()
                    .chain(&files.runtime)
                    .map(|f| package.root().join(&f.path)),
            );
        }
        verify_artifacts(&paths)?;
        // The first static archive of the default target is the one the
        // default harness references a symbol of.
        let symbol = match paths.first() {
            Some(path) if is_static_archive(path) => linkable_symbol(path, apple)?,
            _ => None,
        };
        Ok((paths, symbol))
    })();
    let (libraries, symbol) = match checked {
        Ok(checked) => checked,
        Err(e) => {
            result.add_step(VerifyStep::fail(
                "Artifacts",
                format!("Artifact verification failed: {e:#}"),
                started.elapsed(),
            ));
            return;
        }
    };
    result.add_step(VerifyStep::pass(
        "Artifacts",
        format!(
            "Verified {} librar{}: {}",
            libraries.len(),
            if libraries.len() == 1 { "y" } else { "ies" },
            libraries
                .iter()
                .map(|p| p.file_name().unwrap_or_default().to_string_lossy())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        started.elapsed(),
    ));

    // ---- Harness -------------------------------------------------------
    if options.skip_harness {
        result.add_step(VerifyStep::skipped("Harness", "skipped by --skip-harness"));
        return;
    }
    let started = Instant::now();
    let is_cross = options
        .target_triple
        .as_deref()
        .is_some_and(|t| !TargetTriple::parse(t).is_host());

    let ran = (|| -> Result<(HarnessReport, Vec<String>, std::path::PathBuf)> {
        let rebuild = |sources: &[self::build::HarnessSource],
                       cache: &mut crate::sources::SourceCache|
         -> Result<_> {
            consumer.write_harness(sources)?;
            let ws = consumer.workspace(ctx)?;
            build_consumer(&ws, cache, options, ctx)
        };
        let exe_of = |outcome: &crate::ops::harbour_build::BuildResult| {
            outcome
                .artifacts
                .iter()
                .find(|a| a.target == HARNESS_TARGET)
                .map(|a| a.path.clone())
                .ok_or_else(|| anyhow::anyhow!("the harness was built but produced no executable"))
        };

        if let Some(config) = shim.as_ref().and_then(|s| s.harness()) {
            let (hcxx, sources) = declared_sources(config)?;
            if hcxx != cxx {
                // The harness is written in the other language from the
                // library. The manifest says which, so it is rewritten, and the
                // cache that holds the old one is replaced.
                consumer.write_manifest(package_name, &requirement, hcxx)?;
                cache = source_cache(&registry, ctx, options.target_triple.as_deref());
            }
            let outcome = rebuild(&sources, &mut cache)?;
            let report = HarnessReport {
                level: "declared",
                lang: Some(if hcxx { "c++" } else { "c" }.to_string()),
                headers: vec![config.header.clone()],
                ..HarnessReport::default()
            };
            return Ok((report, Vec::new(), exe_of(&outcome)?));
        }

        // The default harness. The directories come from the consumer's own
        // compile surface, once the build has produced any generated headers.
        let ws = consumer.workspace(ctx)?;
        let include_dirs = consumer_include_dirs(&ws, &mut cache, options)?;
        let (candidates, mut rejected) =
            discover_headers(package_name, package.root(), &closure, &include_dirs, cxx)?;
        if candidates.is_empty() {
            anyhow::bail!(
                "no public headers were found for `{package_name}`, so there is nothing for a \
                 consumer to include\n\
                 hint: declare the target's `public_headers`, or give the shim a \
                 `[metadata.harness]` naming a header and a call"
            );
        }

        let total = candidates.len();
        let mut remaining: Vec<(usize, Header)> = candidates.into_iter().enumerate().collect();
        let outcome = loop {
            let mut sources = vec![main_source(symbol.as_deref(), cxx)];
            sources.extend(remaining.iter().map(|(n, h)| header_unit(*n, h, cxx)));
            match rebuild(&sources, &mut cache) {
                Ok(outcome) => break outcome,
                Err(e) => {
                    let text = format!("{e:#}");
                    let dropped = failed_header_unit(&text)
                        .and_then(|n| remaining.iter().position(|(i, _)| *i == n));
                    let Some(at) = dropped else {
                        return Err(e);
                    };
                    let (_, header) = remaining.remove(at);
                    rejected.push(RejectedHeader {
                        header: header.include,
                        kind: "compile",
                        error: first_compiler_error(&text, &[package.root(), consumer.dir()]),
                    });
                    if remaining.is_empty() {
                        anyhow::bail!(
                            "none of the {total} public header(s) compiles on its own, so a \
                             consumer could not include the package\n{}",
                            rejected
                                .iter()
                                .take(5)
                                .map(|r| format!("  {}: {}", r.header, r.error))
                                .collect::<Vec<_>>()
                                .join("\n")
                        );
                    }
                }
            }
        };

        let mut warnings: Vec<String> = rejected
            .iter()
            .filter(|r| r.kind == "compile")
            .map(|r| {
                format!(
                    "public header `{}` does not compile on its own: {}",
                    r.header, r.error
                )
            })
            .collect();
        let umbrella: Vec<&str> = rejected
            .iter()
            .filter(|r| r.kind == "umbrella")
            .map(|r| r.header.as_str())
            .collect();
        if !umbrella.is_empty() {
            warnings.push(format!(
                "{} public header(s) are guarded with an `#error` telling consumers to include \
                 another header, so they were not tried on their own: {}",
                umbrella.len(),
                umbrella.join(", ")
            ));
        }
        let report = HarnessReport {
            level: "default",
            lang: Some(if cxx { "c++" } else { "c" }.to_string()),
            headers: remaining.iter().map(|(_, h)| h.include.clone()).collect(),
            rejected_headers: rejected,
            link_symbol: symbol.clone(),
            executed: false,
        };
        Ok((report, warnings, exe_of(&outcome)?))
    })();

    let (mut report, warnings, exe) = match ran {
        Ok(ran) => ran,
        Err(e) => {
            result.add_step(VerifyStep::fail(
                "Harness",
                format!("Harness failed: {e:#}"),
                started.elapsed(),
            ));
            return;
        }
    };

    let execution = if is_cross {
        Ok(false)
    } else {
        run_harness(&exe, consumer.dir()).map(|()| true)
    };
    match execution {
        Ok(executed) => {
            report.executed = executed;
            let what = match report.level {
                "declared" => format!(
                    "Consumer test passed (tested {})",
                    report.headers.join(", ")
                ),
                _ => format!(
                    "{} of {} public header(s) included{} and linked{}",
                    report.headers.len(),
                    report.headers.len() + report.rejected_headers.len(),
                    if report.rejected_headers.is_empty() {
                        ""
                    } else {
                        " (see the warnings for the rest)"
                    },
                    report
                        .link_symbol
                        .as_ref()
                        .map(|s| format!(" (referenced `{s}`)"))
                        .unwrap_or_default()
                ),
            };
            let suffix = if executed {
                ""
            } else {
                " [linked, not run: cross-compiling]"
            };
            let mut step = VerifyStep::pass(
                "Harness",
                format!("harness: {}: {what}{suffix}", report.describe()),
                started.elapsed(),
            );
            for warning in warnings {
                step = step.with_warning(warning);
            }
            if report.level == "default" && report.link_symbol.is_none() {
                step = step.with_warning(
                    "the library's symbol table could not be read or names nothing a harness can \
                     reference, so the link was not forced to pull the archive in",
                );
            }
            result.harness = report;
            result.add_step(step);
        }
        Err(e) => {
            result.harness = report;
            result.add_step(VerifyStep::fail(
                "Harness",
                format!("Harness test failed: {e:#}"),
                started.elapsed(),
            ));
        }
    }
}

/// Whether `path` is a static archive by its name (`.a` or `.lib`).
fn is_static_archive(path: &std::path::Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("a") | Some("lib")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_verify_empty_package() {
        let options = VerifyOptions::default();
        let ctx = GlobalContext::with_cwd_isolated(std::env::current_dir().unwrap()).unwrap();
        let result = verify(options, &ctx).unwrap();
        assert!(!result.passed);
        assert_eq!(result.failure.as_ref().unwrap().stage, "Resolve");
    }
}
