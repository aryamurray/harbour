//! Target definitions - what gets built.
//!
//! A Target represents a buildable artifact: executable, static library,
//! shared library, or header-only library.

mod core;
mod ffi;
mod language;

// Re-export all public types to maintain API compatibility
pub use self::core::{
    BuildRecipe, CustomCommand, Target, TargetDepSpec, TargetKind, Visibility,
};
pub use self::ffi::{
    FfiBundler, FfiBundlerParseError, FfiConfig, FfiLanguage, FfiLanguageParseError,
};
pub use self::language::{
    CStandard, CStandardParseError, CppStandard, CppStandardParseError, Language,
};
