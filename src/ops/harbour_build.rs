//! Implementation of `harbour build`.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{bail, Result};

use crate::builder::{BuildContext, BuildPlan, NativeBuilder};
use crate::core::target::CppStandard;
use crate::core::Workspace;
use crate::ops::resolve::resolve_workspace;
use crate::resolver::{CppConstraints, Resolve};
use crate::sources::SourceCache;

/// Validate that all requested targets exist in the root package.
///
/// This prevents silent no-ops when the user specifies a nonexistent target.
fn validate_target_filter(ws: &Workspace, targets: &[String]) -> Result<()> {
    let valid_targets: Vec<_> = ws
        .root_package()
        .targets()
        .iter()
        .map(|t| t.name.to_string())
        .collect();

    for requested in targets {
        if !valid_targets.iter().any(|t| t == requested) {
            bail!(
                "unknown target `{}` in root package\n\
                 available targets: {}\n\
                 hint: use `harbour tree` to see all targets",
                requested,
                if valid_targets.is_empty() {
                    "(none)".to_string()
                } else {
                    valid_targets.join(", ")
                }
            );
        }
    }

    Ok(())
}

/// Options for the build command.
#[derive(Debug, Clone, Default)]
pub struct BuildOptions {
    /// Build in release mode
    pub release: bool,

    /// Specific targets to build (empty = all)
    pub targets: Vec<String>,

    /// Emit compile_commands.json
    pub emit_compile_commands: bool,

    /// Emit build plan as JSON
    pub emit_plan: bool,

    /// Number of parallel jobs
    pub jobs: Option<usize>,

    /// Verbose output
    pub verbose: bool,

    /// Explicit C++ standard from CLI (--std flag)
    pub cpp_std: Option<CppStandard>,
}

/// Build result.
#[derive(Debug)]
pub struct BuildResult {
    /// Built artifacts
    pub artifacts: Vec<Artifact>,

    /// Build plan (if requested)
    pub plan: Option<BuildPlan>,
}

/// A built artifact.
#[derive(Debug)]
pub struct Artifact {
    /// Artifact path
    pub path: PathBuf,

    /// Target name
    pub target: String,
}

/// Build the workspace.
pub fn build(
    ws: &Workspace,
    source_cache: &mut SourceCache,
    opts: &BuildOptions,
) -> Result<BuildResult> {
    // Validate target filter if specified
    let target_filter = if !opts.targets.is_empty() {
        validate_target_filter(ws, &opts.targets)?;
        Some(opts.targets.as_slice())
    } else {
        None
    };

    // Resolve dependencies (uses lockfile if available)
    let resolve = resolve_workspace(ws, source_cache)?;

    // Ensure output directory exists
    ws.ensure_output_dir()?;

    // Create build context
    let profile = if opts.release { "release" } else { "debug" };
    let mut build_ctx = BuildContext::new(ws, profile)?;

    // Compute C++ constraints from the resolved packages
    let packages = collect_packages(&resolve, source_cache)?;
    let cpp_constraints = CppConstraints::compute(
        &resolve,
        &packages,
        &ws.manifest().build,
        opts.cpp_std,
    )?;

    // Log C++ constraints if any C++ is involved
    if cpp_constraints.has_cpp {
        if let Some(std) = cpp_constraints.effective_std {
            tracing::info!("C++ standard: {}", std);
        }
        if !cpp_constraints.effective_exceptions {
            tracing::info!("C++ exceptions: disabled");
        }
        if !cpp_constraints.effective_rtti {
            tracing::info!("C++ RTTI: disabled");
        }
    }

    // Set C++ constraints on build context
    build_ctx = build_ctx.with_cpp_constraints(cpp_constraints.clone());

    // Create build plan with target filter
    let plan = BuildPlan::new(&build_ctx, &resolve, source_cache, target_filter)?;

    // If only emitting plan, return early
    if opts.emit_plan {
        let plan_json = serde_json::to_string_pretty(&plan)?;
        println!("{}", plan_json);

        return Ok(BuildResult {
            artifacts: vec![],
            plan: Some(plan),
        });
    }

    // Emit compile_commands.json if requested
    if opts.emit_compile_commands {
        let cc_path = ws.output_dir().join("compile_commands.json");
        plan.emit_compile_commands(&build_ctx, &cc_path)?;
        tracing::info!("Wrote {}", cc_path.display());
    }

    // Execute build with C++ options if needed
    let cxx_opts = build_ctx.cxx_options();
    let builder = if let Some(opts) = cxx_opts {
        NativeBuilder::with_cxx_options(&build_ctx, opts)
    } else {
        NativeBuilder::new(&build_ctx)
    };
    let artifacts = builder.execute(&plan, opts.jobs)?;

    Ok(BuildResult {
        artifacts,
        plan: None,
    })
}

/// Collect packages from the resolve for C++ constraint computation.
fn collect_packages(
    resolve: &Resolve,
    source_cache: &mut SourceCache,
) -> Result<HashMap<crate::core::PackageId, crate::core::Package>> {
    let mut packages = HashMap::new();

    for pkg_id in resolve.topological_order() {
        if let Ok(package) = source_cache.load_package(pkg_id) {
            packages.insert(pkg_id, package);
        }
    }

    Ok(packages)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::GlobalContext;
    use tempfile::TempDir;

    fn create_test_project(dir: &std::path::Path) {
        std::fs::write(
            dir.join("Harbour.toml"),
            r#"
[package]
name = "test"
version = "1.0.0"

[targets.test]
kind = "exe"
sources = ["src/**/*.c"]

[targets.test.surface.compile.private]
cflags = ["-Wall"]
"#,
        )
        .unwrap();

        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("src/main.c"),
            r#"
#include <stdio.h>

int main(void) {
    printf("Hello from Harbour!\n");
    return 0;
}
"#,
        )
        .unwrap();
    }

    #[test]
    #[ignore] // Requires C compiler
    fn test_build() {
        let tmp = TempDir::new().unwrap();
        create_test_project(tmp.path());

        let ctx = GlobalContext::with_cwd(tmp.path().to_path_buf()).unwrap();
        let ws = Workspace::new(&tmp.path().join("Harbour.toml"), &ctx).unwrap();

        let mut cache = SourceCache::new(tmp.path().join("cache"));
        let opts = BuildOptions::default();

        let result = build(&ws, &mut cache, &opts).unwrap();
        assert!(!result.artifacts.is_empty());
    }

    #[test]
    fn test_validate_target_filter_valid() {
        let tmp = TempDir::new().unwrap();
        create_test_project(tmp.path());

        let ctx = GlobalContext::with_cwd(tmp.path().to_path_buf()).unwrap();
        let ws = Workspace::new(&tmp.path().join("Harbour.toml"), &ctx).unwrap();

        // "test" is a valid target in create_test_project
        let result = validate_target_filter(&ws, &["test".to_string()]);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_target_filter_invalid() {
        let tmp = TempDir::new().unwrap();
        create_test_project(tmp.path());

        let ctx = GlobalContext::with_cwd(tmp.path().to_path_buf()).unwrap();
        let ws = Workspace::new(&tmp.path().join("Harbour.toml"), &ctx).unwrap();

        // "nonexistent" is not a valid target
        let result = validate_target_filter(&ws, &["nonexistent".to_string()]);
        assert!(result.is_err());

        let err = result.unwrap_err().to_string();
        assert!(err.contains("unknown target"));
        assert!(err.contains("nonexistent"));
        assert!(err.contains("available targets"));
    }
}
