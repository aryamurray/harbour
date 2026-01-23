//! CMake adapter for existing CMake projects.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::builder::context::BuildContext;
use crate::ops::harbour_build::Artifact;
use crate::util::fs::ensure_dir;
use crate::util::process::{find_cmake, ProcessBuilder};

/// CMake build adapter.
pub struct CMakeBuilder<'a> {
    ctx: &'a BuildContext,
    source_dir: PathBuf,
    build_dir: PathBuf,
    cmake_args: Vec<String>,
    targets: Vec<String>,
}

impl<'a> CMakeBuilder<'a> {
    /// Create a new CMake builder.
    pub fn new(ctx: &'a BuildContext, source_dir: PathBuf) -> Result<Self> {
        // Ensure CMake is available
        if find_cmake().is_none() {
            bail!(
                "CMake not found\n\
                 \n\
                 CMake is required to build CMake-based dependencies.\n\
                 Install CMake and ensure it's in your PATH."
            );
        }

        let build_dir = ctx.output_dir.join("cmake-build");

        Ok(CMakeBuilder {
            ctx,
            source_dir,
            build_dir,
            cmake_args: Vec::new(),
            targets: Vec::new(),
        })
    }

    /// Add CMake arguments.
    pub fn args(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.cmake_args.extend(args.into_iter().map(|s| s.into()));
        self
    }

    /// Specify targets to build.
    pub fn targets(mut self, targets: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.targets.extend(targets.into_iter().map(|s| s.into()));
        self
    }

    /// Set the build directory.
    pub fn build_dir(mut self, dir: PathBuf) -> Self {
        self.build_dir = dir;
        self
    }

    /// Configure and build.
    pub fn build(&self) -> Result<Vec<Artifact>> {
        // Ensure build directory exists
        ensure_dir(&self.build_dir)?;

        // Configure
        self.configure()?;

        // Build
        self.compile()?;

        // Find built artifacts
        self.find_artifacts()
    }

    /// Run CMake configuration.
    fn configure(&self) -> Result<()> {
        tracing::info!("Configuring CMake project");

        let cmake = find_cmake().unwrap();
        let mut cmd = ProcessBuilder::new(cmake);

        // Source directory
        cmd = cmd.arg("-S").arg(&self.source_dir);

        // Build directory
        cmd = cmd.arg("-B").arg(&self.build_dir);

        // Build type
        let build_type = if self.ctx.is_release() {
            "Release"
        } else {
            "Debug"
        };
        cmd = cmd.arg(format!("-DCMAKE_BUILD_TYPE={}", build_type));

        // Install prefix (output to our deps dir)
        cmd = cmd.arg(format!(
            "-DCMAKE_INSTALL_PREFIX={}",
            self.ctx.deps_dir.display()
        ));

        // Position independent code
        cmd = cmd.arg("-DCMAKE_POSITION_INDEPENDENT_CODE=ON");

        // Custom args
        for arg in &self.cmake_args {
            cmd = cmd.arg(arg);
        }

        let output = cmd.exec()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("CMake configuration failed:\n{}", stderr);
        }

        Ok(())
    }

    /// Run CMake build.
    fn compile(&self) -> Result<()> {
        tracing::info!("Building CMake project");

        let cmake = find_cmake().unwrap();
        let mut cmd = ProcessBuilder::new(cmake);

        cmd = cmd.arg("--build").arg(&self.build_dir);

        // Parallel build
        cmd = cmd.arg("--parallel");

        // Configuration (for multi-config generators like Visual Studio)
        let config = if self.ctx.is_release() {
            "Release"
        } else {
            "Debug"
        };
        cmd = cmd.arg("--config").arg(config);

        // Specific targets
        if !self.targets.is_empty() {
            cmd = cmd.arg("--target");
            for target in &self.targets {
                cmd = cmd.arg(target);
            }
        }

        let output = cmd.exec()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("CMake build failed:\n{}", stderr);
        }

        Ok(())
    }

    /// Find built artifacts.
    fn find_artifacts(&self) -> Result<Vec<Artifact>> {
        let mut artifacts = Vec::new();

        // Look for libraries in common locations
        let search_dirs = [
            self.build_dir.clone(),
            self.build_dir.join("lib"),
            self.build_dir.join("Release"),
            self.build_dir.join("Debug"),
        ];

        let lib_extensions = if self.ctx.os() == "windows" {
            vec!["lib", "dll"]
        } else if self.ctx.os() == "macos" {
            vec!["a", "dylib"]
        } else {
            vec!["a", "so"]
        };

        for dir in &search_dirs {
            if dir.exists() {
                for entry in std::fs::read_dir(dir)? {
                    let entry = entry?;
                    let path = entry.path();

                    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                        if lib_extensions.contains(&ext) {
                            let name = path
                                .file_stem()
                                .and_then(|n| n.to_str())
                                .unwrap_or("unknown")
                                .to_string();

                            artifacts.push(Artifact {
                                path,
                                target: name,
                            });
                        }
                    }
                }
            }
        }

        Ok(artifacts)
    }
}

/// Check if a directory contains a CMake project.
pub fn is_cmake_project(dir: &Path) -> bool {
    dir.join("CMakeLists.txt").exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_cmake_project() {
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();

        // Not a CMake project initially
        assert!(!is_cmake_project(tmp.path()));

        // Create CMakeLists.txt
        std::fs::write(tmp.path().join("CMakeLists.txt"), "cmake_minimum_required(VERSION 3.10)").unwrap();

        // Now it's a CMake project
        assert!(is_cmake_project(tmp.path()));
    }
}
