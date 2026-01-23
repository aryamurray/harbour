//! Native C compiler driver.
//!
//! Compiles C source files and links them into executables or libraries.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use rayon::prelude::*;

use crate::builder::context::BuildContext;
use crate::builder::plan::{BuildPlan, CompileStep, LinkStep};
use crate::ops::harbour_build::Artifact;
use crate::util::fs::ensure_dir;
use crate::util::process::ProcessBuilder;

/// Native C builder.
pub struct NativeBuilder<'a> {
    ctx: &'a BuildContext,
}

impl<'a> NativeBuilder<'a> {
    /// Create a new native builder.
    pub fn new(ctx: &'a BuildContext) -> Self {
        NativeBuilder { ctx }
    }

    /// Execute the build plan.
    pub fn execute(&self, plan: &BuildPlan, jobs: Option<usize>) -> Result<Vec<Artifact>> {
        // Set up rayon thread pool
        if let Some(j) = jobs {
            rayon::ThreadPoolBuilder::new()
                .num_threads(j)
                .build_global()
                .ok(); // Ignore if already set
        }

        // Compile all sources in parallel
        tracing::info!(
            "Compiling {} files",
            plan.compile_steps.len()
        );

        let compile_results: Vec<Result<()>> = plan
            .compile_steps
            .par_iter()
            .map(|step| self.compile(step))
            .collect();

        // Check for compile errors
        for result in compile_results {
            result?;
        }

        // Link (must be sequential per-target, but targets can be parallel)
        tracing::info!("Linking {} targets", plan.link_steps.len());

        let mut artifacts = Vec::new();
        for step in &plan.link_steps {
            let artifact = self.link(step)?;
            artifacts.push(artifact);
        }

        Ok(artifacts)
    }

    /// Compile a single source file.
    fn compile(&self, step: &CompileStep) -> Result<()> {
        // Ensure output directory exists
        if let Some(parent) = step.output.parent() {
            ensure_dir(parent)?;
        }

        // Build command
        let mut cmd = self.ctx.compiler();

        // Add include directories
        for dir in &step.include_dirs {
            cmd = cmd.arg(format!("-I{}", dir.display()));
        }

        // Add defines
        for define in &step.defines {
            cmd = cmd.arg(define);
        }

        // Add profile flags
        for flag in self.ctx.profile_cflags() {
            cmd = cmd.arg(flag);
        }

        // Add custom flags
        for flag in &step.cflags {
            cmd = cmd.arg(flag);
        }

        // Compile only
        cmd = cmd.arg("-c");

        // Input and output
        cmd = cmd.arg(&step.source);
        cmd = cmd.arg("-o");
        cmd = cmd.arg(&step.output);

        // Execute
        tracing::debug!(
            "Compiling {} -> {}",
            step.source.display(),
            step.output.display()
        );

        let output = cmd.exec()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!(
                "compilation failed for {}\n{}",
                step.source.display(),
                stderr
            );
        }

        Ok(())
    }

    /// Link object files into a target.
    fn link(&self, step: &LinkStep) -> Result<Artifact> {
        // Ensure output directory exists
        if let Some(parent) = step.output.parent() {
            ensure_dir(parent)?;
        }

        match step.kind.as_str() {
            "staticlib" => self.link_static(step),
            "sharedlib" => self.link_shared(step),
            "exe" => self.link_executable(step),
            _ => bail!("unknown target kind: {}", step.kind),
        }
    }

    /// Create a static library.
    fn link_static(&self, step: &LinkStep) -> Result<Artifact> {
        let mut cmd = self.ctx.archiver();

        // ar rcs output.a obj1.o obj2.o ...
        cmd = cmd.arg("rcs");
        cmd = cmd.arg(&step.output);

        for obj in &step.objects {
            cmd = cmd.arg(obj);
        }

        tracing::debug!("Creating static library {}", step.output.display());

        let output = cmd.exec()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!(
                "archiving failed for {}\n{}",
                step.output.display(),
                stderr
            );
        }

        Ok(Artifact {
            path: step.output.clone(),
            target: step.target.clone(),
        })
    }

    /// Create a shared library.
    fn link_shared(&self, step: &LinkStep) -> Result<Artifact> {
        let mut cmd = self.ctx.compiler();

        // Shared library flag
        cmd = cmd.arg("-shared");

        // Output
        cmd = cmd.arg("-o");
        cmd = cmd.arg(&step.output);

        // Object files
        for obj in &step.objects {
            cmd = cmd.arg(obj);
        }

        // Library search paths
        for dir in &step.lib_dirs {
            cmd = cmd.arg(format!("-L{}", dir.display()));
        }

        // Libraries
        for lib in &step.libs {
            cmd = cmd.arg(lib);
        }

        // Profile flags
        for flag in self.ctx.profile_ldflags() {
            cmd = cmd.arg(flag);
        }

        // Custom flags
        for flag in &step.ldflags {
            cmd = cmd.arg(flag);
        }

        tracing::debug!("Creating shared library {}", step.output.display());

        let output = cmd.exec()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!(
                "linking failed for {}\n{}",
                step.output.display(),
                stderr
            );
        }

        Ok(Artifact {
            path: step.output.clone(),
            target: step.target.clone(),
        })
    }

    /// Link an executable.
    fn link_executable(&self, step: &LinkStep) -> Result<Artifact> {
        let mut cmd = self.ctx.compiler();

        // Output
        cmd = cmd.arg("-o");
        cmd = cmd.arg(&step.output);

        // Object files
        for obj in &step.objects {
            cmd = cmd.arg(obj);
        }

        // Library search paths
        for dir in &step.lib_dirs {
            cmd = cmd.arg(format!("-L{}", dir.display()));
        }

        // Libraries
        for lib in &step.libs {
            cmd = cmd.arg(lib);
        }

        // Profile flags
        for flag in self.ctx.profile_ldflags() {
            cmd = cmd.arg(flag);
        }

        // Custom flags
        for flag in &step.ldflags {
            cmd = cmd.arg(flag);
        }

        tracing::debug!("Linking executable {}", step.output.display());

        let output = cmd.exec()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!(
                "linking failed for {}\n{}",
                step.output.display(),
                stderr
            );
        }

        Ok(Artifact {
            path: step.output.clone(),
            target: step.target.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests require a C compiler, so they're marked as ignore by default
    #[test]
    #[ignore]
    fn test_compile_simple() {
        // This test would require setting up a full build context
        // and is primarily for manual testing
    }
}
