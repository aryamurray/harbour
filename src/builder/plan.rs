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
use crate::builder::util::parse_define_flags;
use crate::core::target::{BuildRecipe, Language, TargetKind};
use crate::resolver::Resolve;
use crate::sources::SourceCache;
use crate::util::fs::glob_files_excluding;

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
    /// Package this belongs to
    pub package: String,
    /// Target name
    pub target: String,
}

/// A pre-build step: runs before source/header fingerprinting and native
/// compilation, typically to materialize a generated file (e.g. a
/// `configure`-produced header) that source files `#include`.
///
/// Structurally identical to [`CustomStep`] -- kept as a distinct type (and
/// distinct [`BuildStep`] variant) so that `NativeBuilder::execute` can find
/// and run all of these *before* it does any header scanning, regardless of
/// where they fall in the step list, without having to reinterpret
/// `CustomStep`'s meaning based on context.
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

                            steps.push(BuildStep::Custom(CustomStep {
                                program: cmd.program.clone(),
                                args: cmd.args.clone(),
                                cwd,
                                env: cmd.env.clone(),
                                outputs: cmd
                                    .outputs
                                    .iter()
                                    .map(|o| package.root().join(o))
                                    .collect(),
                                package: pkg_id.name().to_string(),
                                target: target.name.to_string(),
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

                        if let Some(vcpkg) = ctx.vcpkg.as_ref() {
                            compile_surface
                                .include_dirs
                                .extend(vcpkg.include_dirs.iter().cloned());
                            compile_surface.include_dirs.sort();
                            compile_surface.include_dirs.dedup();

                            link_surface.lib_dirs.extend(vcpkg.lib_dirs.iter().cloned());
                            link_surface.lib_dirs.sort();
                            link_surface.lib_dirs.dedup();
                        }

                        // Pre-build steps (e.g. codegen that materializes a
                        // generated header) must run, and be recorded in the
                        // plan, before source globbing/fingerprinting: a
                        // generated file that a source `#include`s has to
                        // exist before anything scans for it. `NativeBuilder`
                        // enforces the "before fingerprinting" half by
                        // running every `Prebuild` step up front, prior to
                        // its header-scanning decision phase (see its
                        // module-level note); this only has to get the step
                        // itself into the plan.
                        for cmd in &target.prebuild {
                            let cwd = cmd
                                .cwd
                                .clone()
                                .map(|c| package.root().join(c))
                                .unwrap_or_else(|| package.root().to_path_buf());

                            steps.push(BuildStep::Prebuild(PrebuildStep {
                                program: cmd.program.clone(),
                                args: cmd.args.clone(),
                                cwd,
                                env: cmd.env.clone(),
                                outputs: cmd
                                    .outputs
                                    .iter()
                                    .map(|o| package.root().join(o))
                                    .collect(),
                                package: pkg_id.name().to_string(),
                                target: target.name.to_string(),
                            }));
                        }

                        // Find source files. `resolved_sources` folds in any
                        // `[[targets.X.when]]` entries whose condition
                        // matches the platform we're building *for* (never
                        // the host -- see `TargetPlatform::for_target`), so
                        // cross-compiling selects the right source set (e.g.
                        // arm/*.c only when the target triple is actually
                        // aarch64).
                        let pkg_features = surface_resolver.features_for(pkg_id);
                        let (target_sources, target_exclude) =
                            target.resolved_sources(&ctx.platform, &pkg_features);
                        let sources =
                            glob_files_excluding(package.root(), &target_sources, &target_exclude)?;

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

                            if target.kind == TargetKind::StaticLib {
                                // Static library - use archive step (ar/lib.exe, never C++ driver)
                                steps.push(BuildStep::Archive(ArchiveStep {
                                    objects: object_files.clone(),
                                    output: output.clone(),
                                    package: pkg_id.name().to_string(),
                                    target: target.name.to_string(),
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
                let mut cflags = ctx.profile_cflags();
                cflags.extend(step.cflags.iter().cloned());

                let input = crate::builder::toolchain::CompileInput {
                    source: step.source.clone(),
                    output: step.output.clone(),
                    include_dirs: step.include_dirs.clone(),
                    defines: parse_define_flags(&step.defines),
                    cflags,
                };

                let spec = ctx.toolchain().compile_command(&input, step.lang, None);

                let mut args = Vec::with_capacity(spec.args.len() + 1);
                args.push(spec.program.display().to_string());
                args.extend(spec.args);

                CompileCommand {
                    directory: step
                        .source
                        .parent()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| ".".to_string()),
                    file: step.source.display().to_string(),
                    arguments: args,
                    output: Some(step.output.display().to_string()),
                }
            })
            .collect();

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
        });

        let archive = BuildStep::Archive(ArchiveStep {
            objects: vec![],
            output: PathBuf::from("lib/libtest.a"),
            package: "test".to_string(),
            target: "test".to_string(),
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
