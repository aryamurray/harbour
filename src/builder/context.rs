//! Build context - compiler, target, and profile configuration.

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

use crate::core::abi::{CompilerIdentity, TargetTriple};
use crate::core::manifest::Profile;
use crate::core::surface::TargetPlatform;
use crate::core::Workspace;
use crate::util::process::{find_ar, find_c_compiler, ProcessBuilder};

/// Build context containing compiler and target information.
#[derive(Debug, Clone)]
pub struct BuildContext {
    /// C compiler path
    pub cc: PathBuf,

    /// Archiver path
    pub ar: PathBuf,

    /// Target triple
    pub target: TargetTriple,

    /// Compiler identity
    pub compiler: CompilerIdentity,

    /// Target platform for surface condition evaluation
    pub platform: TargetPlatform,

    /// Build profile
    pub profile: Profile,

    /// Profile name
    pub profile_name: String,

    /// Output directory
    pub output_dir: PathBuf,

    /// Dependencies output directory
    pub deps_dir: PathBuf,

    /// Workspace root
    pub workspace_root: PathBuf,
}

impl BuildContext {
    /// Create a new build context from a workspace.
    pub fn new(ws: &Workspace, profile_name: &str) -> Result<Self> {
        // Find C compiler
        let cc = find_c_compiler().ok_or_else(|| {
            anyhow::anyhow!(
                "no C compiler found\n\
                 \n\
                 Harbour requires a C compiler (gcc, clang, or cl).\n\
                 Set the CC environment variable or install a compiler."
            )
        })?;

        // Find archiver
        let ar = find_ar().ok_or_else(|| {
            anyhow::anyhow!(
                "no archiver found\n\
                 \n\
                 Harbour requires an archiver (ar or lib).\n\
                 Set the AR environment variable or install an archiver."
            )
        })?;

        // Detect compiler identity
        let compiler = detect_compiler_identity(&cc)?;

        // Detect target triple
        let target = TargetTriple::host();

        // Create platform for surface conditions
        let platform = TargetPlatform::host().with_compiler(&compiler.family);

        // Get profile
        let profile = if profile_name == "release" {
            ws.manifest().release_profile()
        } else {
            ws.manifest().debug_profile()
        };

        let output_dir = ws.output_dir();
        let deps_dir = ws.deps_dir();

        Ok(BuildContext {
            cc,
            ar,
            target,
            compiler,
            platform,
            profile,
            profile_name: profile_name.to_string(),
            output_dir,
            deps_dir,
            workspace_root: ws.root().to_path_buf(),
        })
    }

    /// Get compiler flags from profile.
    pub fn profile_cflags(&self) -> Vec<String> {
        let mut flags = Vec::new();

        // Optimization level
        if let Some(ref opt) = self.profile.opt_level {
            flags.push(format!("-O{}", opt));
        }

        // Debug info
        if let Some(ref debug) = self.profile.debug {
            if debug != "0" {
                flags.push("-g".to_string());
                if debug == "full" || debug == "2" {
                    flags.push("-g3".to_string());
                }
            }
        }

        // Sanitizers
        for sanitizer in &self.profile.sanitizers {
            flags.push(format!("-fsanitize={}", sanitizer));
        }

        // Custom flags
        flags.extend(self.profile.cflags.iter().cloned());

        flags
    }

    /// Get linker flags from profile.
    pub fn profile_ldflags(&self) -> Vec<String> {
        let mut flags = Vec::new();

        // LTO
        if self.profile.lto == Some(true) {
            flags.push("-flto".to_string());
        }

        // Sanitizers (need to be passed to linker too)
        for sanitizer in &self.profile.sanitizers {
            flags.push(format!("-fsanitize={}", sanitizer));
        }

        // Custom flags
        flags.extend(self.profile.ldflags.iter().cloned());

        flags
    }

    /// Check if this is a release build.
    pub fn is_release(&self) -> bool {
        self.profile_name == "release"
    }

    /// Get the OS name.
    pub fn os(&self) -> &str {
        &self.target.os
    }

    /// Create a compiler process builder.
    pub fn compiler(&self) -> ProcessBuilder {
        ProcessBuilder::new(&self.cc)
    }

    /// Create an archiver process builder.
    pub fn archiver(&self) -> ProcessBuilder {
        ProcessBuilder::new(&self.ar)
    }
}

/// Detect the compiler identity from the compiler path.
fn detect_compiler_identity(cc: &Path) -> Result<CompilerIdentity> {
    let cc_name = cc
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("cc")
        .to_lowercase();

    let family = if cc_name.contains("clang") {
        "clang"
    } else if cc_name.contains("gcc") || cc_name.contains("g++") {
        "gcc"
    } else if cc_name.contains("cl") {
        "msvc"
    } else {
        // Try to detect from --version output
        let output = ProcessBuilder::new(cc).arg("--version").exec();

        if let Ok(output) = output {
            let stdout = String::from_utf8_lossy(&output.stdout).to_lowercase();
            if stdout.contains("clang") {
                "clang"
            } else if stdout.contains("gcc") {
                "gcc"
            } else {
                "unknown"
            }
        } else {
            "unknown"
        }
    };

    // Try to get version
    let version = get_compiler_version(cc, family).unwrap_or_else(|| "unknown".to_string());

    Ok(CompilerIdentity::new(family, &version))
}

/// Get the compiler version.
fn get_compiler_version(cc: &Path, family: &str) -> Option<String> {
    let output = if family == "msvc" {
        ProcessBuilder::new(cc).exec().ok()?
    } else {
        ProcessBuilder::new(cc).arg("--version").exec().ok()?
    };

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Parse version from output
    // This is a simplified version parser
    for line in stdout.lines() {
        // Look for version numbers like "14.0.0" or "13.2.1"
        for word in line.split_whitespace() {
            if word.chars().next()?.is_ascii_digit() {
                let parts: Vec<&str> = word.split('.').collect();
                if parts.len() >= 2 {
                    return Some(format!("{}.{}", parts[0], parts[1]));
                }
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_profile_cflags() {
        let profile = Profile {
            opt_level: Some("2".to_string()),
            debug: Some("1".to_string()),
            sanitizers: vec!["address".to_string()],
            ..Default::default()
        };

        let ctx = BuildContext {
            cc: PathBuf::from("gcc"),
            ar: PathBuf::from("ar"),
            target: TargetTriple::host(),
            compiler: CompilerIdentity::new("gcc", "13.0"),
            platform: TargetPlatform::host(),
            profile,
            profile_name: "debug".to_string(),
            output_dir: PathBuf::from("target"),
            deps_dir: PathBuf::from("target/deps"),
            workspace_root: PathBuf::from("."),
        };

        let flags = ctx.profile_cflags();
        assert!(flags.contains(&"-O2".to_string()));
        assert!(flags.contains(&"-g".to_string()));
        assert!(flags.contains(&"-fsanitize=address".to_string()));
    }
}
