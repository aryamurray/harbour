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
use crate::core::target::{BuildRecipe, Language, TargetKind};
use crate::resolver::Resolve;
use crate::sources::SourceCache;
use crate::util::fs::glob_files;

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
    /// Run a custom command
    Custom(CustomStep),
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
            let targets_to_build: Vec<_> = if is_root && target_filter.is_some() {
                let filter = target_filter.unwrap();
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

                // Handle recipe dispatch
                match &target.recipe {
                    Some(BuildRecipe::CMake {
                        source_dir,
                        args,
                        targets: cmake_targets,
                    }) => {
                        // CMake recipe - generate CMake step
                        let src_dir = source_dir
                            .clone()
                            .unwrap_or_else(|| package.root().to_path_buf());
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
                        let compile_surface =
                            surface_resolver.resolve_compile_surface(pkg_id, target)?;
                        let link_surface =
                            surface_resolver.resolve_link_surface(pkg_id, target, &ctx.deps_dir)?;

                        // Find source files
                        let sources = glob_files(package.root(), &target.sources)?;

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

                        // Create compile steps
                        let mut object_files = Vec::new();
                        let obj_ext = ctx.toolchain().object_extension();

                        // Determine if target needs C++ compilation
                        let target_lang = target.lang;

                        for source in sources {
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
                                lang: target_lang,
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

                            let link_step = LinkStep {
                                objects: object_files,
                                output,
                                package: pkg_id.name().to_string(),
                                target: target.name.to_string(),
                                kind: format!("{:?}", target.kind).to_lowercase(),
                                lib_dirs: link_surface.lib_dirs.clone(),
                                libs: link_surface
                                    .libs
                                    .iter()
                                    .flat_map(|l| l.to_flags())
                                    .collect(),
                                ldflags: link_surface.ldflags.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
