//! Target definitions - what gets built.
//!
//! A Target represents a buildable artifact: executable, static library,
//! shared library, or header-only library.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::core::surface::Surface;
use crate::util::InternedString;

/// Source language for a target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    /// C language (default)
    #[default]
    C,
    /// C++ language
    #[serde(alias = "cpp", alias = "cxx", alias = "c++")]
    Cxx,
}

impl Language {
    /// Get the language name as a string.
    pub fn as_str(&self) -> &'static str {
        match self {
            Language::C => "c",
            Language::Cxx => "c++",
        }
    }
}

/// C++ standard version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum CppStandard {
    /// C++11
    #[serde(rename = "11", alias = "c++11", alias = "cpp11")]
    Cpp11,
    /// C++14
    #[serde(rename = "14", alias = "c++14", alias = "cpp14")]
    Cpp14,
    /// C++17
    #[serde(rename = "17", alias = "c++17", alias = "cpp17")]
    Cpp17,
    /// C++20
    #[serde(rename = "20", alias = "c++20", alias = "cpp20")]
    Cpp20,
    /// C++23
    #[serde(rename = "23", alias = "c++23", alias = "cpp23")]
    Cpp23,
}

impl CppStandard {
    /// Get the standard as a compiler flag value (e.g., "c++17").
    pub fn as_flag_value(&self) -> &'static str {
        match self {
            CppStandard::Cpp11 => "c++11",
            CppStandard::Cpp14 => "c++14",
            CppStandard::Cpp17 => "c++17",
            CppStandard::Cpp20 => "c++20",
            CppStandard::Cpp23 => "c++23",
        }
    }

    /// Get the MSVC-style standard flag value (e.g., "c++17", "c++latest" for C++23).
    pub fn as_msvc_flag_value(&self) -> &'static str {
        match self {
            CppStandard::Cpp11 => "c++14", // MSVC doesn't support c++11 flag, use 14
            CppStandard::Cpp14 => "c++14",
            CppStandard::Cpp17 => "c++17",
            CppStandard::Cpp20 => "c++20",
            CppStandard::Cpp23 => "c++latest",
        }
    }
}

impl std::str::FromStr for CppStandard {
    type Err = CppStandardParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "11" | "c++11" | "cpp11" => Ok(CppStandard::Cpp11),
            "14" | "c++14" | "cpp14" => Ok(CppStandard::Cpp14),
            "17" | "c++17" | "cpp17" => Ok(CppStandard::Cpp17),
            "20" | "c++20" | "cpp20" => Ok(CppStandard::Cpp20),
            "23" | "c++23" | "cpp23" => Ok(CppStandard::Cpp23),
            _ => Err(CppStandardParseError(s.to_string())),
        }
    }
}

/// Error returned when parsing an invalid C++ standard string.
#[derive(Debug, Clone)]
pub struct CppStandardParseError(pub String);

impl std::fmt::Display for CppStandardParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "invalid C++ standard '{}', valid values: 11, 14, 17, 20, 23",
            self.0
        )
    }
}

impl std::error::Error for CppStandardParseError {}

impl std::fmt::Display for CppStandard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "C++{}", match self {
            CppStandard::Cpp11 => "11",
            CppStandard::Cpp14 => "14",
            CppStandard::Cpp17 => "17",
            CppStandard::Cpp20 => "20",
            CppStandard::Cpp23 => "23",
        })
    }
}

/// C standard version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum CStandard {
    /// C89 (also known as C90, ANSI C)
    #[serde(rename = "89", alias = "c89", alias = "C89", alias = "90", alias = "c90", alias = "C90")]
    C89,
    /// C99
    #[serde(rename = "99", alias = "c99", alias = "C99")]
    C99,
    /// C11
    #[serde(rename = "11", alias = "c11", alias = "C11")]
    C11,
    /// C17 (also known as C18)
    #[serde(rename = "17", alias = "c17", alias = "C17", alias = "18", alias = "c18", alias = "C18")]
    C17,
    /// C23
    #[serde(rename = "23", alias = "c23", alias = "C23")]
    C23,
}

impl CStandard {
    /// Get the standard as a compiler flag value (e.g., "c11").
    pub fn as_flag_value(&self) -> &'static str {
        match self {
            CStandard::C89 => "c89",
            CStandard::C99 => "c99",
            CStandard::C11 => "c11",
            CStandard::C17 => "c17",
            CStandard::C23 => "c23",
        }
    }

    /// Get the GNU-extension variant (e.g., "gnu11").
    pub fn as_gnu_flag_value(&self) -> &'static str {
        match self {
            CStandard::C89 => "gnu89",
            CStandard::C99 => "gnu99",
            CStandard::C11 => "gnu11",
            CStandard::C17 => "gnu17",
            CStandard::C23 => "gnu23",
        }
    }
}

impl std::str::FromStr for CStandard {
    type Err = CStandardParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "89" | "c89" | "C89" | "90" | "c90" | "C90" => Ok(CStandard::C89),
            "99" | "c99" | "C99" => Ok(CStandard::C99),
            "11" | "c11" | "C11" => Ok(CStandard::C11),
            "17" | "c17" | "C17" | "18" | "c18" | "C18" => Ok(CStandard::C17),
            "23" | "c23" | "C23" => Ok(CStandard::C23),
            _ => Err(CStandardParseError(s.to_string())),
        }
    }
}

/// Error returned when parsing an invalid C standard string.
#[derive(Debug, Clone)]
pub struct CStandardParseError(pub String);

impl std::fmt::Display for CStandardParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "invalid C standard '{}', valid values: 89, 99, 11, 17, 23",
            self.0
        )
    }
}

impl std::error::Error for CStandardParseError {}

impl std::fmt::Display for CStandard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "C{}", match self {
            CStandard::C89 => "89",
            CStandard::C99 => "99",
            CStandard::C11 => "11",
            CStandard::C17 => "17",
            CStandard::C23 => "23",
        })
    }
}

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
                        self.name, self.name
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
