//! Native C compiler driver.
//!
//! Compiles C source files and links them into executables or libraries.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use rayon::prelude::*;

use crate::builder::context::BuildContext;
use crate::builder::plan::{ArchiveStep, BuildPlan, BuildStep, CMakeStep, CompileStep, CustomStep, LinkStep};
use crate::core::surface::LinkGroup;
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
    ///
    /// Processes all steps in order:
    /// - Compile steps run in parallel
    /// - Archive, Link, CMake, and Custom steps run sequentially
    pub fn execute(&self, plan: &BuildPlan, jobs: Option<usize>) -> Result<Vec<Artifact>> {
        // Set up rayon thread pool
        if let Some(j) = jobs {
            rayon::ThreadPoolBuilder::new()
                .num_threads(j)
                .build_global()
                .ok(); // Ignore if already set
        }

        // Separate compile steps for parallel execution
        let compile_steps: Vec<_> = plan.steps.iter()
            .filter_map(|s| match s {
                BuildStep::Compile(c) => Some(c),
                _ => None,
            })
            .collect();

        // Compile all sources in parallel
        if !compile_steps.is_empty() {
            tracing::info!("Compiling {} files", compile_steps.len());

            let compile_results: Vec<Result<()>> = compile_steps
                .par_iter()
                .map(|step| self.compile(step))
                .collect();

            // Check for compile errors
            for result in compile_results {
                result?;
            }
        }

        // Process remaining steps sequentially
        let mut artifacts = Vec::new();

        for step in &plan.steps {
            match step {
                BuildStep::Compile(_) => {
                    // Already handled above
                }
                BuildStep::Archive(s) => {
                    let artifact = self.archive(s)?;
                    artifacts.push(artifact);
                }
                BuildStep::Link(s) => {
                    let artifact = self.link(s)?;
                    artifacts.push(artifact);
                }
                BuildStep::CMake(s) => {
                    self.run_cmake(s)?;
                    // CMake produces artifacts but we don't track them yet
                }
                BuildStep::Custom(s) => {
                    self.run_custom(s)?;
                }
            }
        }

        Ok(artifacts)
    }

    /// Create a static library using the archive step.
    fn archive(&self, step: &ArchiveStep) -> Result<Artifact> {
        // Ensure output directory exists
        if let Some(parent) = step.output.parent() {
            ensure_dir(parent)?;
        }

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

    /// Run a CMake build step.
    fn run_cmake(&self, step: &CMakeStep) -> Result<()> {
        ensure_dir(&step.build_dir)?;

        // Configure
        tracing::info!("Configuring CMake for {}", step.package);
        let mut configure = ProcessBuilder::new("cmake");
        configure = configure.arg("-S").arg(&step.source_dir);
        configure = configure.arg("-B").arg(&step.build_dir);

        // Add user arguments
        for arg in &step.args {
            configure = configure.arg(arg);
        }

        let output = configure.exec()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("CMake configure failed for {}:\n{}", step.package, stderr);
        }

        // Build
        tracing::info!("Building CMake target for {}", step.package);
        let mut build = ProcessBuilder::new("cmake");
        build = build.arg("--build").arg(&step.build_dir);

        // Specific targets if requested
        for target in &step.targets {
            build = build.arg("--target").arg(target);
        }

        let output = build.exec()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("CMake build failed for {}:\n{}", step.package, stderr);
        }

        Ok(())
    }

    /// Run a custom command step.
    fn run_custom(&self, step: &CustomStep) -> Result<()> {
        tracing::info!("Running custom command for {}: {}", step.package, step.program);

        let mut cmd = ProcessBuilder::new(&step.program);

        for arg in &step.args {
            cmd = cmd.arg(arg);
        }

        cmd = cmd.cwd(&step.cwd);

        let output = cmd.exec()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!(
                "custom command `{}` failed for {}:\n{}",
                step.program,
                step.package,
                stderr
            );
        }

        Ok(())
    }

    /// Emit link group flags for a specific toolchain.
    ///
    /// Different platforms have different support for link groups:
    /// - GCC/GNU ld: Full support for --whole-archive and --start-group
    /// - Clang/LLVM: Supports --whole-archive, no --start-group
    /// - macOS: Uses -force_load instead of --whole-archive
    /// - MSVC: Uses /WHOLEARCHIVE, no equivalent to --start-group
    fn emit_link_groups(&self, cmd: &mut ProcessBuilder, groups: &[LinkGroup]) -> Result<()> {
        let compiler_family = &self.ctx.compiler.family;

        for group in groups {
            match group {
                LinkGroup::WholeArchive { libs } => {
                    match compiler_family.as_str() {
                        "gcc" => {
                            *cmd = cmd.clone().arg("-Wl,--whole-archive");
                            for lib in libs {
                                *cmd = cmd.clone().arg(format!("-l{}", lib));
                            }
                            *cmd = cmd.clone().arg("-Wl,--no-whole-archive");
                        }
                        "clang" => {
                            // Standard Clang also uses GNU ld flags
                            *cmd = cmd.clone().arg("-Wl,--whole-archive");
                            for lib in libs {
                                *cmd = cmd.clone().arg(format!("-l{}", lib));
                            }
                            *cmd = cmd.clone().arg("-Wl,--no-whole-archive");
                        }
                        "msvc" => {
                            bail!(
                                "WholeArchive link group not yet supported on MSVC.\n\
                                 Consider vendoring or restructuring dependencies."
                            );
                        }
                        _ => {
                            // Check if we're on macOS (Apple Clang)
                            if cfg!(target_os = "macos") {
                                // macOS uses -force_load per library
                                for lib in libs {
                                    *cmd = cmd.clone().arg("-Wl,-force_load");
                                    *cmd = cmd.clone().arg(format!("-l{}", lib));
                                }
                            } else {
                                // Fall back to GNU-style
                                *cmd = cmd.clone().arg("-Wl,--whole-archive");
                                for lib in libs {
                                    *cmd = cmd.clone().arg(format!("-l{}", lib));
                                }
                                *cmd = cmd.clone().arg("-Wl,--no-whole-archive");
                            }
                        }
                    }
                }
                LinkGroup::StartEndGroup { libs } => {
                    match compiler_family.as_str() {
                        "gcc" => {
                            *cmd = cmd.clone().arg("-Wl,--start-group");
                            for lib in libs {
                                *cmd = cmd.clone().arg(format!("-l{}", lib));
                            }
                            *cmd = cmd.clone().arg("-Wl,--end-group");
                        }
                        _ => {
                            // Clang/macOS/MSVC: --start-group not available
                            bail!(
                                "StartEndGroup link group only supported on GCC/GNU ld.\n\
                                 On other platforms, order libraries to resolve circular dependencies."
                            );
                        }
                    }
                }
            }
        }

        Ok(())
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

    /// Link object files into a target (shared library or executable).
    fn link(&self, step: &LinkStep) -> Result<Artifact> {
        // Ensure output directory exists
        if let Some(parent) = step.output.parent() {
            ensure_dir(parent)?;
        }

        match step.kind.as_str() {
            "staticlib" => {
                // Static libraries are handled by archive() now, but keep
                // compatibility for plans that use LinkStep for static libs
                let archive_step = ArchiveStep {
                    objects: step.objects.clone(),
                    output: step.output.clone(),
                    package: step.package.clone(),
                    target: step.target.clone(),
                };
                self.archive(&archive_step)
            }
            "sharedlib" => self.link_shared(step),
            "exe" => self.link_executable(step),
            _ => bail!("unknown target kind: {}", step.kind),
        }
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
