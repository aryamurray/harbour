//! Target definitions - what gets built.
//!
//! A Target represents a buildable artifact: executable, static library,
//! or shared library.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::core::surface::Surface;
use crate::util::InternedString;

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
        }
    }

    /// Get the typical file prefix for this target kind.
    pub fn prefix(&self, os: &str) -> &'static str {
        match self {
            TargetKind::Exe => "",
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

    /// Check if this is a library (static or shared).
    pub fn is_library(&self) -> bool {
        matches!(self, TargetKind::StaticLib | TargetKind::SharedLib)
    }

    /// Check if this produces a linkable artifact.
    pub fn is_linkable(&self) -> bool {
        matches!(self, TargetKind::StaticLib | TargetKind::SharedLib)
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

    /// Target-specific dependencies
    #[serde(default)]
    pub deps: Vec<TargetDep>,

    /// Build recipe override
    #[serde(default)]
    pub recipe: Option<BuildRecipe>,
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
            deps: Vec::new(),
            recipe: None,
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

/// A dependency of a target with visibility settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetDep {
    /// Package name
    pub package: String,

    /// Target name within the package (defaults to package name)
    #[serde(default)]
    pub target: Option<String>,

    /// Compile surface visibility
    #[serde(default = "default_visibility")]
    pub compile: Visibility,

    /// Link surface visibility
    #[serde(default = "default_visibility")]
    pub link: Visibility,
}

fn default_visibility() -> Visibility {
    Visibility::Public
}

impl TargetDep {
    /// Create a new target dependency.
    pub fn new(package: impl Into<String>) -> Self {
        TargetDep {
            package: package.into(),
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

    /// Get the effective target name.
    pub fn target_name(&self) -> &str {
        self.target.as_deref().unwrap_or(&self.package)
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

    /// Custom build command
    Custom {
        /// Commands to run
        commands: Vec<String>,

        /// Output artifacts
        outputs: Vec<PathBuf>,
    },
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
        assert_eq!(
            TargetKind::Exe.output_filename("myapp", "linux"),
            "myapp"
        );
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
