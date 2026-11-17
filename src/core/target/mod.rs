//! Target definitions - what gets built.
//!
//! A Target represents a buildable artifact: executable, static library,
//! shared library, header-only library, or a prebuilt library that is linked
//! as it is.

mod core;
mod language;
mod triple;
mod warnings;

// Re-export all public types to maintain API compatibility
pub use self::core::{
    is_shared_library_name, order_targets_by_local_deps, validate_host_tools, BuildRecipe,
    ConditionalSources, CustomCommand, PrebuiltFile, ResolvedPrebuilt, Target, TargetDepSpec,
    TargetKind, Visibility,
};
pub use self::language::{
    CStandard, CStandardParseError, CStandardSpec, CppStandard, CppStandardParseError, Language,
};
pub use self::triple::{arch_spelling_siblings, canonical_arch, TargetTriple};
pub use self::warnings::{
    capped_warnings_note, CappedWarnings, DepWarnings, WarningLevel, WarningLevelParseError,
    WarningOptions,
};
