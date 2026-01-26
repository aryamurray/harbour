//! Native C/C++ compiler driver.
//!
//! Compiles C/C++ source files and links them into executables or libraries.

use anyhow::{bail, Result};
use rayon::prelude::*;

use crate::builder::context::BuildContext;
use crate::builder::plan::{
    ArchiveStep, BuildPlan, BuildStep, CMakeStep, CompileStep, CustomStep, LinkStep,
};
use crate::builder::toolchain::{ArchiveInput, CommandSpec, CompileInput, CxxOptions, LinkInput};
use crate::core::target::Language;
use crate::ops::harbour_build::Artifact;
use crate::util::fs::ensure_dir;
use crate::util::process::ProcessBuilder;

/// Native C/C++ builder.
pub struct NativeBuilder<'a> {
    ctx: &'a BuildContext,
    /// C++ options for compilation (if any C++ is involved)
    cxx_opts: Option<CxxOptions>,
}

impl<'a> NativeBuilder<'a> {
    /// Create a new native builder.
    pub fn new(ctx: &'a BuildContext) -> Self {
        NativeBuilder {
            ctx,
            cxx_opts: None,
        }
    }

    /// Create a new native builder with C++ options.
    pub fn with_cxx_options(ctx: &'a BuildContext, cxx_opts: CxxOptions) -> Self {
        NativeBuilder {
            ctx,
            cxx_opts: Some(cxx_opts),
        }
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
        let compile_steps: Vec<_> = plan
            .steps
            .iter()
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

        let input = ArchiveInput {
            objects: step.objects.clone(),
            output: step.output.clone(),
        };

        let spec = self.ctx.toolchain().archive_command(&input);
        let cmd = self.process_builder_from_spec(spec);

        tracing::debug!("Creating static library {}", step.output.display());

        let output = cmd.exec()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("archiving failed for {}\n{}", step.output.display(), stderr);
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
        tracing::info!(
            "Running custom command for {}: {}",
            step.package,
            step.program
        );

        let mut cmd = ProcessBuilder::new(&step.program);

        for arg in &step.args {
            cmd = cmd.arg(arg);
        }

        cmd = cmd.cwd(&step.cwd);

        // Apply environment variables
        for (key, value) in &step.env {
            cmd = cmd.env(key, value);
        }

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

    /// Compile a single source file.
    fn compile(&self, step: &CompileStep) -> Result<()> {
        // Ensure output directory exists
        if let Some(parent) = step.output.parent() {
            ensure_dir(parent)?;
        }

        let mut cflags = self.ctx.profile_cflags();
        cflags.extend(step.cflags.iter().cloned());

        let input = CompileInput {
            source: step.source.clone(),
            output: step.output.clone(),
            include_dirs: step.include_dirs.clone(),
            defines: parse_define_flags(&step.defines),
            cflags,
        };

        // Generate compile command with language and C++ options
        let spec = self
            .ctx
            .toolchain()
            .compile_command(&input, step.lang, self.cxx_opts.as_ref());
        let cmd = self.process_builder_from_spec(spec);

        // Execute
        tracing::debug!(
            "Compiling {} -> {} ({})",
            step.source.display(),
            step.output.display(),
            step.lang.as_str()
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
        let (libs, mut extra_ldflags) = split_link_flags(&step.libs);
        let mut ldflags = self.ctx.profile_ldflags();
        ldflags.extend(step.ldflags.iter().cloned());
        ldflags.append(&mut extra_ldflags);

        let input = LinkInput {
            objects: step.objects.clone(),
            output: step.output.clone(),
            lib_dirs: step.lib_dirs.clone(),
            libs,
            ldflags,
        };

        // Select C or C++ linker driver based on use_cxx_linker
        let driver = if step.use_cxx_linker {
            Language::Cxx
        } else {
            Language::C
        };

        let spec = self
            .ctx
            .toolchain()
            .link_shared_command(&input, driver, self.cxx_opts.as_ref());
        let cmd = self.process_builder_from_spec(spec);

        tracing::debug!(
            "Creating shared library {} (driver: {})",
            step.output.display(),
            driver.as_str()
        );

        let output = cmd.exec()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("linking failed for {}\n{}", step.output.display(), stderr);
        }

        Ok(Artifact {
            path: step.output.clone(),
            target: step.target.clone(),
        })
    }

    /// Link an executable.
    fn link_executable(&self, step: &LinkStep) -> Result<Artifact> {
        let (libs, mut extra_ldflags) = split_link_flags(&step.libs);
        let mut ldflags = self.ctx.profile_ldflags();
        ldflags.extend(step.ldflags.iter().cloned());
        ldflags.append(&mut extra_ldflags);

        let input = LinkInput {
            objects: step.objects.clone(),
            output: step.output.clone(),
            lib_dirs: step.lib_dirs.clone(),
            libs,
            ldflags,
        };

        // Select C or C++ linker driver based on use_cxx_linker
        let driver = if step.use_cxx_linker {
            Language::Cxx
        } else {
            Language::C
        };

        let spec = self
            .ctx
            .toolchain()
            .link_exe_command(&input, driver, self.cxx_opts.as_ref());
        let cmd = self.process_builder_from_spec(spec);

        tracing::debug!(
            "Linking executable {} (driver: {})",
            step.output.display(),
            driver.as_str()
        );

        let output = cmd.exec()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("linking failed for {}\n{}", step.output.display(), stderr);
        }

        Ok(Artifact {
            path: step.output.clone(),
            target: step.target.clone(),
        })
    }

    fn process_builder_from_spec(&self, spec: CommandSpec) -> ProcessBuilder {
        let mut cmd = ProcessBuilder::new(&spec.program);

        for arg in spec.args {
            cmd = cmd.arg(arg);
        }

        for (key, value) in spec.env {
            cmd = cmd.env(key, value);
        }

        cmd
    }
}

fn parse_define_flags(defines: &[String]) -> Vec<(String, Option<String>)> {
    let mut parsed = Vec::new();

    for define in defines {
        let trimmed = define
            .strip_prefix("-D")
            .or_else(|| define.strip_prefix("/D"));

        let Some(rest) = trimmed else {
            continue;
        };

        if let Some((name, value)) = rest.split_once('=') {
            parsed.push((name.to_string(), Some(value.to_string())));
        } else if !rest.is_empty() {
            parsed.push((rest.to_string(), None));
        }
    }

    parsed
}

fn split_link_flags(flags: &[String]) -> (Vec<String>, Vec<String>) {
    let mut libs = Vec::new();
    let mut extra = Vec::new();
    let mut iter = flags.iter().peekable();

    while let Some(flag) = iter.next() {
        if flag == "-framework" {
            if let Some(name) = iter.next() {
                extra.push(flag.clone());
                extra.push(name.clone());
            }
            continue;
        }

        if let Some(name) = flag.strip_prefix("-l") {
            if !name.is_empty() {
                libs.push(name.to_string());
            }
            continue;
        }

        if flag.ends_with(".lib")
            || flag.ends_with(".a")
            || flag.ends_with(".so")
            || flag.ends_with(".dylib")
            || flag.ends_with(".dll")
        {
            extra.push(flag.clone());
            continue;
        }

        extra.push(flag.clone());
    }

    (libs, extra)
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
