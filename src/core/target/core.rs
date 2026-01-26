//! Core target types.
//!
//! This module contains the main Target struct and related types
//! for defining build targets.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::core::surface::Surface;
use crate::util::InternedString;

use super::ffi::FfiConfig;
use super::language::{CppStandard, CStandard, Language};

/// The kind of target being built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TargetKind {
    /// Executable binary
    #[serde(alias = "bin")]
    Exe,

    /// Static library (.a / .lib)
    #[serde(alias = "lib", alias = "static")]
    StaticLib,

    /// Shared/dynamic library (.so / .dylib / .dll)
    #[serde(alias = "dylib", alias = "dynamic")]
    SharedLib,

    /// Header-only library (no compile/link steps)
    #[serde(alias = "header-only", alias = "interface")]
    HeaderOnly,
}

impl Default for TargetKind {
    fn default() -> Self {
        TargetKind::Exe
    }
}

impl TargetKind {
    /// Get the typical file extension for this target kind.
    pub fn extension(&self, os: &str) -> &'static str {
        match self {
            TargetKind::Exe => {
                if os == "windows" {
                    "exe"
                } else {
                    ""
                }
            }
            TargetKind::StaticLib => {
                if os == "windows" {
                    "lib"
                } else {
                    "a"
                }
            }
            TargetKind::SharedLib => match os {
                "windows" => "dll",
                "macos" => "dylib",
                _ => "so",
            },
            TargetKind::HeaderOnly => "",
        }
    }

    /// Get the typical file prefix for this target kind.
    pub fn prefix(&self, os: &str) -> &'static str {
        match self {
            TargetKind::Exe | TargetKind::HeaderOnly => "",
            TargetKind::StaticLib | TargetKind::SharedLib => {
                if os == "windows" {
                    ""
                } else {
                    "lib"
                }
            }
        }
    }

    /// Get the output filename for a target.
    pub fn output_filename(&self, name: &str, os: &str) -> String {
        let prefix = self.prefix(os);
        let ext = self.extension(os);
        if ext.is_empty() {
            format!("{}{}", prefix, name)
        } else {
            format!("{}{}.{}", prefix, name, ext)
        }
    }

    /// Check if this is a library (static, shared, or header-only).
    pub fn is_library(&self) -> bool {
        matches!(
            self,
            TargetKind::StaticLib | TargetKind::SharedLib | TargetKind::HeaderOnly
        )
    }

    /// Check if this produces a linkable artifact.
    pub fn is_linkable(&self) -> bool {
        matches!(self, TargetKind::StaticLib | TargetKind::SharedLib)
    }

    /// Check if this is a header-only library.
    pub fn is_header_only(&self) -> bool {
        matches!(self, TargetKind::HeaderOnly)
    }
}

/// A build target with its configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Target {
    /// Target name (usually same as package name for single-target packages)
    pub name: InternedString,

    /// What kind of artifact to produce
    #[serde(default)]
    pub kind: TargetKind,

    /// Source file patterns (globs)
    #[serde(default)]
    pub sources: Vec<String>,

    /// Public header patterns (for libraries)
    #[serde(default)]
    pub public_headers: Vec<String>,

    /// Surface contract (compile/link requirements)
    #[serde(default)]
    pub surface: Surface,

    /// Target-specific dependencies (keyed by package name for O(1) lookup)
    #[serde(default)]
    pub deps: HashMap<InternedString, TargetDepSpec>,

    /// Build recipe override
    #[serde(default)]
    pub recipe: Option<BuildRecipe>,

    /// Source language (C or C++)
    #[serde(default)]
    pub lang: Language,

    /// C standard version (only meaningful when lang = C)
    #[serde(default)]
    pub c_std: Option<CStandard>,

    /// C++ standard version (only meaningful when lang = C++)
    #[serde(default)]
    pub cpp_std: Option<CppStandard>,

    /// Backend-specific configuration
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<crate::core::manifest::BackendConfig>,

    /// FFI binding generation configuration
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ffi: Option<FfiConfig>,
}

impl Target {
    /// Create a new target with the given name and kind.
    pub fn new(name: impl Into<InternedString>, kind: TargetKind) -> Self {
        Target {
            name: name.into(),
            kind,
            sources: Vec::new(),
            public_headers: Vec::new(),
            surface: Surface::default(),
            deps: HashMap::new(),
            recipe: None,
            lang: Language::default(),
            c_std: None,
            cpp_std: None,
            backend: None,
            ffi: None,
        }
    }

    /// Create a new executable target.
    pub fn exe(name: impl Into<InternedString>) -> Self {
        Self::new(name, TargetKind::Exe)
    }

    /// Create a new static library target.
    pub fn staticlib(name: impl Into<InternedString>) -> Self {
        Self::new(name, TargetKind::StaticLib)
    }

    /// Create a new shared library target.
    pub fn sharedlib(name: impl Into<InternedString>) -> Self {
        Self::new(name, TargetKind::SharedLib)
    }

    /// Create a new header-only library target.
    pub fn headeronly(name: impl Into<InternedString>) -> Self {
        Self::new(name, TargetKind::HeaderOnly)
    }

    /// Validate target configuration.
    ///
    /// Checks for:
    /// - Header-only targets must not have sources or recipes
    /// - C++ source extensions must match lang=c++ setting
    pub fn validate(&self) -> Result<()> {
        // Header-only validation
        if self.kind == TargetKind::HeaderOnly {
            if !self.sources.is_empty() {
                bail!(
                    "header-only target '{}' must not have sources\n\
                     hint: remove the sources field or change kind to staticlib/sharedlib",
                    self.name
                );
            }
            if self.recipe.is_some() {
                bail!(
                    "header-only target '{}' must not have a recipe\n\
                     hint: remove the recipe field",
                    self.name
                );
            }
        }

        // Source extension validation: C++ extensions require lang=c++
        if self.lang == Language::C {
            let cpp_extensions = [".cc", ".cpp", ".cxx", ".C", ".c++"];
            for pattern in &self.sources {
                if cpp_extensions.iter().any(|ext| pattern.ends_with(ext)) {
                    bail!(
                        "target '{}' has lang=c but sources match C++ extensions\n\
                         hint: set lang = 'c++' in [targets.{}]",
                        self.name,
                        self.name
                    );
                }
            }
        }

        Ok(())
    }

    /// Check if this target requires C++ compilation or linking.
    pub fn requires_cpp(&self) -> bool {
        self.lang == Language::Cxx || self.cpp_std.is_some()
    }

    /// Add source patterns.
    pub fn with_sources(mut self, patterns: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.sources = patterns.into_iter().map(|p| p.into()).collect();
        self
    }

    /// Add public header patterns.
    pub fn with_public_headers(
        mut self,
        patterns: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.public_headers = patterns.into_iter().map(|p| p.into()).collect();
        self
    }

    /// Get the output filename for this target.
    pub fn output_filename(&self, os: &str) -> String {
        self.kind.output_filename(&self.name, os)
    }
}

/// Specification for a target-level dependency with visibility settings.
///
/// Target-level deps allow fine-grained control over which surfaces
/// propagate from dependencies. When specified, they override the
/// package-level dependency list for surface resolution.
///
/// The package name is stored as the key in `Target.deps` HashMap,
/// enabling O(1) lookup by dependency name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetDepSpec {
    /// Target name within the package (defaults to package name)
    #[serde(default)]
    pub target: Option<String>,

    /// Compile surface visibility - controls whether the dependency's
    /// public compile surface propagates to this target
    #[serde(default = "default_visibility")]
    pub compile: Visibility,

    /// Link surface visibility - controls whether the dependency's
    /// public link surface propagates to this target
    #[serde(default = "default_visibility")]
    pub link: Visibility,
}

fn default_visibility() -> Visibility {
    Visibility::Public
}

impl TargetDepSpec {
    /// Create a new target dependency spec with default (public) visibility.
    pub fn new() -> Self {
        TargetDepSpec {
            target: None,
            compile: Visibility::Public,
            link: Visibility::Public,
        }
    }

    /// Set the target name.
    pub fn with_target(mut self, target: impl Into<String>) -> Self {
        self.target = Some(target.into());
        self
    }

    /// Set compile visibility.
    pub fn with_compile(mut self, visibility: Visibility) -> Self {
        self.compile = visibility;
        self
    }

    /// Set link visibility.
    pub fn with_link(mut self, visibility: Visibility) -> Self {
        self.link = visibility;
        self
    }

    /// Get the effective target name given the package name.
    pub fn target_name<'a>(&'a self, package_name: &'a str) -> &'a str {
        self.target.as_deref().unwrap_or(package_name)
    }
}

impl Default for TargetDepSpec {
    fn default() -> Self {
        Self::new()
    }
}

/// Visibility of a dependency's surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    /// Propagates to dependents
    Public,
    /// Internal only
    Private,
}

impl Default for Visibility {
    fn default() -> Self {
        Visibility::Public
    }
}

/// Build recipe for a target.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum BuildRecipe {
    /// Use the native Harbour builder
    Native,

    /// Use CMake
    CMake {
        /// CMakeLists.txt directory (defaults to package root)
        #[serde(default)]
        source_dir: Option<PathBuf>,

        /// Additional CMake arguments
        #[serde(default)]
        args: Vec<String>,

        /// CMake targets to build
        #[serde(default)]
        targets: Vec<String>,
    },

    /// Use Meson
    Meson {
        /// meson.build directory (defaults to package root)
        #[serde(default)]
        source_dir: Option<PathBuf>,

        /// Additional Meson options (-D flags)
        #[serde(default)]
        options: Vec<String>,

        /// Meson targets to build
        #[serde(default)]
        targets: Vec<String>,
    },

    /// Custom build steps (structured, not shell commands)
    Custom {
        /// Steps to execute
        steps: Vec<CustomCommand>,
    },
}

/// A structured custom command (not a shell string).
///
/// This is safer than shell strings because:
/// - No shell injection vulnerabilities
/// - Cross-platform (no shell-specific syntax)
/// - Easier to analyze for caching/fingerprinting
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomCommand {
    /// Program to execute
    pub program: String,

    /// Arguments to pass
    #[serde(default)]
    pub args: Vec<String>,

    /// Working directory (relative to package root)
    #[serde(default)]
    pub cwd: Option<PathBuf>,

    /// Environment variables to set for this command
    #[serde(default)]
    pub env: BTreeMap<String, String>,

    /// Output files this command produces (for fingerprinting)
    #[serde(default)]
    pub outputs: Vec<PathBuf>,

    /// Input files this command depends on (for fingerprinting)
    #[serde(default)]
    pub inputs: Vec<PathBuf>,
}

impl CustomCommand {
    /// Create a new custom command.
    pub fn new(program: impl Into<String>) -> Self {
        CustomCommand {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            env: BTreeMap::new(),
            outputs: Vec::new(),
            inputs: Vec::new(),
        }
    }

    /// Add an argument.
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Add multiple arguments.
    pub fn args(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.args.extend(args.into_iter().map(|a| a.into()));
        self
    }

    /// Set the working directory.
    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// Add an output file.
    pub fn output(mut self, output: impl Into<PathBuf>) -> Self {
        self.outputs.push(output.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_target_kind_extensions() {
        assert_eq!(TargetKind::Exe.extension("linux"), "");
        assert_eq!(TargetKind::Exe.extension("windows"), "exe");
        assert_eq!(TargetKind::StaticLib.extension("linux"), "a");
        assert_eq!(TargetKind::StaticLib.extension("windows"), "lib");
        assert_eq!(TargetKind::SharedLib.extension("linux"), "so");
        assert_eq!(TargetKind::SharedLib.extension("macos"), "dylib");
        assert_eq!(TargetKind::SharedLib.extension("windows"), "dll");
    }

    #[test]
    fn test_output_filename() {
        assert_eq!(TargetKind::Exe.output_filename("myapp", "linux"), "myapp");
        assert_eq!(
            TargetKind::Exe.output_filename("myapp", "windows"),
            "myapp.exe"
        );
        assert_eq!(
            TargetKind::StaticLib.output_filename("mylib", "linux"),
            "libmylib.a"
        );
        assert_eq!(
            TargetKind::SharedLib.output_filename("mylib", "macos"),
            "libmylib.dylib"
        );
    }

    #[test]
    fn test_target_builder() {
        let target = Target::staticlib("mylib")
            .with_sources(["src/**/*.c"])
            .with_public_headers(["include/**/*.h"]);

        assert_eq!(target.name.as_str(), "mylib");
        assert_eq!(target.kind, TargetKind::StaticLib);
        assert_eq!(target.sources, vec!["src/**/*.c"]);
        assert_eq!(target.public_headers, vec!["include/**/*.h"]);
    }
}
