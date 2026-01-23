//! Implementation of `harbour build`.

use std::path::PathBuf;

use anyhow::Result;

use crate::builder::{BuildContext, BuildPlan, NativeBuilder};
use crate::core::Workspace;
use crate::ops::resolve::resolve_workspace;
use crate::resolver::Resolve;
use crate::sources::SourceCache;

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
    // Resolve dependencies (uses lockfile if available)
    let resolve = resolve_workspace(ws, source_cache)?;

    // Ensure output directory exists
    ws.ensure_output_dir()?;

    // Create build context
    let profile = if opts.release { "release" } else { "debug" };
    let build_ctx = BuildContext::new(ws, profile)?;

    // Create build plan
    let plan = BuildPlan::new(&build_ctx, &resolve, source_cache)?;

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
        plan.emit_compile_commands(&cc_path)?;
        tracing::info!("Wrote {}", cc_path.display());
    }

    // Execute build
    let builder = NativeBuilder::new(&build_ctx);
    let artifacts = builder.execute(&plan, opts.jobs)?;

    Ok(BuildResult {
        artifacts,
        plan: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::GlobalContext;
    use tempfile::TempDir;

    fn create_test_project(dir: &std::path::Path) {
        std::fs::write(
            dir.join("Harbor.toml"),
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
        let ws = Workspace::new(&tmp.path().join("Harbor.toml"), &ctx).unwrap();

        let mut cache = SourceCache::new(tmp.path().join("cache"));
        let opts = BuildOptions::default();

        let result = build(&ws, &mut cache, &opts).unwrap();
        assert!(!result.artifacts.is_empty());
    }
}
