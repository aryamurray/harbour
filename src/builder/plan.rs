//! Build plan generation.
//!
//! A BuildPlan describes all compilation and linking steps needed to build
//! a workspace. Steps can be native compilation, CMake invocation, or custom
//! commands.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::builder::context::BuildContext;
use crate::builder::surface_resolver::SurfaceResolver;
use crate::builder::toolchain::ToolchainPlatform;
use crate::core::abi::AbiSurfaceKey;
use crate::core::target::{BuildRecipe, CStandardSpec, Language, TargetKind};
use crate::resolver::Resolve;
use crate::sources::SourceCache;
use crate::util::fs::glob_files_excluding;
use crate::util::process::ProcessBuilder;

/// A complete build plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildPlan {
    /// All build steps in execution order
    pub steps: Vec<BuildStep>,

    /// Compilation steps (subset of steps, for compile_commands.json)
    pub compile_steps: Vec<CompileStep>,

    /// Link steps (subset of steps, kept for compatibility)
    pub link_steps: Vec<LinkStep>,

    /// Build order (package IDs in topological order)
    pub build_order: Vec<String>,
}

/// A build step in the plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BuildStep {
    /// Compile a source file to an object file
    Compile(CompileStep),
    /// Create a static library from object files
    Archive(ArchiveStep),
    /// Link objects into a shared library or executable
    Link(LinkStep),
    /// Run CMake to configure and build
    CMake(CMakeStep),
    /// Run Meson to configure and build
    Meson(MesonStep),
    /// Run a custom command
    Custom(CustomStep),
    /// Run a pre-build step (e.g. codegen) before native compilation
    Prebuild(PrebuildStep),
}

/// A step to create a static library.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveStep {
    /// Object files to archive
    pub objects: Vec<PathBuf>,
    /// Output archive file
    pub output: PathBuf,
    /// Package this belongs to
    pub package: String,
    /// Target name
    pub target: String,
    /// Surface-derived ABI inputs for this target's cache key.
    ///
    /// `#[serde(default)]` so a plan written by an older Harbour still
    /// deserialises; the effect of the default is a rebuild, which is the
    /// safe direction.
    #[serde(default)]
    pub abi: AbiSurfaceKey,
}

/// A CMake build step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CMakeStep {
    /// Source directory containing CMakeLists.txt
    pub source_dir: PathBuf,
    /// Build directory for CMake output
    pub build_dir: PathBuf,
    /// Additional CMake arguments
    pub args: Vec<String>,
    /// CMake targets to build (empty = all)
    pub targets: Vec<String>,
    /// Package this belongs to
    pub package: String,
    /// Target name
    pub target: String,
}

/// A Meson build step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MesonStep {
    /// Source directory containing meson.build
    pub source_dir: PathBuf,
    /// Build directory for Meson output
    pub build_dir: PathBuf,
    /// Additional Meson options (-D flags)
    pub options: Vec<String>,
    /// Meson targets to build (empty = all)
    pub targets: Vec<String>,
    /// Package this belongs to
    pub package: String,
    /// Target name
    pub target: String,
}

/// A custom command step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomStep {
    /// Program to execute
    pub program: String,
    /// Arguments
    pub args: Vec<String>,
    /// Working directory (resolved absolute path)
    pub cwd: PathBuf,
    /// Environment variables to set
    pub env: BTreeMap<String, String>,
    /// Expected outputs (for fingerprinting)
    pub outputs: Vec<PathBuf>,
    /// Directory Harbour expects this target's artifacts in.
    #[serde(default)]
    pub artifact_dir: PathBuf,
    /// This package's root directory.
    #[serde(default)]
    pub package_root: PathBuf,
    /// Package this belongs to
    pub package: String,
    /// Target name
    pub target: String,
}

/// A pre-build step: a code generator, run during planning to materialize
/// files (a generated header, or a whole generated translation unit) that the
/// rest of the plan is then derived from.
///
/// Unlike every other [`BuildStep`], this one has *already run* by the time a
/// plan reaches `NativeBuilder::execute`. It has to: a target's compile steps
/// come from expanding its source globs, and a generator's output cannot be
/// globbed before the generator has produced it. See
/// [`BuildPlan::with_root_packages`] for why planning owns the side effect.
/// The variant is kept in the plan as a record of what was generated and how,
/// so a plan still fully describes the build it produced.
///
/// Structurally identical to [`CustomStep`], but kept a distinct type so the
/// two cannot be confused: a `CustomStep` is pending work, a `PrebuildStep`
/// is completed work.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrebuildStep {
    /// Program to execute
    pub program: String,
    /// Arguments
    pub args: Vec<String>,
    /// Working directory (resolved absolute path)
    pub cwd: PathBuf,
    /// Environment variables to set
    pub env: BTreeMap<String, String>,
    /// Expected outputs (informational; pre-build steps are never skipped)
    pub outputs: Vec<PathBuf>,
    /// Package this belongs to
    pub package: String,
    /// Target name
    pub target: String,
}

impl PrebuildStep {
    /// Run the generator.
    ///
    /// Deliberately unconditional: a pre-build step's inputs are not modeled
    /// (`CustomCommand::inputs` is advisory), so there is nothing sound to
    /// fingerprint it against, and skipping it could leave a stale generated
    /// source in the compile set. Re-running a generator that produces
    /// byte-identical output is still cheap overall, because the compile
    /// fingerprints downstream are taken *after* this runs and will match.
    pub fn run(&self) -> Result<()> {
        tracing::info!(
            "Running pre-build step for {}: {}",
            self.package,
            self.program
        );

        let mut cmd = ProcessBuilder::new(&self.program);
        for arg in &self.args {
            cmd = cmd.arg(arg);
        }
        cmd = cmd.cwd(&self.cwd);
        for (key, value) in &self.env {
            cmd = cmd.env(key, value);
        }

        let output = cmd.exec()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!(
                "pre-build command `{}` failed for {}:\n{}",
                self.program,
                self.package,
                stderr
            );
        }

        // A generator that exits 0 without writing what it declared is the
        // failure mode this whole ordering fix exists to make visible: the
        // named output would simply be absent from the compile set and the
        // link would fail on a symbol with no obvious owner. Say so here,
        // where the cause is still in hand.
        let missing: Vec<&PathBuf> = self.outputs.iter().filter(|o| !o.exists()).collect();
        if !missing.is_empty() {
            bail!(
                "pre-build command `{}` for {} succeeded but did not produce {} declared \
                 output(s):\n  {}\n\
                 hint: `outputs` lists what this step must generate; paths are relative to \
                 the package root",
                self.program,
                self.package,
                missing.len(),
                missing
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join("\n  ")
            );
        }

        Ok(())
    }
}

/// A single compilation step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompileStep {
    /// Source file
    pub source: PathBuf,

    /// Output object file
    pub output: PathBuf,

    /// Package this belongs to
    pub package: String,

    /// Target name
    pub target: String,

    /// Include directories
    pub include_dirs: Vec<PathBuf>,

    /// Preprocessor defines
    pub defines: Vec<String>,

    /// Compiler flags
    pub cflags: Vec<String>,

    /// Source language (C or C++)
    #[serde(default)]
    pub lang: Language,

    /// The target's C standard, if it pinned one.
    ///
    /// Carried on the step rather than looked up again later, for the same
    /// reason `abi` is carried on the link step: `native.rs` and the
    /// compile-database writer both see only these flattened steps, so
    /// anything not on the step cannot reach the compiler *or* the
    /// fingerprint. That is precisely how `c_std` came to be parsed,
    /// validated, documented in `MANIFEST.md`, and emitted nowhere.
    #[serde(default)]
    pub c_std: Option<CStandardSpec>,
}

/// A single link step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkStep {
    /// Object files to link
    pub objects: Vec<PathBuf>,

    /// Output file
    pub output: PathBuf,

    /// Package this belongs to
    pub package: String,

    /// Target name
    pub target: String,

    /// Target kind
    pub kind: String,

    /// Library search paths
    pub lib_dirs: Vec<PathBuf>,

    /// Libraries to link
    pub libs: Vec<String>,

    /// Linker flags
    pub ldflags: Vec<String>,

    /// macOS frameworks to link (without the `-framework` prefix)
    #[serde(default)]
    pub frameworks: Vec<String>,

    /// Whether to use C++ linker driver (g++/clang++ instead of gcc/clang)
    #[serde(default)]
    pub use_cxx_linker: bool,

    /// Surface-derived ABI inputs for this target's cache key. See
    /// [`ArchiveStep::abi`].
    #[serde(default)]
    pub abi: AbiSurfaceKey,
}

use crate::core::PackageId;

impl BuildPlan {
    /// Create a new build plan from a resolve and build context.
    ///
    /// If `target_filter` is provided, only targets matching the filter will be
    /// built for the root package(s). Dependencies are always built in full.
    ///
    /// The build plan respects each target's recipe:
    /// - Native: Uses standard compile/link steps
    /// - CMake: Generates CMake configuration and build steps
    /// - Custom: Generates custom command steps
    pub fn new(
        ctx: &BuildContext,
        resolve: &Resolve,
        source_cache: &mut SourceCache,
        target_filter: Option<&[String]>,
    ) -> Result<Self> {
        // Use the last package in topological order as the root for backwards compatibility
        let root_pkg_ids: Vec<PackageId> = resolve
            .topological_order()
            .last()
            .map(|id| vec![*id])
            .unwrap_or_default();

        Self::with_root_packages(ctx, resolve, source_cache, &root_pkg_ids, target_filter)
    }

    /// Create a new build plan with explicit root packages.
    ///
    /// Root packages are treated as first-class build targets (output to output_dir),
    /// while their dependencies go to deps_dir.
    ///
    /// If `target_filter` is provided, only targets matching the filter will be
    /// built for the root packages. Dependencies are always built in full.
    pub fn with_root_packages(
        ctx: &BuildContext,
        resolve: &Resolve,
        source_cache: &mut SourceCache,
        root_packages: &[PackageId],
        target_filter: Option<&[String]>,
    ) -> Result<Self> {
        let mut steps = Vec::new();
        let mut compile_steps = Vec::new();
        let mut link_steps = Vec::new();

        // Create surface resolver
        let mut surface_resolver = SurfaceResolver::new(resolve, &ctx.platform);
        surface_resolver.load_packages(source_cache)?;

        // Check every package's declared target requirements before compiling
        // anything. The whole point is to answer "can this build for this
        // target" in one clear sentence naming the package, rather than as a
        // cascade of missing-header errors a thousand files into a
        // dependency nobody was thinking about.
        check_target_requirements(ctx, &surface_resolver)?;

        // Build order: dependencies before dependents
        let build_order: Vec<String> = resolve
            .topological_order()
            .iter()
            .map(|id| format!("{} {}", id.name(), id.version()))
            .collect();

        // Determine root package IDs
        let root_pkg_set: std::collections::HashSet<PackageId> =
            root_packages.iter().copied().collect();

        // Process each package in build order
        for pkg_id in resolve.topological_order() {
            let package = surface_resolver
                .get_package(pkg_id)
                .ok_or_else(|| anyhow::anyhow!("package not loaded: {}", pkg_id))?;

            // Determine which targets to build
            // For root packages, apply filter if specified
            // For dependencies, build all targets
            let is_root = root_pkg_set.contains(&pkg_id);
            let targets_to_build: Vec<_> = if let (true, Some(filter)) = (is_root, target_filter) {
                package
                    .targets()
                    .iter()
                    .filter(|t| filter.iter().any(|f| f == t.name.as_str()))
                    .collect()
            } else {
                package.targets().iter().collect()
            };

            for target in targets_to_build {
                // Determine output directory
                // Root packages go to output_dir/<pkg>/ (for multi-package workspaces)
                // Dependencies go to deps_dir/<pkg>-<version>/
                let target_output_dir = if is_root {
                    if root_packages.len() > 1 {
                        // Multi-root workspace: each package gets its own directory
                        ctx.output_dir.join(pkg_id.name().as_str())
                    } else {
                        // Single root: output directly to output_dir
                        ctx.output_dir.clone()
                    }
                } else {
                    // Dependencies go to deps_dir
                    ctx.deps_dir
                        .join(format!("{}-{}", pkg_id.name(), pkg_id.version()))
                };

                let obj_dir = target_output_dir.join("obj").join(target.name.as_str());
                let lib_dir = target_output_dir.join("lib");
                let bin_dir = target_output_dir.join("bin");

                // A non-native recipe is a documented but second-class path:
                // Harbour does not emit its compile commands, so the target is
                // never fingerprinted (it rebuilds in full on every build),
                // receives no surface flags, and contributes nothing to
                // compile_commands.json. ARCHITECTURE.md's "Package Build
                // Strategy" records why. Saying so at build time keeps that
                // promise honest instead of leaving the degradation silent.
                warn_if_non_native(&target.recipe, pkg_id.name().as_str(), target.name.as_str());

                // `arch` is the literal triple component, so a package keyed
                // on one spelling silently does not apply to another. Say so
                // when it looks like that is what happened.
                if let Some(message) = arch_spelling_advisory(
                    &ctx.platform.arch,
                    target,
                    pkg_id.name().as_str(),
                    target.name.as_str(),
                ) {
                    tracing::warn!("{message}");
                }

                // Handle recipe dispatch
                match &target.recipe {
                    Some(BuildRecipe::CMake {
                        source_dir,
                        args,
                        targets: cmake_targets,
                    }) => {
                        // CMake recipe - generate CMake step.
                        //
                        // `source_dir`, when given, is relative to *this
                        // package's* root -- the same convention the
                        // `Custom` recipe arm uses for `cwd` below. Joining
                        // it onto `package.root()` (rather than using it
                        // verbatim) matters once this package is itself a
                        // dependency: the process cwd during the build is
                        // the *root* package's directory, so a bare
                        // `source_dir` would resolve against the wrong
                        // package and CMake would fail to find
                        // CMakeLists.txt anywhere but the root build.
                        let src_dir =
                            resolve_recipe_source_dir(source_dir.as_deref(), package.root());
                        let build_dir = target_output_dir.join("cmake-build");

                        steps.push(BuildStep::CMake(CMakeStep {
                            source_dir: src_dir,
                            build_dir,
                            args: args.clone(),
                            targets: cmake_targets.clone(),
                            package: pkg_id.name().to_string(),
                            target: target.name.to_string(),
                        }));
                    }
                    Some(BuildRecipe::Custom {
                        steps: custom_steps,
                    }) => {
                        // Custom recipe - generate custom command steps
                        for cmd in custom_steps {
                            let cwd = cmd
                                .cwd
                                .clone()
                                .map(|c| package.root().join(c))
                                .unwrap_or_else(|| package.root().to_path_buf());

                            // Same contract a `prebuild` generator gets, and
                            // for the same reason: a `./configure` run by a
                            // recipe needs `CC` and the triple as badly as
                            // perlasm does. Manifest `env` last, as before.
                            //
                            // No probe answers: probes are answered in the
                            // `Native` arm below, and a target built by a
                            // foreign recipe never reaches it. Passing
                            // `None` rather than an empty set keeps that
                            // visible -- a recipe sees no `HARBOUR_PROBE_*`
                            // at all, which is different from seeing
                            // answers that are all "no".
                            let mut env = generator_env(ctx, package.root(), &lib_dir, None);
                            env.extend(cmd.env.clone());

                            steps.push(BuildStep::Custom(CustomStep {
                                program: cmd.program.clone(),
                                args: cmd.args.clone(),
                                cwd,
                                env,
                                outputs: cmd
                                    .outputs
                                    .iter()
                                    .map(|o| package.root().join(o))
                                    .collect(),
                                package: pkg_id.name().to_string(),
                                target: target.name.to_string(),
                                artifact_dir: lib_dir.clone(),
                                package_root: package.root().to_path_buf(),
                            }));
                        }
                    }
                    Some(BuildRecipe::Meson {
                        source_dir,
                        options,
                        targets: meson_targets,
                    }) => {
                        // Meson recipe - generate Meson step. Same
                        // package-relative `source_dir` convention as the
                        // `CMake` arm above (and the same bug it was fixed
                        // alongside): join onto `package.root()` so a
                        // dependency's relative `source_dir` anchors to that
                        // dependency, not to whatever the root package
                        // happens to be.
                        let src_dir =
                            resolve_recipe_source_dir(source_dir.as_deref(), package.root());
                        let build_dir = target_output_dir.join("meson-build");

                        steps.push(BuildStep::Meson(MesonStep {
                            source_dir: src_dir,
                            build_dir,
                            options: options.clone(),
                            targets: meson_targets.clone(),
                            package: pkg_id.name().to_string(),
                            target: target.name.to_string(),
                        }));
                    }
                    Some(BuildRecipe::Native) | None => {
                        // Skip header-only targets - they have no compile/link steps
                        if target.kind == TargetKind::HeaderOnly {
                            tracing::debug!(
                                "skipping header-only target {} from package {}",
                                target.name,
                                pkg_id.name()
                            );
                            continue;
                        }

                        // Native recipe (default) - use standard compile/link
                        let mut compile_surface =
                            surface_resolver.resolve_compile_surface(pkg_id, target)?;
                        let mut link_surface =
                            surface_resolver.resolve_link_surface(pkg_id, target, &ctx.deps_dir)?;

                        ctx.merge_vcpkg_dirs(&mut compile_surface, &mut link_surface);

                        // Answer this target's configure-style probes, and
                        // fold the answers into the surface as defines.
                        //
                        // The position is chosen, not convenient. It is:
                        //
                        // - **after** the surface fold, because a probe needs
                        //   the include path to be meaningful: "does `zlib.h`
                        //   exist" has no answer without the `-I` a
                        //   dependency contributes. This is the one ordering
                        //   constraint that is easy to miss, and it is why
                        //   probes cannot simply run at the top of the loop.
                        // - **before** the pre-build generators below, so a
                        //   generator can eventually be handed probe answers.
                        // - **before** source resolution, so a future
                        //   probe-conditional source list is expressible.
                        // - **before** compile-step construction, which is
                        //   the whole point: the defines must be on
                        //   `compile_surface` before `CompileStep` is built,
                        //   or they reach neither the compiler nor the
                        //   fingerprint. `merge_vcpkg_dirs` above mutates the
                        //   same surface in place at the same seam, so this
                        //   follows an existing precedent rather than
                        //   inventing a mechanism.
                        //
                        // Probes deliberately see the surface *without* any
                        // probe results, which is what makes "a probe cannot
                        // read another probe's answer" true by construction
                        // rather than by policy -- there is no point in the
                        // pipeline at which it could.
                        //
                        // The answers are kept in scope past this block
                        // rather than consumed by it, because the pre-build
                        // generators below are handed them too
                        // (`HARBOUR_PROBE_*`). That ordering already existed
                        // and is what makes it possible: probes are answered
                        // here, generators run a few lines down, and sources
                        // are resolved after both.
                        let probe_results = if !target.probes.is_empty() {
                            // `answer_for_target` is the single entry point,
                            // shared with `harbour flags`. Assembling a
                            // `ProbeEnv` here instead would give the build
                            // and the inspection command two implementations
                            // of one question -- which is exactly how
                            // `harbour flags` came to report flags the build
                            // never used (2026-09-07 audit, section 2.4).
                            let results = crate::builder::probe::answer_for_target(
                                ctx,
                                &pkg_id,
                                target,
                                &compile_surface,
                            )?;
                            tracing::debug!(
                                package = %pkg_id.name(),
                                target = %target.name,
                                probes = results.answers.len(),
                                measured = results.measured,
                                "probes answered"
                            );
                            // Appended, never sorted, and appended *after*
                            // the declared surface so a manifest can still
                            // override a probe-derived define with a literal
                            // one (cflags and defines are last-wins at the
                            // compiler).
                            // Private to this target's own translation
                            // units. There is no public option, and the
                            // omission was discovered rather than planned: a
                            // `visibility = "public"` field was implemented
                            // and branched on right here, and a test then
                            // proved it does not propagate. A dependent's
                            // surface is folded from each dependency's
                            // *declared* `surface.compile.public` (see
                            // `SurfaceResolver::resolve_compile_surface`),
                            // and a measured answer is in no manifest -- so
                            // the consumer failed to compile on an undefined
                            // `SIZEOF_LONG` while the field looked like it
                            // worked. Making it work means feeding answers
                            // back into the resolver's view of the
                            // dependency, which is a change to the fold, not
                            // to probes.
                            // `contribution` is the only place `emit` is
                            // interpreted; `harbour flags` reads the same
                            // function. A `match` here and another there is
                            // how that command came to report flags the
                            // build never used.
                            match results.contribution(&target.probes) {
                                crate::builder::probe::ProbeContribution::Defines(defs) => {
                                    compile_surface.defines.extend(defs);
                                }
                                crate::builder::probe::ProbeContribution::IncludeDir(dir) => {
                                    // At the *front*: `-I` is
                                    // first-match-wins, and a package that
                                    // also vendors a `config.h` of the same
                                    // name must get the generated one. That
                                    // is the whole migration path off the
                                    // vendored file -- add the probes, and
                                    // the stale copy stops being reachable.
                                    compile_surface.include_dirs.insert(0, dir);
                                }
                            }
                            Some(results)
                        } else {
                            None
                        };

                        // Run this target's pre-build generators now, before
                        // its sources are resolved below.
                        //
                        // This is the one place planning is not a pure
                        // function, and it has to be. A generator's whole
                        // purpose is to produce files that are compiled --
                        // `sources = ["generated/*.c"]` -- and the compile
                        // steps for those files come from expanding that glob.
                        // Expand it before the generator runs and the glob
                        // matches nothing, so the generated translation unit
                        // is silently absent from the plan: the link fails on
                        // a symbol nobody appears to define, or worse, for a
                        // `staticlib` target it succeeds and ships an archive
                        // with a member missing. The set of compile steps is
                        // simply not computable without running the generator
                        // first, so an accurate plan requires the side effect.
                        //
                        // Consequences, accepted deliberately:
                        // - `harbour build --plan` runs generators. The
                        //   alternative is emitting a plan that is wrong for
                        //   exactly the projects that use this feature.
                        // - `NativeBuilder::execute` does *not* re-run
                        //   `Prebuild` steps; they are a record, not pending
                        //   work. Running them here rather than there also
                        //   still satisfies the ordering the fingerprinting
                        //   relies on ("generated files exist before any
                        //   `#include` scanning"), since planning precedes
                        //   execution.
                        //
                        // Packages are walked in topological order and a
                        // target's own prebuild runs before its sources are
                        // read, so a dependency's generated headers are in
                        // place before any dependent is planned.
                        //
                        // `resolved_prebuild` folds in `[[targets.X.when]]`
                        // generators whose condition matches the platform
                        // we're building *for*, on the same terms as
                        // `resolved_sources` below -- a generator is often
                        // the most platform-specific step a package has.
                        let pkg_features = surface_resolver.features_for(pkg_id);
                        for cmd in &target.resolved_prebuild(&ctx.platform, &pkg_features) {
                            let cwd = cmd
                                .cwd
                                .clone()
                                .map(|c| package.root().join(c))
                                .unwrap_or_else(|| package.root().to_path_buf());

                            // Harbour's contract first, the manifest's own
                            // `env` second, so a block can still override
                            // any of it -- exactly as `recipe` always
                            // could. Baked into the step rather than
                            // applied in `PrebuildStep::run` so that the
                            // plan is a faithful record of the environment
                            // the generator actually saw, which is what
                            // `harbour build --plan` then shows.
                            let mut env = generator_env(
                                ctx,
                                package.root(),
                                &lib_dir,
                                probe_results.as_ref(),
                            );
                            env.extend(cmd.env.clone());

                            steps.push(BuildStep::Prebuild(PrebuildStep {
                                program: cmd.program.clone(),
                                args: cmd.args.clone(),
                                cwd,
                                env,
                                outputs: cmd
                                    .outputs
                                    .iter()
                                    .map(|o| package.root().join(o))
                                    .collect(),
                                package: pkg_id.name().to_string(),
                                target: target.name.to_string(),
                            }));

                            // `steps.last()` is the step just pushed.
                            let Some(BuildStep::Prebuild(step)) = steps.last() else {
                                unreachable!("just pushed a Prebuild step");
                            };
                            step.run()?;
                        }

                        // Find source files. `resolved_sources` folds in any
                        // `[[targets.X.when]]` entries whose condition
                        // matches the platform we're building *for* (never
                        // the host -- see `TargetPlatform::for_target`), so
                        // cross-compiling selects the right source set (e.g.
                        // arm/*.c only when the target triple is actually
                        // aarch64).
                        let (target_sources, target_exclude) =
                            target.resolved_sources(&ctx.platform, &pkg_features);
                        let sources =
                            glob_files_excluding(package.root(), &target_sources, &target_exclude)?;

                        // A pattern that names one file and matches nothing is
                        // a mistake, not an empty set. Globs stay permissive --
                        // `src/**/*.S` legitimately matches nothing on a
                        // platform with no assembly -- but a generated manifest
                        // lists sources individually, and a vendored file that
                        // failed to ship would otherwise disappear while the
                        // defines that describe it remain. For openssl that
                        // means claiming an assembly implementation exists for
                        // a primitive whose object is absent.
                        //
                        // This stays unconditional for targets with
                        // `prebuild`: their generators (including any
                        // conditional ones) have already run above, so a
                        // named generated source that is still absent
                        // really is missing, and saying so here is better
                        // than a link error later.
                        let missing: Vec<&String> = target_sources
                            .iter()
                            .filter(|p| !p.contains(['*', '?', '[', '{']))
                            .filter(|p| !package.root().join(p).exists())
                            .collect();
                        if !missing.is_empty() {
                            bail!(
                                "target '{}' lists {} source(s) that do not exist:\n  {}\n\
                                 hint: these are named individually rather than matched by a \
                                 glob, so each is expected to be present; a generated or \
                                 vendored file may be missing",
                                target.name,
                                missing.len(),
                                missing
                                    .iter()
                                    .map(|p| p.as_str())
                                    .collect::<Vec<_>>()
                                    .join("\n  ")
                            );
                        }

                        // Validate source extensions match target language
                        if target.lang == Language::C {
                            for source in &sources {
                                if is_cpp_extension(source) {
                                    bail!(
                                        "target '{}' has lang=c but source '{}' has C++ extension\n\
                                         hint: set lang = 'c++' in [targets.{}]",
                                        target.name,
                                        source.display(),
                                        target.name
                                    );
                                }
                            }
                        }

                        // MSVC assembles with a separate, architecture-specific
                        // assembler (`ml64.exe`, `armasm64.exe`) rather than
                        // `cl`, so handing it a `.S` would fail deep inside the
                        // compiler with an unhelpful message. Say so up front.
                        if ctx.toolchain().platform() == ToolchainPlatform::Msvc {
                            if let Some(asm) = sources.iter().find(|s| is_asm_extension(s)) {
                                bail!(
                                    "target '{}' has assembly source '{}', which MSVC cannot \
                                     assemble with `cl`\n\
                                     hint: MSVC needs a separate assembler (ml64.exe/armasm64.exe), \
                                     not yet supported; build this target with clang or gcc, or \
                                     exclude the assembly sources and use a C fallback",
                                    target.name,
                                    asm.display()
                                );
                            }
                        }

                        // A freestanding build is expressed in GCC-driver
                        // spellings (`-ffreestanding`, `-nostdlib`,
                        // `-Wl,-T,`). MSVC has its own vocabulary
                        // (`/NODEFAULTLIB`, `/ENTRY:`, and no linker-script
                        // concept at all), and none of it is wired, so say
                        // so here rather than handing `cl` flags it will
                        // reject a thousand lines later -- the same
                        // treatment assembly gets above.
                        if ctx.toolchain().platform() == ToolchainPlatform::Msvc {
                            let unsupported = [
                                ("freestanding", target.freestanding),
                                ("linker_script", target.linker_script.is_some()),
                                ("entry", target.entry.is_some()),
                            ]
                            .into_iter()
                            .filter(|(_, set)| *set)
                            .map(|(name, _)| name)
                            .collect::<Vec<_>>();
                            if !unsupported.is_empty() {
                                bail!(
                                    "target '{}' sets `{}`, which Harbour only implements \
                                     for GCC/Clang drivers\n\
                                     hint: these become `-ffreestanding`, `-nostdlib` and \
                                     `-Wl,-T,<script>`; MSVC's equivalents \
                                     (`/NODEFAULTLIB`, `/ENTRY:`) are not yet supported, so \
                                     build this target with clang or gcc",
                                    target.name,
                                    unsupported.join("`, `")
                                );
                            }
                        }

                        // Linking *for* an Apple platform means ld64, which has
                        // no linker-script concept and refuses a `-nostdlib`
                        // link outright ("dynamic executables or dylibs must
                        // link with libSystem.dylib"). Its own diagnostic for
                        // this is `ld: unknown options: --entry=_start -T`,
                        // which names the symptom and not the cause. A warning
                        // rather than an error, because `-fuse-ld=lld` in
                        // `ldflags` is a real way to make it work and rejecting
                        // the build would take that away.
                        if ctx.target.is_apple()
                            && (target.freestanding || target.linker_script.is_some())
                        {
                            tracing::warn!(
                                "`{}` target `{}` is freestanding but the link target is \
                                 Apple ({}). Apple's `ld` has no `-T` and cannot link \
                                 `-nostdlib`, so this link will fail with `unknown \
                                 options`. Build for a bare-metal triple \
                                 (`--target-triple x86_64-unknown-none`) with a cross \
                                 toolchain, or select an ELF-capable linker with \
                                 `-fuse-ld=lld` in this target's ldflags.",
                                pkg_id.name(),
                                target.name,
                                ctx.target.canonical()
                            );
                        }

                        // A linker script that is not there is a mistake, and
                        // one worth catching before the link: `ld` reports a
                        // missing script as "cannot open linker script file",
                        // naming a path resolved somewhere the manifest
                        // author never wrote. Resolution is against the
                        // *package* root -- see `Target::link_control_flags`
                        // -- so this also catches the case a relative path
                        // only works while the package is the build root.
                        //
                        // Placed after the pre-build generators above, which
                        // is load-bearing: a script templated with memory
                        // sizes is ordinary bare-metal practice, so a
                        // generator is allowed to be the thing that produces
                        // it. Checking before they ran would reject exactly
                        // that.
                        if let Some(script) = target.resolved_linker_script(package.root()) {
                            if !script.exists() {
                                bail!(
                                    "target '{}' names linker script `{}`, which does not \
                                     exist at `{}`\n\
                                     hint: `linker_script` resolves against this package's \
                                     root ({}), not the directory `harbour` was run from",
                                    target.name,
                                    target
                                        .linker_script
                                        .as_ref()
                                        .map(|p| p.display().to_string())
                                        .unwrap_or_default(),
                                    script.display(),
                                    package.root().display()
                                );
                            }
                        }

                        // `cl` has no `/std:` for C89, C99 or C23 and no GNU
                        // dialect at all, so a `c_std` it cannot express is
                        // dropped by `MsvcToolchain::compile_command`. Say so:
                        // a pinned standard that silently does nothing is the
                        // exact defect this field is being fixed for, and
                        // swapping a silent GCC-only behaviour for a silent
                        // MSVC-only one would not be a fix. Not an error,
                        // because `cl`'s default C mode already implements most
                        // of C99 and refusing the build would make a portable
                        // package unbuildable on Windows for declaring its
                        // standard honestly.
                        if ctx.toolchain().platform() == ToolchainPlatform::Msvc {
                            if let Some(c_std) = target.c_std {
                                if c_std.as_msvc_flag_value().is_none() {
                                    tracing::warn!(
                                        "`{}` target `{}` pins `c_std = \"{}\"`, which \
                                         `cl` has no `/std:` option for (it has only \
                                         `/std:c11` and `/std:c17`). These sources will \
                                         compile in cl's default C mode. Guard the \
                                         standard per compiler with a \
                                         `[[targets.{}.when]] compiler = ...` block if \
                                         the difference matters.",
                                        pkg_id.name(),
                                        target.name,
                                        c_std,
                                        target.name
                                    );
                                } else if c_std.gnu {
                                    tracing::warn!(
                                        "`{}` target `{}` pins `c_std = \"{}\"`, but `cl` \
                                         has no GNU dialect; compiling with `/std:{}` \
                                         instead. GNU extensions (`typeof`, statement \
                                         expressions, `asm`) will not be available.",
                                        pkg_id.name(),
                                        target.name,
                                        c_std,
                                        c_std.as_msvc_flag_value().unwrap_or_default()
                                    );
                                }
                            }
                        }

                        // Create compile steps
                        let mut object_files = Vec::new();
                        let obj_ext = ctx.toolchain().object_extension();

                        // Determine if target needs C++ compilation
                        let target_lang = target.lang;

                        for source in sources {
                            let source_for_lang = source.clone();
                            let rel_path = source.strip_prefix(package.root()).unwrap_or(&source);
                            let obj_name = rel_path.with_extension(obj_ext);
                            let output = obj_dir.join(obj_name);

                            object_files.push(output.clone());

                            let step = CompileStep {
                                source,
                                output,
                                package: pkg_id.name().to_string(),
                                target: target.name.to_string(),
                                include_dirs: compile_surface.include_dirs.clone(),
                                defines: compile_surface
                                    .defines
                                    .iter()
                                    .map(|d| d.to_flag())
                                    .collect(),
                                cflags: compile_surface.cflags.clone(),
                                lang: language_for_source(&source_for_lang, target_lang),
                                c_std: target.c_std,
                            };
                            steps.push(BuildStep::Compile(step.clone()));
                            compile_steps.push(step);
                        }

                        // Create link/archive step
                        if !object_files.is_empty() {
                            let output_dir = if target.kind == TargetKind::Exe {
                                &bin_dir
                            } else {
                                &lib_dir
                            };

                            let output = output_dir.join(target.output_filename(ctx.os()));

                            // The surface-derived half of this target's ABI
                            // identity, captured here because this is the last
                            // place a resolved surface exists. `native.rs`
                            // builds the `AbiIdentity` that keys the link
                            // fingerprint and sees only these flattened steps,
                            // so anything not carried on the step cannot reach
                            // the cache key -- which is how `surface.abi
                            // .toggles` came to affect nothing at all.
                            let abi = AbiSurfaceKey::from_surface(
                                &target
                                    .surface
                                    .resolve(&ctx.platform, &surface_resolver.features_for(pkg_id)),
                            );

                            if target.kind == TargetKind::StaticLib {
                                // Static library - use archive step (ar/lib.exe, never C++ driver)
                                steps.push(BuildStep::Archive(ArchiveStep {
                                    objects: object_files.clone(),
                                    output: output.clone(),
                                    package: pkg_id.name().to_string(),
                                    target: target.name.to_string(),
                                    abi: abi.clone(),
                                }));
                            }

                            // Determine if we need C++ linker driver
                            // For exe/sharedlib: use C++ driver if target is C++ or requires C++
                            let use_cxx_linker = match target.kind {
                                TargetKind::Exe | TargetKind::SharedLib => target.requires_cpp(),
                                TargetKind::StaticLib | TargetKind::HeaderOnly => false,
                            };

                            // `libs` carries both the resolved dependency
                            // archives (as literal file paths) and the
                            // declared system libraries (as `-lNAME`
                            // flags). Archives come first: `native.rs`
                            // recognizes file-path entries and appends them
                            // right after the object files, before `-L`/
                            // `-lNAME`/ldflags, which is the order a
                            // traditional static linker needs (dependent
                            // before dependency, real libraries before the
                            // system libraries that satisfy what's left).
                            // `link_surface.dep_libs` is already in that
                            // dependents-before-dependencies order -- see
                            // `SurfaceResolver::link_dep_order`.
                            let mut libs: Vec<String> = link_surface
                                .dep_libs
                                .iter()
                                .map(|p| p.display().to_string())
                                .collect();
                            libs.extend(link_surface.libs.iter().flat_map(|l| l.to_flags()));

                            let link_step = LinkStep {
                                objects: object_files,
                                output,
                                package: pkg_id.name().to_string(),
                                target: target.name.to_string(),
                                kind: format!("{:?}", target.kind).to_lowercase(),
                                lib_dirs: link_surface.lib_dirs.clone(),
                                libs,
                                ldflags: link_surface.ldflags.clone(),
                                frameworks: link_surface.frameworks.clone(),
                                use_cxx_linker,
                                abi,
                            };

                            if target.kind != TargetKind::StaticLib {
                                steps.push(BuildStep::Link(link_step.clone()));
                            }
                            link_steps.push(link_step);
                        }
                    }
                }
            }
        }

        Ok(BuildPlan {
            steps,
            compile_steps,
            link_steps,
            build_order,
        })
    }

    /// Emit compile_commands.json for IDE integration.
    pub fn emit_compile_commands(&self, ctx: &BuildContext, path: &Path) -> Result<()> {
        let commands: Vec<CompileCommand> = self
            .compile_steps
            .iter()
            .map(|step| {
                // The same call `NativeBuilder::compile` makes. This used to
                // build its own `CompileInput` and pass `cxx_opts: None`, so
                // every C++ flag -- `-std=`, `-fno-exceptions`, `-fno-rtti`,
                // `-stdlib=` -- was missing from the database while the
                // compiler received all of them.
                let spec = ctx.compile_spec(step)?;

                let mut args = Vec::with_capacity(spec.args.len() + 1);
                args.push(spec.program.display().to_string());
                args.extend(spec.args);

                Ok(CompileCommand {
                    directory: step
                        .source
                        .parent()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| ".".to_string()),
                    file: step.source.display().to_string(),
                    arguments: args,
                    output: Some(step.output.display().to_string()),
                })
            })
            .collect::<Result<_>>()?;

        let json = serde_json::to_string_pretty(&commands)?;
        std::fs::write(path, json)?;

        Ok(())
    }

    /// Get the number of compile steps.
    pub fn compile_count(&self) -> usize {
        self.compile_steps.len()
    }

    /// Get the number of link steps.
    pub fn link_count(&self) -> usize {
        self.link_steps.len()
    }
}

/// compile_commands.json entry.
#[derive(Debug, Serialize, Deserialize)]
struct CompileCommand {
    directory: String,
    file: String,
    arguments: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<String>,
}

/// The environment Harbour hands to every program it runs on a target's
/// behalf: a `[[targets.X.prebuild]]` generator and a `[targets.X.recipe]`
/// custom step alike.
///
/// **Why this is not a convenience.** A generator that is told nothing about
/// its target measures the machine it is running on, and that is wrong in
/// exactly the case where it matters most. openssl's x86_64 perlasm scripts
/// shell out to `$ENV{CC}` to ask the assembler which encodings it accepts;
/// with `CC` unset, `sha512-x86_64.pl` emits 49,912 bytes instead of 97,936
/// and drops the AVX2 and SHA-extension code paths entirely. That output
/// assembles, links, and computes correct digests -- slower, with no error,
/// no warning, and no change in object names or translation-unit counts.
/// Handing over the *target's* toolchain is what turns that from a guess in
/// each manifest (`env = { CC = "cc" }`) into a fact.
///
/// ## The contract
///
/// | variable | value |
/// |---|---|
/// | `HARBOUR_PACKAGE_ROOT` | the declaring package's root directory |
/// | `HARBOUR_ARTIFACT_DIR` | where Harbour expects this target's artifacts |
/// | `HARBOUR_TARGET_TRIPLE` | the triple being built *for* |
/// | `HARBOUR_TARGET_OS` | the same string a `when` block's `os` matches |
/// | `HARBOUR_TARGET_ARCH` | the same string a `when` block's `arch` matches |
/// | `HARBOUR_TARGET_ENV` | the same string a `when` block's `env` matches |
/// | `HARBOUR_HOST_TRIPLE` | the machine the generator is running on |
/// | `HARBOUR_CROSS_COMPILING` | `1` when those two differ, else `0` |
/// | `CC`, `CXX`, `AR` | the tools Harbour resolved for the target |
///
/// Three deliberate choices, because each has a plausible alternative:
///
/// 1. **`HARBOUR_TARGET_{OS,ARCH,ENV}` are the values `when` matches, not
///    the raw triple components.** A generator and the `when` block that
///    selected it must not be able to disagree about what platform this is:
///    `x86_64-apple-darwin` is `macos` to a condition, and would be `darwin`
///    to anything reading the triple itself. One producer
///    ([`TargetPlatform::for_target`]), two readers -- which is also why
///    the synonym normalisation that makes `arch = "aarch64"` match an
///    `arm64-*` triple lives in `for_target` rather than only inside
///    `PlatformCondition::matches`: otherwise a generator on
///    `arm64-apple-darwin` would be told `arm64` here and `aarch64` by the
///    block that selected it. `HARBOUR_TARGET_TRIPLE` stays verbatim on
///    purpose: it is what you hand back to a compiler, not what a condition
///    matched. `HARBOUR_TARGET_ENV` is absent rather than empty when the
///    triple has no environment component, because `gnu` and "no
///    environment at all" are different answers; `OS` is set-but-empty on a
///    bare-metal target, matching the empty string a condition sees there.
/// 2. **`CC` is the compiler *binary*, with no target flags appended.**
///    Multi-word `CC` is an autoconf convention and openssl would tolerate
///    it (it interpolates `$ENV{CC}` into a shell command), but a generator
///    that `exec`s it would not, and space-joining paths that may themselves
///    contain spaces is unparseable in general. The consequence is stated in
///    `MANIFEST.md` rather than hidden: for a toolchain that is the host
///    clang plus `-target`, `CC` alone generates *host* code, so a generator
///    doing target codegen must add `--target=$HARBOUR_TARGET_TRIPLE`.
///    `HARBOUR_CROSS_COMPILING` exists so it can know it has to.
/// 3. **No `HARBOUR_TARGET_POINTER_WIDTH`.** It is derivable from the arch
///    token and it would be a guess. `sizeof(long)` is a question probes
///    answer by compiling for the real target, and that answer reaches
///    generators too (see `HARBOUR_PROBE_*`); a second, weaker source for
///    the same fact is how two readers of one fact come to disagree.
///
/// ## Probe answers
///
/// Each answer from `[targets.NAME.probes]` arrives as
/// `HARBOUR_PROBE_<NAME>`, and when the target emits a generated header,
/// `HARBOUR_PROBE_HEADER` is its path. `probes` is `None` for a target that
/// declares none, which is different from declaring some and getting no
/// answers.
///
/// The manifest's own `env` is applied *after* this and therefore wins, as it
/// already did for `recipe`.
fn generator_env(
    ctx: &BuildContext,
    package_root: &Path,
    artifact_dir: &Path,
    probes: Option<&crate::builder::probe::ProbeResults>,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();

    env.insert(
        "HARBOUR_PACKAGE_ROOT".to_string(),
        package_root.display().to_string(),
    );
    env.insert(
        "HARBOUR_ARTIFACT_DIR".to_string(),
        artifact_dir.display().to_string(),
    );

    env.insert(
        "HARBOUR_TARGET_TRIPLE".to_string(),
        ctx.target.as_str().to_string(),
    );
    env.insert("HARBOUR_TARGET_OS".to_string(), ctx.platform.os.clone());
    env.insert("HARBOUR_TARGET_ARCH".to_string(), ctx.platform.arch.clone());
    if let Some(target_env) = &ctx.platform.env {
        env.insert("HARBOUR_TARGET_ENV".to_string(), target_env.clone());
    }

    let host = crate::core::target::TargetTriple::host();
    env.insert("HARBOUR_HOST_TRIPLE".to_string(), host.as_str().to_string());
    env.insert(
        "HARBOUR_CROSS_COMPILING".to_string(),
        if ctx.target.is_host() { "0" } else { "1" }.to_string(),
    );

    let toolchain = ctx.toolchain();
    env.insert(
        "CC".to_string(),
        toolchain.compiler_path().display().to_string(),
    );
    env.insert(
        "CXX".to_string(),
        toolchain.cxx_compiler_path().display().to_string(),
    );
    env.insert("AR".to_string(), archiver_program(toolchain));

    if let Some(results) = probes {
        env.extend(probe_env_vars(results));
    }

    env
}

/// A target's probe answers, as `HARBOUR_PROBE_<NAME>` variables.
///
/// This is the only thing between a probe and a generator, and it is a pure
/// mapping of the existing `ProbeResults`: probes are answered during
/// planning, generators run a few lines later in the same loop, so nothing
/// in the pipeline had to move. Probe names are already required to be valid
/// C identifiers (they become `-D`s), so they are valid environment variable
/// names with no mangling.
///
/// **A false boolean answer is `0`, not an absent variable** -- deliberately
/// unlike the define it emits, where `HAVE_X` is left undefined because C
/// code writes `#ifdef HAVE_X` and `#define HAVE_X 0` would satisfy it. An
/// environment has no `#ifdef`. If `Absent` set nothing, then a generator
/// reading a *misspelled* name would see exactly what it sees for a probe
/// that answered "no", and would silently take the "no" branch -- collapsing
/// "the answer is no" into "there is no answer", which is the single most
/// damaging mistake a configure system can make and the reason
/// `ProbeValue::Absent` exists as a distinct value at all. With `0`, a
/// generator that wants to be sure can test for presence:
///
/// ```sh
/// [ -n "${HARBOUR_PROBE_HAVE_X+set}" ] || exit 1   # not probed at all
/// ```
fn probe_env_vars(
    results: &crate::builder::probe::ProbeResults,
) -> impl Iterator<Item = (String, String)> + '_ {
    use crate::builder::probe::ProbeValue;

    let answers = results.answers.iter().map(|(name, value)| {
        let rendered = match value {
            ProbeValue::Present => "1".to_string(),
            ProbeValue::Absent => "0".to_string(),
            ProbeValue::Size(n) => n.to_string(),
        };
        (format!("HARBOUR_PROBE_{name}"), rendered)
    });

    // The generated header's path, for a generator that would rather parse
    // the file than read a dozen variables -- openssl's `configdata.pm`
    // rewrite is one variable, but curl's 793-line config header is the
    // shape where parsing wins. Only present when `emit = { header = ... }`.
    let header = results.header.as_ref().map(|path| {
        (
            "HARBOUR_PROBE_HEADER".to_string(),
            path.display().to_string(),
        )
    });

    answers.chain(header)
}

/// The archiver Harbour will actually run, asked of the toolchain rather
/// than guessed from the compiler's name.
///
/// There is no `Toolchain::archiver_path`, and this deliberately does not add
/// one: `archive_command` is the *only* producer of the archive program
/// today, so deriving `AR` from it cannot drift from the archive step, while
/// a second accessor could. The `ArchiveInput` is empty because only
/// `CommandSpec::program` is read -- both implementations (`ar rcs`,
/// `lib /OUT:`) put the tool there and the paths only in the arguments.
fn archiver_program(toolchain: &dyn crate::builder::toolchain::Toolchain) -> String {
    let spec = toolchain.archive_command(&crate::builder::toolchain::ArchiveInput {
        objects: Vec::new(),
        output: PathBuf::new(),
    });
    spec.program.display().to_string()
}

/// The advisory for a target whose `[[targets.X.when]]` blocks name a
/// *sibling* architecture spelling and none that matches this build.
///
/// `arch` in a condition is the literal first component of the triple, so
/// Debian's single armhf cross compiler is `arch = "arm"` reached through
/// `arm-unknown-linux-gnueabihf` and `arch = "armv7"` reached through
/// `armv7-unknown-linux-gnueabihf`. A manifest keyed on one silently does
/// not apply to the other, and an unmatched `when` block is normally
/// expected -- that is exactly how a portable baseline works -- so nothing
/// said anything. openssl caught it one step before it produced a 64-bit
/// `bn_conf.h` on a 32-bit target.
///
/// Deliberately narrow, because the false positive here is worse than the
/// miss: a warning on every package that merely has no block for the
/// current architecture would fire on the normal case and be tuned out.
/// It requires *all* of
///
/// 1. the target declares at least one `arch` condition;
/// 2. none of them names this architecture (compared canonically, so
///    `arm64` and `aarch64` do not trip it);
/// 3. at least one of them is in the **same ISA family** as this
///    architecture -- `armv7` for an `arm` build, `i686` for an `i386`
///    build. `aarch64` blocks on a 32-bit `arm` build are a different
///    family and stay silent, which is the openssl baseline case.
///
/// A warning rather than an error, and not a normalisation, because
/// `arch = "armv7"` legitimately means "ARMv7 and not ARMv4T": collapsing
/// generations would select NEON assembly for a machine that cannot execute
/// it, turning a silently-slower build into a silently-wrong one. See
/// [`canonical_arch`](crate::core::target::canonical_arch).
fn arch_spelling_advisory(
    target_arch: &str,
    target: &crate::core::target::Target,
    package: &str,
    target_name: &str,
) -> Option<String> {
    let declared: Vec<&str> = target
        .when
        .iter()
        .filter_map(|w| w.condition.arch.as_deref())
        .collect();
    if declared.is_empty() {
        return None;
    }
    let canonical = crate::core::target::canonical_arch(target_arch);
    if declared
        .iter()
        .any(|a| crate::core::target::canonical_arch(a) == canonical)
    {
        return None;
    }
    let siblings = crate::core::target::arch_spelling_siblings(target_arch, declared);
    if siblings.is_empty() {
        return None;
    }

    Some(format!(
        "`{package}` target `{target_name}` has no `[[targets.{target_name}.when]]` \
         block for `arch = \"{target_arch}\"`, but it has one for {} -- the same \
         architecture family spelled differently.\n\
         `arch` matches the first component of the target triple literally, so \
         those blocks contribute nothing to this build: their sources, defines \
         and generators are all absent. That is legitimate if this target really \
         is a different machine; if it is not, either name \
         `arch = \"{target_arch}\"` as well or build through the triple the \
         manifest was written for.",
        siblings
            .iter()
            .map(|s| format!("`arch = \"{s}\"`"))
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// Warn once per target built by a non-native recipe.
///
/// See `ARCHITECTURE.md` -> "Package Build Strategy": these targets forfeit
/// fingerprinting, surface flags and `compile_commands.json`. The cost is
/// invisible without this -- a CMake dependency simply appears to rebuild
/// every time for no stated reason.
fn warn_if_non_native(recipe: &Option<BuildRecipe>, package: &str, target: &str) {
    let backend = match recipe {
        Some(BuildRecipe::CMake { .. }) => "cmake",
        Some(BuildRecipe::Meson { .. }) => "meson",
        Some(BuildRecipe::Custom { .. }) => "a custom recipe",
        Some(BuildRecipe::Native) | None => return,
    };

    tracing::warn!(
        "`{package}` target `{target}` is built by {backend}, not natively: it \
         will be rebuilt in full on every build, receives no surface flags from \
         its dependents, and is absent from compile_commands.json. A native \
         target listing sources and defines avoids all three."
    );
}

/// Resolve a recipe's `source_dir` (from `BuildRecipe::CMake`/`Meson`)
/// against the *owning package's* root, not the process's working
/// directory.
///
/// `source_dir` is documented as package-relative, matching the `Custom`
/// recipe's `cwd` convention. When absent, the package root itself is used.
/// `Path::join` already treats an absolute `source_dir` as replacing the
/// base entirely, so this is correct for both relative and absolute values.
///
/// This anchoring matters once the package declaring the recipe is a
/// *dependency* rather than the root of the build: the process cwd during a
/// build is the root package's directory, so using `source_dir` verbatim
/// (the bug this fixes) would resolve it against the wrong package and
/// CMake/Meson would fail to find their build files anywhere but the root
/// build.
fn resolve_recipe_source_dir(source_dir: Option<&Path>, package_root: &Path) -> PathBuf {
    match source_dir {
        Some(dir) => package_root.join(dir),
        None => package_root.to_path_buf(),
    }
}

/// Check if a file path has a C++ source extension.
///
/// C++ extensions: .cpp, .cc, .cxx, .C (uppercase), .c++
/// Note: .C (uppercase) is C++ on case-sensitive systems (Linux, macOS).
fn is_cpp_extension(path: &Path) -> bool {
    let Some(ext) = path.extension() else {
        return false;
    };

    let ext_str = ext.to_string_lossy();

    // Check common C++ extensions (case-insensitive except for .C)
    matches!(
        ext_str.as_ref(),
        "cpp" | "cc" | "cxx" | "c++" | "CPP" | "CC" | "CXX"
    ) || ext_str == "C" // Uppercase .C is C++ on case-sensitive filesystems
}

/// Verify every package in the graph can build for the requested target.
///
/// Two mechanisms with deliberately different strictness, because C offers
/// guarantees at only one of these levels:
///
/// * `requires` is enforced. Freestanding versus hosted is standardised
///   (C §4) -- a freestanding implementation promises only `<float.h>`,
///   `<limits.h>`, `<stdarg.h>`, `<stddef.h>` and the C11 additions -- so a
///   package that needs libc on a bare-metal target is definitely broken, and
///   saying so beats a cascade of missing-header errors from a dependency
///   nobody was thinking about.
/// * `supports` only warns. Above that line nothing is guaranteed: glibc,
///   musl, MSVC and newlib disagree on POSIX coverage, threads and sockets, so
///   the list records what someone has built rather than what can build.
///   Rejecting an unlisted triple would mean rejecting working builds as
///   targets proliferate, and C's triple space is effectively unbounded.
fn check_target_requirements(ctx: &BuildContext, resolver: &SurfaceResolver) -> Result<()> {
    let triple = &ctx.target;
    let canonical = triple.canonical();

    let mut packages: Vec<_> = resolver.packages().values().collect();
    packages.sort_by_key(|p| p.name());

    for package in packages {
        let Some(meta) = package.manifest().package.as_ref() else {
            continue;
        };

        if let Some(requires) = meta.requires {
            if !requires.is_satisfied_by(triple) {
                bail!(
                    "package `{}` requires a {} environment, but the target \
                     `{}` is bare metal\n\
                     hint: it needs libc; either pick a hosted target or use a \
                     package that declares `requires = \"freestanding\"`",
                    package.name(),
                    requires.as_str(),
                    canonical
                );
            }
        }

        if !meta.supports.is_empty()
            && !meta
                .supports
                .iter()
                .any(|pat| crate::core::manifest::triple_matches_pattern(pat, &canonical))
        {
            tracing::warn!(
                "package `{}` does not list `{}` among the targets it supports ({}). \
                 The build will proceed -- the list records what has been built, not \
                 what can be -- but nothing has verified this combination.",
                package.name(),
                canonical,
                meta.supports.join(", ")
            );
        }
    }

    Ok(())
}

/// Whether `path` is an assembly source.
///
/// `.S` is preprocessed assembly (the compiler driver runs it through the
/// C preprocessor, so `-I` and `-D` apply); `.s` is raw assembly. Both are
/// handed to the C driver, which dispatches to the assembler by extension.
fn is_asm_extension(path: &Path) -> bool {
    let Some(ext) = path.extension() else {
        return false;
    };
    matches!(ext.to_string_lossy().as_ref(), "s" | "S" | "asm")
}

/// The language to compile a single source as.
///
/// The target's `lang` is only the default for sources whose extension is
/// ambiguous -- notably `.c`, which compiles as C++ in a `lang = "c++"`
/// target, preserving the previous whole-target behaviour. Assembly and
/// C++ extensions dispatch on the extension itself, so a target can mix
/// them: openssl and most codec libraries ship `.c` and `.S` side by side
/// in one library.
fn language_for_source(source: &Path, target_lang: Language) -> Language {
    if is_asm_extension(source) {
        Language::Asm
    } else if is_cpp_extension(source) {
        Language::Cxx
    } else {
        target_lang
    }
}

#[cfg(test)]
mod tests {

    /// Assembly and C++ dispatch on the extension so one target can mix
    /// them; `.c` follows the target's `lang`, which is what makes a
    /// `lang = "c++"` target still compile its `.c` files as C++.
    #[test]
    fn language_dispatches_per_source_file() {
        use std::path::Path;

        assert_eq!(
            language_for_source(Path::new("crypto/aesv8-armx.S"), Language::C),
            Language::Asm
        );
        assert_eq!(
            language_for_source(Path::new("crypto/x86_64cpuid.s"), Language::Cxx),
            Language::Asm,
            "assembly wins over the target language"
        );
        assert_eq!(
            language_for_source(Path::new("src/engine.cpp"), Language::C),
            Language::Cxx
        );
        assert_eq!(
            language_for_source(Path::new("src/util.c"), Language::Cxx),
            Language::Cxx,
            "an ambiguous .c follows the target language"
        );
        assert_eq!(
            language_for_source(Path::new("src/util.c"), Language::C),
            Language::C
        );
    }

    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_compile_command_serialization() {
        let cmd = CompileCommand {
            directory: "/home/user/project".to_string(),
            file: "src/main.c".to_string(),
            arguments: vec![
                "cc".to_string(),
                "-I/usr/include".to_string(),
                "-c".to_string(),
                "src/main.c".to_string(),
            ],
            output: Some("obj/main.o".to_string()),
        };

        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("directory"));
        assert!(json.contains("arguments"));
    }

    #[test]
    fn test_compile_command_without_output() {
        let cmd = CompileCommand {
            directory: "/project".to_string(),
            file: "src/lib.c".to_string(),
            arguments: vec!["gcc".to_string(), "-c".to_string()],
            output: None,
        };

        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("directory"));
        // output should be skipped when None due to skip_serializing_if
        assert!(!json.contains("output"));
    }

    #[test]
    fn test_is_cpp_extension() {
        // C++ extensions should return true
        assert!(is_cpp_extension(Path::new("file.cpp")));
        assert!(is_cpp_extension(Path::new("file.cc")));
        assert!(is_cpp_extension(Path::new("file.cxx")));
        assert!(is_cpp_extension(Path::new("file.c++")));
        assert!(is_cpp_extension(Path::new("file.C")));
        assert!(is_cpp_extension(Path::new("file.CPP")));
        assert!(is_cpp_extension(Path::new("file.CC")));
        assert!(is_cpp_extension(Path::new("file.CXX")));

        // C extensions should return false
        assert!(!is_cpp_extension(Path::new("file.c")));
        assert!(!is_cpp_extension(Path::new("file.h")));
        assert!(!is_cpp_extension(Path::new("file.hpp")));

        // No extension should return false
        assert!(!is_cpp_extension(Path::new("Makefile")));
        assert!(!is_cpp_extension(Path::new("file")));
    }

    #[test]
    fn test_compile_step_creation() {
        let step = CompileStep {
            source: PathBuf::from("/project/src/main.c"),
            output: PathBuf::from("/project/obj/main.o"),
            package: "mylib".to_string(),
            target: "mylib".to_string(),
            include_dirs: vec![
                PathBuf::from("/project/include"),
                PathBuf::from("/usr/include"),
            ],
            defines: vec!["-DDEBUG".to_string()],
            cflags: vec!["-Wall".to_string(), "-O2".to_string()],
            lang: Language::C,
            c_std: None,
        };

        assert_eq!(step.source, PathBuf::from("/project/src/main.c"));
        assert_eq!(step.package, "mylib");
        assert_eq!(step.include_dirs.len(), 2);
        assert_eq!(step.lang, Language::C);
    }

    #[test]
    fn test_compile_step_cpp() {
        let step = CompileStep {
            source: PathBuf::from("/project/src/main.cpp"),
            output: PathBuf::from("/project/obj/main.o"),
            package: "mylib".to_string(),
            target: "mylib".to_string(),
            include_dirs: vec![],
            defines: vec![],
            cflags: vec!["-std=c++17".to_string()],
            lang: Language::Cxx,
            c_std: None,
        };

        assert_eq!(step.lang, Language::Cxx);
        assert!(step.cflags.contains(&"-std=c++17".to_string()));
    }

    #[test]
    fn test_link_step_creation() {
        let step = LinkStep {
            objects: vec![
                PathBuf::from("/project/obj/main.o"),
                PathBuf::from("/project/obj/util.o"),
            ],
            output: PathBuf::from("/project/bin/myapp"),
            package: "myapp".to_string(),
            target: "myapp".to_string(),
            kind: "exe".to_string(),
            lib_dirs: vec![PathBuf::from("/usr/lib")],
            libs: vec!["-lm".to_string(), "-lpthread".to_string()],
            ldflags: vec!["-Wl,-rpath,/opt/lib".to_string()],
            frameworks: vec![],
            use_cxx_linker: false,
            abi: Default::default(),
        };

        assert_eq!(step.objects.len(), 2);
        assert_eq!(step.kind, "exe");
        assert!(!step.use_cxx_linker);
    }

    #[test]
    fn test_link_step_cxx_linker() {
        let step = LinkStep {
            objects: vec![PathBuf::from("/project/obj/main.o")],
            output: PathBuf::from("/project/bin/cppapp"),
            package: "cppapp".to_string(),
            target: "cppapp".to_string(),
            kind: "exe".to_string(),
            lib_dirs: vec![],
            libs: vec![],
            ldflags: vec![],
            frameworks: vec![],
            use_cxx_linker: true,
            abi: Default::default(),
        };

        assert!(step.use_cxx_linker);
    }

    #[test]
    fn test_archive_step_creation() {
        let step = ArchiveStep {
            objects: vec![
                PathBuf::from("/project/obj/a.o"),
                PathBuf::from("/project/obj/b.o"),
            ],
            output: PathBuf::from("/project/lib/libmylib.a"),
            package: "mylib".to_string(),
            target: "mylib".to_string(),
            abi: Default::default(),
        };

        assert_eq!(step.objects.len(), 2);
        assert_eq!(step.output, PathBuf::from("/project/lib/libmylib.a"));
    }

    #[test]
    fn test_resolve_recipe_source_dir_relative_anchors_to_package_root() {
        // Regression test: a dependency's relative `source_dir` must anchor
        // to *that dependency's* root, not to the process's working
        // directory (which, during a real build, is the root package's
        // directory). Before the fix, `Some(source_dir)` was used verbatim,
        // so e.g. a dependency at `/deps/cmakelib-1.0.0` with
        // `source_dir = "."` would resolve to whatever the current
        // directory happened to be instead of `/deps/cmakelib-1.0.0`.
        let dep_root = PathBuf::from("/deps/cmakelib-1.0.0");
        let resolved = resolve_recipe_source_dir(Some(Path::new(".")), &dep_root);
        assert_eq!(resolved, PathBuf::from("/deps/cmakelib-1.0.0/."));

        let resolved = resolve_recipe_source_dir(Some(Path::new("vendor/cmake")), &dep_root);
        assert_eq!(resolved, PathBuf::from("/deps/cmakelib-1.0.0/vendor/cmake"));
    }

    #[test]
    fn test_resolve_recipe_source_dir_absolute_is_used_as_is() {
        let dep_root = PathBuf::from("/deps/cmakelib-1.0.0");
        let resolved = resolve_recipe_source_dir(Some(Path::new("/other/absolute/dir")), &dep_root);
        assert_eq!(resolved, PathBuf::from("/other/absolute/dir"));
    }

    #[test]
    fn test_resolve_recipe_source_dir_none_defaults_to_package_root() {
        let dep_root = PathBuf::from("/deps/cmakelib-1.0.0");
        let resolved = resolve_recipe_source_dir(None, &dep_root);
        assert_eq!(resolved, dep_root);
    }

    #[test]
    fn test_cmake_step_creation() {
        let step = CMakeStep {
            source_dir: PathBuf::from("/project"),
            build_dir: PathBuf::from("/project/build"),
            args: vec![
                "-DCMAKE_BUILD_TYPE=Release".to_string(),
                "-DBUILD_SHARED_LIBS=ON".to_string(),
            ],
            targets: vec!["mylib".to_string()],
            package: "mylib".to_string(),
            target: "mylib".to_string(),
        };

        assert_eq!(step.args.len(), 2);
        assert_eq!(step.targets.len(), 1);
    }

    #[test]
    fn test_meson_step_creation() {
        let step = MesonStep {
            source_dir: PathBuf::from("/project"),
            build_dir: PathBuf::from("/project/builddir"),
            options: vec![
                "-Ddefault_library=static".to_string(),
                "-Dbuildtype=release".to_string(),
            ],
            targets: vec!["mylib".to_string()],
            package: "mylib".to_string(),
            target: "mylib".to_string(),
        };

        assert_eq!(step.options.len(), 2);
        assert_eq!(step.targets.len(), 1);
        assert_eq!(step.build_dir, PathBuf::from("/project/builddir"));
    }

    #[test]
    fn test_custom_step_creation() {
        let mut env = BTreeMap::new();
        env.insert("CC".to_string(), "gcc".to_string());

        let step = CustomStep {
            program: "make".to_string(),
            args: vec!["-j4".to_string(), "all".to_string()],
            cwd: PathBuf::from("/project"),
            env,
            outputs: vec![PathBuf::from("/project/lib/libcustom.a")],
            artifact_dir: PathBuf::new(),
            package_root: PathBuf::new(),
            package: "custom".to_string(),
            target: "custom".to_string(),
        };

        assert_eq!(step.program, "make");
        assert_eq!(step.args.len(), 2);
        assert!(step.env.contains_key("CC"));
    }

    #[test]
    fn test_build_step_enum_variants() {
        let compile = BuildStep::Compile(CompileStep {
            source: PathBuf::from("src/main.c"),
            output: PathBuf::from("obj/main.o"),
            package: "test".to_string(),
            target: "test".to_string(),
            include_dirs: vec![],
            defines: vec![],
            cflags: vec![],
            lang: Language::C,
            c_std: None,
        });

        let archive = BuildStep::Archive(ArchiveStep {
            objects: vec![],
            output: PathBuf::from("lib/libtest.a"),
            package: "test".to_string(),
            target: "test".to_string(),
            abi: Default::default(),
        });

        let link = BuildStep::Link(LinkStep {
            objects: vec![],
            output: PathBuf::from("bin/test"),
            package: "test".to_string(),
            target: "test".to_string(),
            kind: "exe".to_string(),
            lib_dirs: vec![],
            libs: vec![],
            ldflags: vec![],
            frameworks: vec![],
            use_cxx_linker: false,
            abi: Default::default(),
        });

        // Verify they can be matched
        assert!(matches!(compile, BuildStep::Compile(_)));
        assert!(matches!(archive, BuildStep::Archive(_)));
        assert!(matches!(link, BuildStep::Link(_)));
    }

    #[test]
    fn test_build_plan_counts() {
        let plan = BuildPlan {
            steps: vec![
                BuildStep::Compile(CompileStep {
                    source: PathBuf::from("a.c"),
                    output: PathBuf::from("a.o"),
                    package: "test".to_string(),
                    target: "test".to_string(),
                    include_dirs: vec![],
                    defines: vec![],
                    cflags: vec![],
                    lang: Language::C,
                    c_std: None,
                }),
                BuildStep::Compile(CompileStep {
                    source: PathBuf::from("b.c"),
                    output: PathBuf::from("b.o"),
                    package: "test".to_string(),
                    target: "test".to_string(),
                    include_dirs: vec![],
                    defines: vec![],
                    cflags: vec![],
                    lang: Language::C,
                    c_std: None,
                }),
            ],
            compile_steps: vec![
                CompileStep {
                    source: PathBuf::from("a.c"),
                    output: PathBuf::from("a.o"),
                    package: "test".to_string(),
                    target: "test".to_string(),
                    include_dirs: vec![],
                    defines: vec![],
                    cflags: vec![],
                    lang: Language::C,
                    c_std: None,
                },
                CompileStep {
                    source: PathBuf::from("b.c"),
                    output: PathBuf::from("b.o"),
                    package: "test".to_string(),
                    target: "test".to_string(),
                    include_dirs: vec![],
                    defines: vec![],
                    cflags: vec![],
                    lang: Language::C,
                    c_std: None,
                },
            ],
            link_steps: vec![LinkStep {
                objects: vec![],
                output: PathBuf::from("test"),
                package: "test".to_string(),
                target: "test".to_string(),
                kind: "exe".to_string(),
                lib_dirs: vec![],
                libs: vec![],
                ldflags: vec![],
                frameworks: vec![],
                use_cxx_linker: false,
                abi: Default::default(),
            }],
            build_order: vec!["test 1.0.0".to_string()],
        };

        assert_eq!(plan.compile_count(), 2);
        assert_eq!(plan.link_count(), 1);
        assert_eq!(plan.build_order.len(), 1);
    }

    #[test]
    fn test_build_plan_serialization() {
        let plan = BuildPlan {
            steps: vec![],
            compile_steps: vec![],
            link_steps: vec![],
            build_order: vec!["pkg-a 1.0.0".to_string(), "pkg-b 2.0.0".to_string()],
        };

        let json = serde_json::to_string(&plan).unwrap();
        let deserialized: BuildPlan = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.build_order.len(), 2);
        assert_eq!(deserialized.build_order[0], "pkg-a 1.0.0");
    }
}
