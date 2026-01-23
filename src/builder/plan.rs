//! Build plan generation.
//!
//! A BuildPlan describes all compilation and linking steps needed to build
//! a workspace.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::builder::context::BuildContext;
use crate::builder::surface_resolver::{EffectiveCompileSurface, EffectiveLinkSurface, SurfaceResolver};
use crate::core::target::TargetKind;
use crate::core::PackageId;
use crate::resolver::Resolve;
use crate::sources::SourceCache;
use crate::util::fs::glob_files;

/// A complete build plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildPlan {
    /// Compilation steps
    pub compile_steps: Vec<CompileStep>,

    /// Link steps
    pub link_steps: Vec<LinkStep>,

    /// Build order (package IDs in topological order)
    pub build_order: Vec<String>,
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
}

impl BuildPlan {
    /// Create a new build plan from a resolve and build context.
    pub fn new(
        ctx: &BuildContext,
        resolve: &Resolve,
        source_cache: &mut SourceCache,
    ) -> Result<Self> {
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

        // Process each package in build order
        for pkg_id in resolve.topological_order() {
            let package = surface_resolver
                .get_package(pkg_id)
                .ok_or_else(|| anyhow::anyhow!("package not loaded: {}", pkg_id))?;

            for target in package.targets() {
                // Resolve surfaces
                let compile_surface = surface_resolver.resolve_compile_surface(pkg_id, target)?;
                let link_surface =
                    surface_resolver.resolve_link_surface(pkg_id, target, &ctx.deps_dir)?;

                // Determine output directory
                let target_output_dir = if pkg_id == resolve.topological_order().last().copied().unwrap() {
                    // Root package goes to output_dir
                    ctx.output_dir.clone()
                } else {
                    // Dependencies go to deps_dir
                    ctx.deps_dir
                        .join(format!("{}-{}", pkg_id.name(), pkg_id.version()))
                };

                let obj_dir = target_output_dir.join("obj").join(target.name.as_str());
                let lib_dir = target_output_dir.join("lib");
                let bin_dir = target_output_dir.join("bin");

                // Find source files
                let sources = glob_files(package.root(), &target.sources)?;

                // Create compile steps
                let mut object_files = Vec::new();
                for source in sources {
                    let rel_path = source
                        .strip_prefix(package.root())
                        .unwrap_or(&source);
                    let obj_name = rel_path.with_extension("o");
                    let output = obj_dir.join(obj_name);

                    object_files.push(output.clone());

                    compile_steps.push(CompileStep {
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
                    });
                }

                // Create link step
                if !object_files.is_empty() {
                    let output_dir = if target.kind == TargetKind::Exe {
                        &bin_dir
                    } else {
                        &lib_dir
                    };

                    let output = output_dir.join(target.output_filename(ctx.os()));

                    link_steps.push(LinkStep {
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
                    });
                }
            }
        }

        Ok(BuildPlan {
            compile_steps,
            link_steps,
            build_order,
        })
    }

    /// Emit compile_commands.json for IDE integration.
    pub fn emit_compile_commands(&self, path: &Path) -> Result<()> {
        let commands: Vec<CompileCommand> = self
            .compile_steps
            .iter()
            .map(|step| {
                let mut args = vec!["cc".to_string()];
                for dir in &step.include_dirs {
                    args.push(format!("-I{}", dir.display()));
                }
                args.extend(step.defines.iter().cloned());
                args.extend(step.cflags.iter().cloned());
                args.push("-c".to_string());
                args.push(step.source.display().to_string());
                args.push("-o".to_string());
                args.push(step.output.display().to_string());

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
