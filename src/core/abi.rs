//! ABI identity computation.
//!
//! Every built artifact has an ABI identity that serves as a cache key.
//! This ensures we detect when dependencies need rebuilding due to
//! incompatible ABI changes.

use crate::core::surface::{Define, ResolvedSurface};
use crate::core::target::TargetKind;
use crate::util::hash::Fingerprint;

/// Target triple components.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetTriple {
    /// CPU architecture (x86_64, aarch64, etc.)
    pub arch: String,
    /// Vendor (unknown, apple, pc, etc.)
    pub vendor: String,
    /// Operating system (linux, darwin, windows, etc.)
    pub os: String,
    /// Environment/ABI (gnu, musl, msvc, etc.)
    pub env: Option<String>,
}

impl TargetTriple {
    /// Create a new target triple.
    pub fn new(arch: &str, vendor: &str, os: &str, env: Option<&str>) -> Self {
        TargetTriple {
            arch: arch.to_string(),
            vendor: vendor.to_string(),
            os: os.to_string(),
            env: env.map(|s| s.to_string()),
        }
    }

    /// Detect the host target triple.
    pub fn host() -> Self {
        // Use Rust's target triple as approximation
        let arch = std::env::consts::ARCH;
        let os = std::env::consts::OS;

        let (vendor, env) = match os {
            "linux" => ("unknown", Some("gnu")),
            "macos" => ("apple", None),
            "windows" => ("pc", Some("msvc")),
            _ => ("unknown", None),
        };

        TargetTriple::new(arch, vendor, os, env)
    }

    /// Parse a target triple string.
    pub fn parse(s: &str) -> Option<Self> {
        let parts: Vec<&str> = s.split('-').collect();
        if parts.len() < 3 {
            return None;
        }

        Some(TargetTriple {
            arch: parts[0].to_string(),
            vendor: parts[1].to_string(),
            os: parts[2].to_string(),
            env: parts.get(3).map(|s| s.to_string()),
        })
    }

    /// Get the triple as a string representation.
    pub fn as_str(&self) -> String {
        match &self.env {
            Some(env) => format!("{}-{}-{}-{}", self.arch, self.vendor, self.os, env),
            None => format!("{}-{}-{}", self.arch, self.vendor, self.os),
        }
    }
}

impl std::fmt::Display for TargetTriple {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.env {
            Some(env) => write!(f, "{}-{}-{}-{}", self.arch, self.vendor, self.os, env),
            None => write!(f, "{}-{}-{}", self.arch, self.vendor, self.os),
        }
    }
}

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

        fp.update_str(&self.target.to_string())
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
        self.target == other.target
            && self.compiler.family == other.compiler.family
            && self.kind == other.kind
            && self.pic == other.pic
            && self.public_defines == other.public_defines
    }
}

/// Check if a rebuild is needed based on ABI identity.
pub fn needs_rebuild(current: &AbiIdentity, cached: &AbiIdentity) -> Option<String> {
    if current.target != cached.target {
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
    fn test_target_triple() {
        let triple = TargetTriple::host();
        assert!(!triple.arch.is_empty());
        assert!(!triple.os.is_empty());
    }

    #[test]
    fn test_target_triple_parse() {
        let triple = TargetTriple::parse("x86_64-unknown-linux-gnu").unwrap();
        assert_eq!(triple.arch, "x86_64");
        assert_eq!(triple.vendor, "unknown");
        assert_eq!(triple.os, "linux");
        assert_eq!(triple.env, Some("gnu".to_string()));
    }

    #[test]
    fn test_abi_fingerprint() {
        let target = TargetTriple::new("x86_64", "unknown", "linux", Some("gnu"));
        let compiler = CompilerIdentity::new("gcc", "13.0");

        let abi1 = AbiIdentity::new(target.clone(), compiler.clone(), TargetKind::StaticLib);
        let abi2 = AbiIdentity::new(target.clone(), compiler.clone(), TargetKind::StaticLib);

        assert_eq!(abi1.fingerprint(), abi2.fingerprint());
    }

    #[test]
    fn test_abi_compatibility() {
        let target = TargetTriple::new("x86_64", "unknown", "linux", Some("gnu"));
        let gcc = CompilerIdentity::new("gcc", "13.0");
        let clang = CompilerIdentity::new("clang", "17.0");

        let abi1 = AbiIdentity::new(target.clone(), gcc, TargetKind::StaticLib);
        let abi2 = AbiIdentity::new(target.clone(), clang, TargetKind::StaticLib);

        assert!(!abi1.is_compatible(&abi2));
    }
}
