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

impl FfiConfig {
    /// Reject the fields of this table that nothing reads.
    ///
    /// `header_files` is the only one with a consumer: `harbour ffi
    /// generate` uses it when `--header` was not passed. Everything else
    /// parsed and was discarded, and `deny_unknown_fields` on the struct
    /// made the whole table look honoured. Verified by running, with every
    /// field set: `--lang` was still *required* despite
    /// `languages = ["python"]`; `output_dir = "manifest-bindings"` was
    /// ignored in favour of the hardcoded `<root>/bindings`; `bundler` came
    /// from a per-language default rather than the manifest; and
    /// `include_functions`/`exclude_functions`/`include_types`/
    /// `exclude_types` have no CLI equivalent at all, so those four were the
    /// only way to express filtering and did nothing.
    ///
    /// Each rejection names the flag to pass instead, so a manifest can be
    /// converted into a working command rather than merely refused.
    ///
    /// tracking: <https://github.com/aryamurray/harbour/issues/109>
    pub fn validate_implemented(&self, target: &str) -> anyhow::Result<()> {
        let mut unread: Vec<(&str, String)> = Vec::new();
        if !self.languages.is_empty() {
            unread.push(("languages", "pass `--lang <LANG>`".to_string()));
        }
        if self.bundler.is_some() {
            unread.push(("bundler", "pass `--bundler <BUNDLER>`".to_string()));
        }
        if self.output_dir.is_some() {
            unread.push(("output_dir", "pass `--output <DIR>`".to_string()));
        }
        if self.strip_prefix.is_some() {
            unread.push(("strip_prefix", "pass `--strip-prefix <PREFIX>`".to_string()));
        }
        if self.async_wrappers {
            unread.push(("async_wrappers", "pass `--async-wrappers`".to_string()));
        }
        for (name, values) in [
            ("include_functions", &self.include_functions),
            ("exclude_functions", &self.exclude_functions),
            ("include_types", &self.include_types),
            ("exclude_types", &self.exclude_types),
        ] {
            if !values.is_empty() {
                // No flag exists for these four. Saying so is the point:
                // "use the flag instead" would be a second lie.
                unread.push((
                    name,
                    "no equivalent flag: binding filtering is not implemented at all".to_string(),
                ));
            }
        }

        if unread.is_empty() {
            return Ok(());
        }

        let listed = unread
            .iter()
            .map(|(name, advice)| format!("  {name}: {advice}"))
            .collect::<Vec<_>>()
            .join("\n");
        anyhow::bail!(
            "target `{target}`: `[targets.{target}.ffi]` key(s) not implemented: {}\n\
             hint: `harbour ffi generate` reads only `header_files` from this \
             table; everything else comes from the command line, so these keys \
             parsed and were discarded:\n{listed}\n\
             tracking: https://github.com/aryamurray/harbour/issues/109",
            unread
                .iter()
                .map(|(name, _)| *name)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
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
