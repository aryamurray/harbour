//! ABI identity computation.
//!
//! Every built artifact has an ABI identity that serves as a cache key.
//! This ensures we detect when dependencies need rebuilding due to
//! incompatible ABI changes.

use crate::core::surface::{Define, ResolvedSurface};
use crate::core::target::{TargetKind, TargetTriple};
use crate::util::hash::Fingerprint;

/// Compiler identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompilerIdentity {
    /// Compiler family (gcc, clang, msvc)
    pub family: String,
    /// Compiler version
    pub version: String,
}

impl CompilerIdentity {
    pub fn new(family: &str, version: &str) -> Self {
        CompilerIdentity {
            family: family.to_string(),
            version: version.to_string(),
        }
    }
}

impl std::fmt::Display for CompilerIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}-{}", self.family, self.version)
    }
}

/// Complete ABI identity for a built artifact.
#[derive(Debug, Clone)]
pub struct AbiIdentity {
    /// Target triple
    pub target: TargetTriple,
    /// Compiler identity
    pub compiler: CompilerIdentity,
    /// Target kind (staticlib, sharedlib, exe)
    pub kind: TargetKind,
    /// Position-independent code
    pub pic: bool,
    /// Symbol visibility (default/hidden)
    pub visibility: String,
    /// Public defines that affect ABI
    pub public_defines: Vec<String>,
    /// ABI toggles from surface
    pub toggles: Vec<String>,
}

impl AbiIdentity {
    /// Create a new ABI identity.
    pub fn new(target: TargetTriple, compiler: CompilerIdentity, kind: TargetKind) -> Self {
        AbiIdentity {
            target,
            compiler,
            kind,
            pic: true, // Default to PIC for libraries
            visibility: "default".to_string(),
            public_defines: Vec::new(),
            toggles: Vec::new(),
        }
    }

    /// Set PIC mode.
    pub fn with_pic(mut self, pic: bool) -> Self {
        self.pic = pic;
        self
    }

    /// Set visibility.
    pub fn with_visibility(mut self, visibility: impl Into<String>) -> Self {
        self.visibility = visibility.into();
        self
    }

    /// Add public defines from a resolved surface.
    pub fn with_surface(mut self, surface: &ResolvedSurface) -> Self {
        // Extract define names that affect ABI
        self.public_defines = surface
            .compile_public
            .defines
            .iter()
            .map(|d| match d {
                Define::Flag(name) => name.clone(),
                Define::KeyValue { name, value } => format!("{}={}", name, value),
            })
            .collect();

        self.toggles = surface.abi.toggles.clone();
        self
    }

    /// Compute the fingerprint (cache key).
    pub fn fingerprint(&self) -> String {
        let mut fp = Fingerprint::new();

        // Canonical, not raw, so equivalent spellings of one target (e.g.
        // `arm-none-eabi` vs `arm-unknown-none-eabi`) hash identically.
        fp.update_str(&self.target.canonical())
            .update_str(&self.compiler.to_string())
            .update_str(&format!("{:?}", self.kind))
            .update_bool(self.pic)
            .update_str(&self.visibility);

        // Sort defines for determinism
        let mut defines = self.public_defines.clone();
        defines.sort();
        for define in &defines {
            fp.update_str(define);
        }

        // Sort toggles for determinism
        let mut toggles = self.toggles.clone();
        toggles.sort();
        for toggle in &toggles {
            fp.update_str(toggle);
        }

        fp.finish_short()
    }

    /// Check if two ABI identities are compatible.
    pub fn is_compatible(&self, other: &AbiIdentity) -> bool {
        // Must match exactly for now
        // In the future, we could have more nuanced compatibility rules
        //
        // Canonical comparison, not `==`, so equivalent spellings of one
        // target aren't treated as an ABI mismatch.
        self.target.canonical() == other.target.canonical()
            && self.compiler.family == other.compiler.family
            && self.kind == other.kind
            && self.pic == other.pic
            && self.public_defines == other.public_defines
    }
}

/// Check if a rebuild is needed based on ABI identity.
pub fn needs_rebuild(current: &AbiIdentity, cached: &AbiIdentity) -> Option<String> {
    // Canonical comparison: two spellings of the same target must not report
    // a spurious rebuild.
    if current.target.canonical() != cached.target.canonical() {
        return Some(format!(
            "target changed: {} -> {}",
            cached.target, current.target
        ));
    }

    if current.compiler.family != cached.compiler.family {
        return Some(format!(
            "compiler changed: {} -> {}",
            cached.compiler.family, current.compiler.family
        ));
    }

    if current.kind != cached.kind {
        return Some(format!(
            "target kind changed: {:?} -> {:?}",
            cached.kind, current.kind
        ));
    }

    if current.pic != cached.pic {
        return Some(format!(
            "PIC setting changed: {} -> {}",
            cached.pic, current.pic
        ));
    }

    if current.public_defines != cached.public_defines {
        return Some("public defines changed".to_string());
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_abi_fingerprint() {
        let target = TargetTriple::parse("x86_64-unknown-linux-gnu");
        let compiler = CompilerIdentity::new("gcc", "13.0");

        let abi1 = AbiIdentity::new(target.clone(), compiler.clone(), TargetKind::StaticLib);
        let abi2 = AbiIdentity::new(target.clone(), compiler.clone(), TargetKind::StaticLib);

        assert_eq!(abi1.fingerprint(), abi2.fingerprint());
    }

    #[test]
    fn test_abi_fingerprint_stable_across_equivalent_spellings() {
        let compiler = CompilerIdentity::new("gcc", "13.0");

        let abi1 = AbiIdentity::new(
            TargetTriple::parse("arm-none-eabi"),
            compiler.clone(),
            TargetKind::StaticLib,
        );
        let abi2 = AbiIdentity::new(
            TargetTriple::parse("arm-unknown-none-eabi"),
            compiler,
            TargetKind::StaticLib,
        );

        assert_eq!(abi1.fingerprint(), abi2.fingerprint());
        assert!(abi1.is_compatible(&abi2));
    }

    #[test]
    fn test_abi_compatibility() {
        let target = TargetTriple::parse("x86_64-unknown-linux-gnu");
        let gcc = CompilerIdentity::new("gcc", "13.0");
        let clang = CompilerIdentity::new("clang", "17.0");

        let abi1 = AbiIdentity::new(target.clone(), gcc, TargetKind::StaticLib);
        let abi2 = AbiIdentity::new(target.clone(), clang, TargetKind::StaticLib);

        assert!(!abi1.is_compatible(&abi2));
    }
}
