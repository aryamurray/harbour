//! FFI binding generation types.
//!
//! This module contains types for configuring FFI binding generation
//! for foreign language interop.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// FFI binding generation configuration.
///
/// Specifies how to generate language bindings for FFI consumption.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FfiConfig {
    /// Target languages for binding generation (e.g., "typescript", "python")
    #[serde(default)]
    pub languages: Vec<FfiLanguage>,

    /// FFI runtime/bundler to use (e.g., "koffi", "ffi-napi")
    #[serde(default)]
    pub bundler: Option<FfiBundler>,

    /// Output directory for generated bindings (relative to package root)
    #[serde(default)]
    pub output_dir: Option<PathBuf>,

    /// Header files to parse for binding generation (globs)
    /// If not specified, uses public_headers from the target
    #[serde(default)]
    pub header_files: Vec<String>,

    /// Functions to include in bindings (if empty, include all)
    #[serde(default)]
    pub include_functions: Vec<String>,

    /// Functions to exclude from bindings
    #[serde(default)]
    pub exclude_functions: Vec<String>,

    /// Types to include in bindings (if empty, include all)
    #[serde(default)]
    pub include_types: Vec<String>,

    /// Types to exclude from bindings
    #[serde(default)]
    pub exclude_types: Vec<String>,

    /// Prefix to strip from function names (e.g., "mylib_")
    #[serde(default)]
    pub strip_prefix: Option<String>,

    /// Generate async wrappers for functions
    #[serde(default)]
    pub async_wrappers: bool,
}

/// Supported FFI target languages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FfiLanguage {
    /// TypeScript/JavaScript bindings
    #[serde(alias = "ts", alias = "js", alias = "javascript")]
    TypeScript,

    /// Python bindings
    #[serde(alias = "py")]
    Python,

    /// C# bindings
    #[serde(alias = "cs", alias = "dotnet")]
    CSharp,

    /// Rust bindings
    #[serde(alias = "rs")]
    Rust,
}

impl std::fmt::Display for FfiLanguage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FfiLanguage::TypeScript => write!(f, "typescript"),
            FfiLanguage::Python => write!(f, "python"),
            FfiLanguage::CSharp => write!(f, "csharp"),
            FfiLanguage::Rust => write!(f, "rust"),
        }
    }
}

impl std::str::FromStr for FfiLanguage {
    type Err = FfiLanguageParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "typescript" | "ts" | "js" | "javascript" => Ok(FfiLanguage::TypeScript),
            "python" | "py" => Ok(FfiLanguage::Python),
            "csharp" | "cs" | "c#" | "dotnet" => Ok(FfiLanguage::CSharp),
            "rust" | "rs" => Ok(FfiLanguage::Rust),
            _ => Err(FfiLanguageParseError(s.to_string())),
        }
    }
}

/// Error returned when parsing an invalid FFI language string.
#[derive(Debug, Clone)]
pub struct FfiLanguageParseError(pub String);

impl std::fmt::Display for FfiLanguageParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "invalid FFI language '{}', valid values: typescript, python, csharp, rust",
            self.0
        )
    }
}

impl std::error::Error for FfiLanguageParseError {}

/// Supported FFI bundlers/runtimes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FfiBundler {
    /// koffi - Node.js FFI library
    Koffi,

    /// ffi-napi - Node.js native addon FFI
    #[serde(alias = "ffi-napi")]
    FfiNapi,

    /// ctypes - Python ctypes
    #[serde(alias = "ctypes")]
    Ctypes,

    /// cffi - Python cffi
    Cffi,

    /// P/Invoke - .NET P/Invoke
    #[serde(alias = "pinvoke")]
    PInvoke,
}

impl std::fmt::Display for FfiBundler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FfiBundler::Koffi => write!(f, "koffi"),
            FfiBundler::FfiNapi => write!(f, "ffi-napi"),
            FfiBundler::Ctypes => write!(f, "ctypes"),
            FfiBundler::Cffi => write!(f, "cffi"),
            FfiBundler::PInvoke => write!(f, "pinvoke"),
        }
    }
}

impl std::str::FromStr for FfiBundler {
    type Err = FfiBundlerParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "koffi" => Ok(FfiBundler::Koffi),
            "ffi-napi" | "ffinapi" => Ok(FfiBundler::FfiNapi),
            "ctypes" => Ok(FfiBundler::Ctypes),
            "cffi" => Ok(FfiBundler::Cffi),
            "pinvoke" | "p/invoke" => Ok(FfiBundler::PInvoke),
            _ => Err(FfiBundlerParseError(s.to_string())),
        }
    }
}

/// Error returned when parsing an invalid FFI bundler string.
#[derive(Debug, Clone)]
pub struct FfiBundlerParseError(pub String);

impl std::fmt::Display for FfiBundlerParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "invalid FFI bundler '{}', valid values: koffi, ffi-napi, ctypes, cffi, pinvoke",
            self.0
        )
    }
}

impl std::error::Error for FfiBundlerParseError {}
