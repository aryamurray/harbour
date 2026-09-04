//! Native C/C++ compiler driver.
//!
//! Compiles C/C++ source files and links them into executables or libraries.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use rayon::prelude::*;

use crate::builder::context::BuildContext;
use crate::builder::fingerprint::{
    collect_header_deps, CompileFingerprint, FingerprintCache, LinkFingerprint,
    ToolchainFingerprint,
};
use crate::builder::plan::{
    ArchiveStep, BuildPlan, BuildStep, CMakeStep, CompileStep, CustomStep, LinkStep, MesonStep,
    PrebuildStep,
};
use crate::builder::toolchain::{ArchiveInput, CommandSpec, CompileInput, CxxOptions, LinkInput};
use crate::builder::util::parse_define_flags;
use crate::core::abi::AbiIdentity;
use crate::core::target::{Language, TargetKind};
use crate::ops::harbour_build::Artifact;
use crate::util::fs::ensure_dir;
use crate::util::process::ProcessBuilder;

/// Outcome of executing a build plan, including incremental-build stats.
///
/// `compiled`/`skipped` count only native `Compile` steps -- CMake, Meson,
/// and Custom recipe steps are always re-run (see the module-level note in
/// `execute` for why those are out of scope for this pass).
#[derive(Debug)]
pub struct BuildOutcome {
    /// Artifacts produced (or reused) by this build
    pub artifacts: Vec<Artifact>,
    /// Number of source files actually compiled
    pub compiled: usize,
    /// Number of source files skipped because their fingerprint was unchanged
    pub skipped: usize,
}

/// Name of the fingerprint cache file, stored under the build context's
/// per-profile (and, for cross builds, per-target) output directory. Since
/// `BuildContext::output_dir` already varies by profile/target, storing the
/// cache there for free gives us cache separation across profile switches
/// and cross-compilation targets without any extra bookkeeping.
const FINGERPRINT_CACHE_FILE: &str = ".harbour-fingerprints.json";

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

    /// Normalize a path for use as a fingerprint cache key.
    ///
    /// The cache is keyed by path, so the same file must always produce the
    /// same key across builds. Canonicalizing the file itself is unsafe here
    /// because the file may not exist yet on the *first* build (e.g. an
    /// object file before it's compiled) while it does exist afterwards --
    /// canonicalizing only sometimes would make the key inconsistent across
    /// runs. On macOS in particular, `/tmp` (and therefore `TMPDIR`-based
    /// paths) is a symlink to `/private/tmp`, so a path that is not
    /// canonicalized on one run and canonicalized on the next can silently
    /// change spelling and defeat the cache.
    ///
    /// Instead, canonicalize the parent *directory*, which reliably exists
    /// in both cases, and re-append the file name.
    fn normalize_cache_key(path: &Path) -> PathBuf {
        let Some(name) = path.file_name() else {
            return path.to_path_buf();
        };
        match path.parent() {
            Some(dir) => dir
                .canonicalize()
                .unwrap_or_else(|_| dir.to_path_buf())
                .join(name),
            None => path.to_path_buf(),
        }
    }

    /// Path to the fingerprint cache for this build context.
    ///
    /// Derived from `BuildContext::output_dir`, which is already
    /// per-profile (and per-target for cross builds), so switching profile
    /// or target automatically gets a separate cache with no extra
    /// bookkeeping and no risk of one profile/target reusing another's
    /// fingerprints.
    fn fingerprint_cache_path(&self) -> PathBuf {
        self.ctx.output_dir.join(FINGERPRINT_CACHE_FILE)
    }

    /// Build the [`ToolchainFingerprint`] for the current build context.
    fn toolchain_fingerprint(&self) -> ToolchainFingerprint {
        ToolchainFingerprint::new(
            &self.ctx.target.canonical(),
            &self.ctx.compiler.family,
            self.ctx.toolchain().compiler_path(),
            self.ctx.toolchain().cxx_compiler_path(),
            &self.ctx.compiler.version,
            self.cxx_opts.as_ref(),
            &self.ctx.profile_name,
        )
    }

    /// Assemble the complete set of per-file inputs that affect a compile's
    /// output: include directories, preprocessor defines, and cflags (both
    /// profile-derived and target-derived, plus the target's own). This
    /// must stay in sync with the actual command built in `compile()` --
    /// anything fed to the compiler that isn't captured here is a potential
    /// silent-stale-binary bug.
    fn compile_fingerprint_flags(&self, step: &CompileStep) -> Vec<String> {
        let mut parts = Vec::with_capacity(
            step.include_dirs.len() + step.defines.len() + step.cflags.len() + 4,
        );
        for dir in &step.include_dirs {
            parts.push(format!("-I{}", dir.display()));
        }
        parts.extend(step.defines.iter().cloned());
        parts.extend(self.ctx.profile_cflags());
        parts.extend(step.cflags.iter().cloned());
        parts
    }

    /// Compute the current fingerprint for a compile step and decide
    /// whether it can be skipped.
    ///
    /// A step is only skipped when its fingerprint matches the cached one
    /// *and* its output object file still exists on disk -- if the object
    /// was deleted (or never existed) we must compile regardless of what
    /// the fingerprint cache says.
    fn plan_compile(
        &self,
        step: &CompileStep,
        toolchain_fp: &ToolchainFingerprint,
        cache: &FingerprintCache,
    ) -> Result<(CompileFingerprint, bool)> {
        let flags = self.compile_fingerprint_flags(step);
        let headers = collect_header_deps(&step.source, &step.include_dirs);
        let fingerprint = CompileFingerprint::for_source(
            &step.source,
            toolchain_fp,
            &flags,
            &headers,
            step.lang,
        )?;

        let key = Self::normalize_cache_key(&step.source);
        let up_to_date = step.output.exists() && !cache.needs_compile(&key, &fingerprint);
        Ok((fingerprint, up_to_date))
    }

    /// Assemble the linker-level inputs that affect a link/archive step's
    /// output, beyond the object files themselves (which are hashed
    /// separately). Must stay in sync with `link_shared`/`link_executable`.
    fn link_fingerprint_flags(&self, step: &LinkStep) -> Vec<String> {
        let mut parts = Vec::new();
        parts.push(step.kind.clone());
        parts.push(step.use_cxx_linker.to_string());
        for dir in &step.lib_dirs {
            parts.push(format!("-L{}", dir.display()));
        }
        parts.extend(step.libs.iter().cloned());
        parts.extend(self.ctx.profile_ldflags());
        parts.extend(step.ldflags.iter().cloned());
        for framework in &step.frameworks {
            parts.push(format!("-framework {framework}"));
        }
        parts
    }

    /// Resolve library references in a link step to actual files on disk,
    /// where possible, so that a dependency library whose *content* changed
    /// (without its path changing) still triggers a relink.
    ///
    /// Bare `-lNAME` references are searched for across `lib_dirs` using
    /// common library file naming conventions. References that are already
    /// literal file paths (e.g. from `LibRef::Path`) are used directly.
    /// System libraries that can't be resolved this way are left out of the
    /// hashed set; they are still covered textually by
    /// `link_fingerprint_flags`, and are not expected to change between
    /// builds of the same project (same conservative tradeoff as unresolved
    /// `#include`s in header tracking).
    fn resolve_link_libs(step: &LinkStep) -> Vec<PathBuf> {
        let mut found = Vec::new();

        for raw in &step.libs {
            if is_lib_file_path(raw) {
                let direct = PathBuf::from(raw);
                if direct.is_file() {
                    found.push(direct);
                    continue;
                }
                for dir in &step.lib_dirs {
                    let candidate = dir.join(raw);
                    if candidate.is_file() {
                        found.push(candidate);
                        break;
                    }
                }
                continue;
            }

            let Some(name) = raw.strip_prefix("-l") else {
                continue;
            };
            if name.is_empty() {
                continue;
            }

            'dirs: for dir in &step.lib_dirs {
                for candidate_name in [
                    format!("lib{name}.a"),
                    format!("lib{name}.so"),
                    format!("lib{name}.dylib"),
                    format!("{name}.lib"),
                ] {
                    let candidate = dir.join(&candidate_name);
                    if candidate.is_file() {
                        found.push(candidate);
                        break 'dirs;
                    }
                }
            }
        }

        found
    }

    /// Execute the build plan.
    ///
    /// Processes all steps in order:
    /// - Compile steps run in parallel; each is skipped if its fingerprint
    ///   (source content, transitive headers, compile flags, and toolchain
    ///   identity) matches the cached fingerprint from a previous build and
    ///   its output object still exists.
    /// - Archive and Link steps run sequentially and are skipped under the
    ///   same fingerprint-match-plus-output-exists rule.
    /// - CMake, Meson, and Custom recipe steps are always re-run. Building
    ///   fingerprinting for arbitrary external build systems and custom
    ///   commands is out of scope for this pass; running them unconditionally
    ///   is the conservative choice (never skips work that might be needed).
    /// - Prebuild steps (pre-compile codegen, e.g. materializing a
    ///   generated header) always run too, for the same reason -- their
    ///   inputs aren't modeled any more precisely than a `Custom` step's
    ///   are, so there is nothing safe to fingerprint them against. They
    ///   run *before* anything else in this method, in particular before
    ///   the compile decision phase below: that phase scans every source's
    ///   transitive `#include`s to build each `CompileFingerprint`, and a
    ///   pre-build step's whole purpose is to create a file some source
    ///   `#include`s that does not exist in the repo checkout. Running
    ///   prebuild first is what makes "regenerated but byte-identical output
    ///   causes no rebuild" hold: the fingerprint over that header's
    ///   contents is computed *after* it has been (re)generated, so an
    ///   unchanged file hashes the same and the dependent compile is still
    ///   skipped even though the prebuild step itself always ran.
    pub fn execute(&self, plan: &BuildPlan, jobs: Option<usize>) -> Result<BuildOutcome> {
        // Set up rayon thread pool
        if let Some(j) = jobs {
            rayon::ThreadPoolBuilder::new()
                .num_threads(j)
                .build_global()
                .ok(); // Ignore if already set
        }

        // Run every pre-build step first, before any header scanning below:
        // their entire purpose is to materialize files (e.g. a generated
        // header) that a source's `#include` chain -- and therefore its
        // fingerprint -- may depend on. Plan order already respects package
        // build order (dependencies before dependents), so this also runs a
        // dependency's prebuild before a dependent's compile.
        for step in &plan.steps {
            if let BuildStep::Prebuild(s) = step {
                self.run_prebuild(s)?;
            }
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

        let cache_path = self.fingerprint_cache_path();
        let mut cache = FingerprintCache::load(&cache_path)?;
        let toolchain_fp = self.toolchain_fingerprint();

        let mut compiled_count = 0usize;
        let mut skipped_count = 0usize;

        if !compile_steps.is_empty() {
            // Decision phase: read-only against `cache`, safe to parallelize.
            let decisions: Vec<Result<(CompileFingerprint, bool)>> = compile_steps
                .par_iter()
                .map(|step| self.plan_compile(step, &toolchain_fp, &cache))
                .collect();
            let decisions: Vec<(CompileFingerprint, bool)> =
                decisions.into_iter().collect::<Result<Vec<_>>>()?;

            let to_run: Vec<usize> = decisions
                .iter()
                .enumerate()
                .filter(|(_, (_, up_to_date))| !up_to_date)
                .map(|(i, _)| i)
                .collect();

            compiled_count = to_run.len();
            skipped_count = decisions.len() - to_run.len();

            if !to_run.is_empty() {
                tracing::info!(
                    "Compiling {} file(s) ({} up to date)",
                    to_run.len(),
                    skipped_count
                );

                let compile_results: Vec<Result<()>> = to_run
                    .par_iter()
                    .map(|&i| self.compile(compile_steps[i]))
                    .collect();

                for result in compile_results {
                    result?;
                }
            } else {
                tracing::info!("All {} file(s) up to date", decisions.len());
            }

            // Only persist fingerprints after every compile in this batch
            // succeeded (the `?` above would have returned early otherwise),
            // so we never record success for a step that failed to build.
            for (step, (fingerprint, _)) in compile_steps.iter().zip(decisions) {
                cache.update_compile(Self::normalize_cache_key(&step.source), fingerprint);
            }
            cache.save(&cache_path)?;
        }

        // Process remaining steps sequentially
        let mut artifacts = Vec::new();

        for step in &plan.steps {
            match step {
                BuildStep::Compile(_) => {
                    // Already handled above
                }
                BuildStep::Archive(s) => {
                    let artifact = self.archive_incremental(s, &mut cache)?;
                    artifacts.push(artifact);
                }
                BuildStep::Link(s) => {
                    let artifact = self.link_incremental(s, &mut cache)?;
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
                BuildStep::Prebuild(_) => {
                    // Already run, up front, before the decision phase.
                }
            }
        }

        cache.save(&cache_path)?;

        Ok(BuildOutcome {
            artifacts,
            compiled: compiled_count,
            skipped: skipped_count,
        })
    }

    /// Create a static library, skipping the archive step if its
    /// fingerprint (object file contents) is unchanged and the archive
    /// still exists.
    fn archive_incremental(
        &self,
        step: &ArchiveStep,
        cache: &mut FingerprintCache,
    ) -> Result<Artifact> {
        let abi = AbiIdentity::new(
            self.ctx.target.clone(),
            self.ctx.compiler.clone(),
            TargetKind::StaticLib,
        );
        let fingerprint = LinkFingerprint::for_link(&step.objects, &[], &[], &abi)?;
        let key = Self::normalize_cache_key(&step.output);

        if step.output.exists() && !cache.needs_link(&key, &fingerprint) {
            cache.update_link(key, fingerprint);
            return Ok(Artifact {
                path: step.output.clone(),
                target: step.target.clone(),
            });
        }

        let artifact = self.archive(step)?;
        cache.update_link(key, fingerprint);
        Ok(artifact)
    }

    /// Link a shared library or executable, skipping the link step if its
    /// fingerprint (objects, resolved dependency libraries, and flags) is
    /// unchanged and the output still exists.
    fn link_incremental(&self, step: &LinkStep, cache: &mut FingerprintCache) -> Result<Artifact> {
        let kind = match step.kind.as_str() {
            "staticlib" => TargetKind::StaticLib,
            "sharedlib" => TargetKind::SharedLib,
            "exe" => TargetKind::Exe,
            other => bail!("unknown target kind: {}", other),
        };
        let abi = AbiIdentity::new(self.ctx.target.clone(), self.ctx.compiler.clone(), kind);

        let libs = Self::resolve_link_libs(step);
        let flags = self.link_fingerprint_flags(step);
        let fingerprint = LinkFingerprint::for_link(&step.objects, &libs, &flags, &abi)?;
        let key = Self::normalize_cache_key(&step.output);

        if step.output.exists() && !cache.needs_link(&key, &fingerprint) {
            cache.update_link(key, fingerprint);
            return Ok(Artifact {
                path: step.output.clone(),
                target: step.target.clone(),
            });
        }

        let artifact = self.link(step)?;
        cache.update_link(key, fingerprint);
        Ok(artifact)
    }

    /// Create a static library using the archive step.
    fn archive(&self, step: &ArchiveStep) -> Result<Artifact> {
        // Ensure output directory exists
        if let Some(parent) = step.output.parent() {
            ensure_dir(parent)?;
        }

        // Recreate the archive rather than updating it. `ar r` *replaces or
        // adds* members, matching them by file name, so any member whose
        // name is no longer produced survives forever -- a deleted or
        // renamed source keeps contributing its old object, and the linker
        // may resolve a symbol from the stale copy instead of the current
        // one.
        //
        // On Windows this produced a wrong program: when MSVC detection
        // fails between two builds the object extension flips from `.obj`
        // to `.o`, so the fresh object arrives under a *new* member name,
        // both copies sit in the archive, and the stale one wins. MSVC's
        // own `lib /OUT:` already creates a new archive; this makes the
        // GNU-style path behave the same way.
        if step.output.exists() {
            std::fs::remove_file(&step.output).with_context(|| {
                format!(
                    "removing the previous archive {} before recreating it",
                    step.output.display()
                )
            })?;
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

    /// Run a pre-build step. Structurally the same as [`Self::run_custom`]
    /// (and deliberately unconditional, for the same reason -- see the
    /// module-level note on `execute`), but kept as its own method since it
    /// runs at a different point in `execute` and takes a [`PrebuildStep`].
    fn run_prebuild(&self, step: &PrebuildStep) -> Result<()> {
        tracing::info!(
            "Running pre-build step for {}: {}",
            step.package,
            step.program
        );

        let mut cmd = ProcessBuilder::new(&step.program);

        for arg in &step.args {
            cmd = cmd.arg(arg);
        }

        cmd = cmd.cwd(&step.cwd);

        for (key, value) in &step.env {
            cmd = cmd.env(key, value);
        }

        let output = cmd.exec()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!(
                "pre-build command `{}` failed for {}:\n{}",
                step.program,
                step.package,
                stderr
            );
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

        // Where Harbour expects the artifacts, and where the sources are.
        // A foreign build system has no other way to learn either, and
        // without them a recipe cannot put its output where dependents look
        // for it. These were previously set only by an unused parallel
        // implementation in `builder::shim::custom_shim`, so in practice a
        // recipe saw none of them.
        cmd = cmd.env(
            "HARBOUR_ARTIFACT_DIR",
            step.artifact_dir.display().to_string(),
        );
        cmd = cmd.env(
            "HARBOUR_PACKAGE_ROOT",
            step.package_root.display().to_string(),
        );

        // Manifest `env` last, so a recipe can override the above.
        for (key, value) in &step.env {
            cmd = cmd.env(key, value);
        }

        let output = cmd.exec()?;

        // A foreign build system's own diagnostics are the only useful signal
        // when a recipe misbehaves, and discarding them on success made
        // configure-style builds opaque. Emitted at debug so `-v` reaches
        // them without cluttering a normal build.
        if !output.stdout.is_empty() {
            tracing::debug!(
                "custom command `{}` for {} stdout:\n{}",
                step.program,
                step.package,
                String::from_utf8_lossy(&output.stdout).trim_end()
            );
        }
        if !output.stderr.is_empty() {
            tracing::debug!(
                "custom command `{}` for {} stderr:\n{}",
                step.program,
                step.package,
                String::from_utf8_lossy(&output.stderr).trim_end()
            );
        }

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            bail!(
                "custom command `{}` failed for {}:\n{}{}",
                step.program,
                step.package,
                stdout.trim_end(),
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
        let (libs, lib_paths, mut extra_ldflags) = split_link_flags(&step.libs);
        let mut objects = step.objects.clone();
        objects.extend(lib_paths.into_iter().map(PathBuf::from));
        let mut ldflags = self.ctx.profile_ldflags();
        ldflags.extend(step.ldflags.iter().cloned());
        ldflags.append(&mut extra_ldflags);

        let input = LinkInput {
            objects,
            output: step.output.clone(),
            lib_dirs: step.lib_dirs.clone(),
            libs,
            ldflags,
            frameworks: step.frameworks.clone(),
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
        let (libs, lib_paths, mut extra_ldflags) = split_link_flags(&step.libs);
        let mut objects = step.objects.clone();
        objects.extend(lib_paths.into_iter().map(PathBuf::from));
        let mut ldflags = self.ctx.profile_ldflags();
        ldflags.extend(step.ldflags.iter().cloned());
        ldflags.append(&mut extra_ldflags);

        let input = LinkInput {
            objects,
            output: step.output.clone(),
            lib_dirs: step.lib_dirs.clone(),
            libs,
            ldflags,
            frameworks: step.frameworks.clone(),
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

/// Whether a raw library reference string looks like a literal library file
/// path rather than a bare `-lNAME` reference.
fn is_lib_file_path(raw: &str) -> bool {
    raw.ends_with(".lib")
        || raw.ends_with(".a")
        || raw.ends_with(".so")
        || raw.ends_with(".dylib")
        || raw.ends_with(".dll")
}

/// Split the raw linker-reference strings collected on a [`LinkStep`] into
/// three groups, by how they need to be positioned on the final command
/// line:
///
/// - bare `-lNAME` library names, rendered as `-lNAME` after `-L` search
///   paths (ordinary system libraries -- order among these rarely matters).
/// - literal library file paths (typically resolved dependency archives
///   from `SurfaceResolver::link_dep_order`), which the caller must append
///   directly after the real object files. A traditional single-pass,
///   left-to-right static linker only pulls members from an archive to
///   satisfy symbols that are *already* undefined, so an archive must
///   appear after the object/archive that needs it and before the system
///   libraries that satisfy whatever symbols remain -- it cannot be routed
///   through the `-lNAME`/ldflags tail the way it was previously.
/// - everything else (`-framework NAME` pairs, `-Wl,...` flags, etc.),
///   rendered as trailing ldflags.
fn split_link_flags(flags: &[String]) -> (Vec<String>, Vec<String>, Vec<String>) {
    let mut libs = Vec::new();
    let mut lib_paths = Vec::new();
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

        if is_lib_file_path(flag) {
            lib_paths.push(flag.clone());
            continue;
        }

        extra.push(flag.clone());
    }

    (libs, lib_paths, extra)
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

        let (libs, lib_paths, extra) = split_link_flags(&flags);

        assert_eq!(libs, vec!["m", "pthread", "z"]);
        assert!(lib_paths.is_empty());
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

        let (libs, lib_paths, extra) = split_link_flags(&flags);

        assert!(libs.is_empty());
        assert!(lib_paths.is_empty());
        assert_eq!(extra.len(), 4);
        assert_eq!(extra[0], "-framework");
        assert_eq!(extra[1], "CoreFoundation");
        assert_eq!(extra[2], "-framework");
        assert_eq!(extra[3], "Security");
    }

    #[test]
    fn test_split_link_flags_library_files() {
        // Literal library file paths (e.g. resolved dependency archives)
        // are now their own group, positioned like object files by the
        // caller -- not routed through the ldflags tail.
        let flags = vec![
            "mylib.lib".to_string(),
            "libfoo.a".to_string(),
            "bar.so".to_string(),
            "baz.dylib".to_string(),
            "qux.dll".to_string(),
        ];

        let (libs, lib_paths, extra) = split_link_flags(&flags);

        assert!(libs.is_empty());
        assert!(extra.is_empty());
        assert_eq!(lib_paths.len(), 5);
        assert!(lib_paths.contains(&"mylib.lib".to_string()));
        assert!(lib_paths.contains(&"libfoo.a".to_string()));
        assert!(lib_paths.contains(&"bar.so".to_string()));
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

        let (libs, lib_paths, extra) = split_link_flags(&flags);

        assert_eq!(libs, vec!["m", "z"]);
        assert_eq!(lib_paths, vec!["custom.a".to_string()]);
        assert_eq!(extra.len(), 3);
        assert!(extra.contains(&"-framework".to_string()));
        assert!(extra.contains(&"Foundation".to_string()));
        assert!(extra.contains(&"-Wl,-rpath,/opt/lib".to_string()));
    }

    #[test]
    fn test_split_link_flags_empty_lib_name() {
        // -l with no library name should be skipped entirely
        let flags = vec!["-l".to_string(), "-lvalid".to_string()];

        let (libs, lib_paths, extra) = split_link_flags(&flags);

        assert_eq!(libs, vec!["valid"]);
        // "-l" without a name is skipped, not added to extra
        assert!(lib_paths.is_empty());
        assert!(extra.is_empty());
    }

    #[test]
    fn test_split_link_flags_dangling_framework() {
        // -framework without a following name
        let flags = vec!["-lm".to_string(), "-framework".to_string()];

        let (libs, lib_paths, extra) = split_link_flags(&flags);

        assert_eq!(libs, vec!["m"]);
        // The dangling -framework is not added because iter.next() returns None
        assert!(lib_paths.is_empty());
        assert!(extra.is_empty());
    }

    #[test]
    fn test_split_link_flags_preserves_lib_path_order() {
        // Dependency archives must keep the order the caller gave them in
        // (dependents before dependencies) -- verify it survives the split.
        let flags = vec![
            "/deps/liblibb-0.1.0/lib/liblibb.a".to_string(),
            "/deps/liba-0.1.0/lib/libliba.a".to_string(),
            "-lm".to_string(),
        ];

        let (libs, lib_paths, extra) = split_link_flags(&flags);

        assert_eq!(libs, vec!["m"]);
        assert!(extra.is_empty());
        assert_eq!(
            lib_paths,
            vec![
                "/deps/liblibb-0.1.0/lib/liblibb.a".to_string(),
                "/deps/liba-0.1.0/lib/libliba.a".to_string(),
            ]
        );
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
            frameworks: vec![],
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
            frameworks: vec![],
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
            frameworks: vec![],
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
            artifact_dir: PathBuf::new(),
            package_root: PathBuf::new(),
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
