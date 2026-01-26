//! Native C/C++ compiler driver.
//!
//! Compiles C/C++ source files and links them into executables or libraries.

use anyhow::{bail, Result};
use rayon::prelude::*;

use crate::builder::context::BuildContext;
use crate::builder::plan::{
    ArchiveStep, BuildPlan, BuildStep, CMakeStep, CompileStep, CustomStep, LinkStep, MesonStep,
};
use crate::builder::toolchain::{ArchiveInput, CommandSpec, CompileInput, CxxOptions, LinkInput};
use crate::builder::util::parse_define_flags;
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
                BuildStep::Meson(s) => {
                    self.run_meson(s)?;
                    // Meson produces artifacts but we don't track them yet
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

    /// Run a Meson build step.
    fn run_meson(&self, step: &MesonStep) -> Result<()> {
        ensure_dir(&step.build_dir)?;

        // Configure with meson setup
        tracing::info!("Configuring Meson for {}", step.package);
        let mut configure = ProcessBuilder::new("meson");
        configure = configure.arg("setup");
        configure = configure.arg(&step.build_dir);
        configure = configure.arg(&step.source_dir);

        // Add user options
        for opt in &step.options {
            configure = configure.arg(opt);
        }

        let output = configure.exec()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("Meson setup failed for {}:\n{}", step.package, stderr);
        }

        // Build with meson compile
        tracing::info!("Building Meson target for {}", step.package);
        let mut build = ProcessBuilder::new("meson");
        build = build.arg("compile");
        build = build.arg("-C").arg(&step.build_dir);

        // Specific targets if requested
        for target in &step.targets {
            build = build.arg(target);
        }

        let output = build.exec()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("Meson compile failed for {}:\n{}", step.package, stderr);
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
    use std::path::PathBuf;

    // Tests require a C compiler, so they're marked as ignore by default
    #[test]
    #[ignore]
    fn test_compile_simple() {
        // This test would require setting up a full build context
        // and is primarily for manual testing
    }

    #[test]
    fn test_split_link_flags_libraries() {
        let flags = vec![
            "-lm".to_string(),
            "-lpthread".to_string(),
            "-lz".to_string(),
        ];

        let (libs, extra) = split_link_flags(&flags);

        assert_eq!(libs, vec!["m", "pthread", "z"]);
        assert!(extra.is_empty());
    }

    #[test]
    fn test_split_link_flags_framework() {
        let flags = vec![
            "-framework".to_string(),
            "CoreFoundation".to_string(),
            "-framework".to_string(),
            "Security".to_string(),
        ];

        let (libs, extra) = split_link_flags(&flags);

        assert!(libs.is_empty());
        assert_eq!(extra.len(), 4);
        assert_eq!(extra[0], "-framework");
        assert_eq!(extra[1], "CoreFoundation");
        assert_eq!(extra[2], "-framework");
        assert_eq!(extra[3], "Security");
    }

    #[test]
    fn test_split_link_flags_library_files() {
        let flags = vec![
            "mylib.lib".to_string(),
            "libfoo.a".to_string(),
            "bar.so".to_string(),
            "baz.dylib".to_string(),
            "qux.dll".to_string(),
        ];

        let (libs, extra) = split_link_flags(&flags);

        assert!(libs.is_empty());
        assert_eq!(extra.len(), 5);
        assert!(extra.contains(&"mylib.lib".to_string()));
        assert!(extra.contains(&"libfoo.a".to_string()));
        assert!(extra.contains(&"bar.so".to_string()));
    }

    #[test]
    fn test_split_link_flags_mixed() {
        let flags = vec![
            "-lm".to_string(),
            "-framework".to_string(),
            "Foundation".to_string(),
            "-lz".to_string(),
            "custom.a".to_string(),
            "-Wl,-rpath,/opt/lib".to_string(),
        ];

        let (libs, extra) = split_link_flags(&flags);

        assert_eq!(libs, vec!["m", "z"]);
        assert_eq!(extra.len(), 4);
        assert!(extra.contains(&"-framework".to_string()));
        assert!(extra.contains(&"Foundation".to_string()));
        assert!(extra.contains(&"custom.a".to_string()));
        assert!(extra.contains(&"-Wl,-rpath,/opt/lib".to_string()));
    }

    #[test]
    fn test_split_link_flags_empty_lib_name() {
        // -l with no library name should be skipped entirely
        let flags = vec!["-l".to_string(), "-lvalid".to_string()];

        let (libs, extra) = split_link_flags(&flags);

        assert_eq!(libs, vec!["valid"]);
        // "-l" without a name is skipped, not added to extra
        assert!(extra.is_empty());
    }

    #[test]
    fn test_split_link_flags_dangling_framework() {
        // -framework without a following name
        let flags = vec!["-lm".to_string(), "-framework".to_string()];

        let (libs, extra) = split_link_flags(&flags);

        assert_eq!(libs, vec!["m"]);
        // The dangling -framework is not added because iter.next() returns None
        assert!(extra.is_empty());
    }

    #[test]
    fn test_compile_step_fields() {
        let step = CompileStep {
            source: PathBuf::from("/src/main.c"),
            output: PathBuf::from("/obj/main.o"),
            package: "test_pkg".to_string(),
            target: "test_target".to_string(),
            include_dirs: vec![PathBuf::from("/include"), PathBuf::from("/usr/include")],
            defines: vec!["-DDEBUG".to_string(), "-DVERSION=1".to_string()],
            cflags: vec!["-Wall".to_string(), "-Werror".to_string()],
            lang: Language::C,
        };

        assert_eq!(step.source, PathBuf::from("/src/main.c"));
        assert_eq!(step.output, PathBuf::from("/obj/main.o"));
        assert_eq!(step.package, "test_pkg");
        assert_eq!(step.target, "test_target");
        assert_eq!(step.include_dirs.len(), 2);
        assert_eq!(step.defines.len(), 2);
        assert_eq!(step.cflags.len(), 2);
        assert_eq!(step.lang, Language::C);
    }

    #[test]
    fn test_link_step_exe() {
        let step = LinkStep {
            objects: vec![PathBuf::from("/obj/a.o"), PathBuf::from("/obj/b.o")],
            output: PathBuf::from("/bin/myapp"),
            package: "myapp".to_string(),
            target: "myapp".to_string(),
            kind: "exe".to_string(),
            lib_dirs: vec![PathBuf::from("/lib")],
            libs: vec!["-lm".to_string()],
            ldflags: vec![],
            use_cxx_linker: false,
        };

        assert_eq!(step.kind, "exe");
        assert!(!step.use_cxx_linker);
        assert_eq!(step.objects.len(), 2);
    }

    #[test]
    fn test_link_step_shared_lib() {
        let step = LinkStep {
            objects: vec![PathBuf::from("/obj/lib.o")],
            output: PathBuf::from("/lib/libfoo.so"),
            package: "foo".to_string(),
            target: "foo".to_string(),
            kind: "sharedlib".to_string(),
            lib_dirs: vec![],
            libs: vec![],
            ldflags: vec!["-shared".to_string()],
            use_cxx_linker: true,
        };

        assert_eq!(step.kind, "sharedlib");
        assert!(step.use_cxx_linker);
    }

    #[test]
    fn test_link_step_static_lib() {
        let step = LinkStep {
            objects: vec![PathBuf::from("/obj/lib.o")],
            output: PathBuf::from("/lib/libbar.a"),
            package: "bar".to_string(),
            target: "bar".to_string(),
            kind: "staticlib".to_string(),
            lib_dirs: vec![],
            libs: vec![],
            ldflags: vec![],
            use_cxx_linker: false,
        };

        assert_eq!(step.kind, "staticlib");
    }

    #[test]
    fn test_archive_step_fields() {
        let step = ArchiveStep {
            objects: vec![
                PathBuf::from("/obj/a.o"),
                PathBuf::from("/obj/b.o"),
                PathBuf::from("/obj/c.o"),
            ],
            output: PathBuf::from("/lib/libmylib.a"),
            package: "mylib".to_string(),
            target: "mylib".to_string(),
        };

        assert_eq!(step.objects.len(), 3);
        assert_eq!(step.output, PathBuf::from("/lib/libmylib.a"));
        assert_eq!(step.package, "mylib");
    }

    #[test]
    fn test_cmake_step_fields() {
        let step = CMakeStep {
            source_dir: PathBuf::from("/project"),
            build_dir: PathBuf::from("/project/build"),
            args: vec!["-DCMAKE_BUILD_TYPE=Release".to_string()],
            targets: vec!["all".to_string()],
            package: "cmake_pkg".to_string(),
            target: "cmake_target".to_string(),
        };

        assert_eq!(step.source_dir, PathBuf::from("/project"));
        assert_eq!(step.build_dir, PathBuf::from("/project/build"));
        assert_eq!(step.args.len(), 1);
        assert_eq!(step.targets.len(), 1);
    }

    #[test]
    fn test_custom_step_fields() {
        use std::collections::BTreeMap;

        let mut env = BTreeMap::new();
        env.insert("CC".to_string(), "clang".to_string());
        env.insert("CXX".to_string(), "clang++".to_string());

        let step = CustomStep {
            program: "make".to_string(),
            args: vec!["-j8".to_string()],
            cwd: PathBuf::from("/project"),
            env,
            outputs: vec![PathBuf::from("/project/out/result")],
            package: "custom_pkg".to_string(),
            target: "custom_target".to_string(),
        };

        assert_eq!(step.program, "make");
        assert_eq!(step.args.len(), 1);
        assert_eq!(step.cwd, PathBuf::from("/project"));
        assert_eq!(step.env.len(), 2);
        assert_eq!(step.outputs.len(), 1);
    }
}
