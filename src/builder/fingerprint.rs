//! Build fingerprinting for incremental builds.
//!
//! Fingerprints capture all inputs to a build step, allowing us to skip
//! rebuilding when nothing has changed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::builder::toolchain::CxxOptions;
use crate::core::abi::AbiIdentity;
use crate::core::manifest::MsvcRuntime;
use crate::core::target::Language;
use crate::util::hash::{sha256_file, sha256_str, Fingerprint as HashFingerprint};

/// Toolchain fingerprint for cache invalidation.
///
/// This captures all relevant toolchain settings that affect build output.
/// If any of these change, the entire build should be invalidated.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolchainFingerprint {
    /// Target triple (e.g., "x86_64-unknown-linux-gnu")
    pub target_triple: String,

    /// Compiler family (gcc/clang/msvc)
    pub compiler_family: String,

    /// Hash of normalized C compiler path
    pub compiler_path_hash: String,

    /// Hash of normalized C++ compiler path
    pub cxx_compiler_path_hash: String,

    /// Compiler version
    pub compiler_version: String,

    /// C++ compiler version (if different from C compiler)
    pub cxx_compiler_version: Option<String>,

    /// Effective C++ standard
    pub effective_cpp_std: Option<String>,

    /// C++ runtime library
    pub cpp_runtime: Option<String>,

    /// MSVC runtime ("dynamic" or "static")
    pub msvc_runtime: String,

    /// Whether exceptions are enabled
    pub effective_exceptions: bool,

    /// Whether RTTI is enabled
    pub effective_rtti: bool,

    /// Whether PIC is enabled
    pub pic: bool,

    /// Build profile (debug/release)
    pub profile: String,

    /// Harbour version
    pub harbour_version: String,
}

impl ToolchainFingerprint {
    /// Create a new toolchain fingerprint.
    pub fn new(
        target_triple: &str,
        compiler_family: &str,
        compiler_path: &Path,
        cxx_compiler_path: &Path,
        compiler_version: &str,
        cxx_opts: Option<&CxxOptions>,
        profile: &str,
    ) -> Self {
        let cpp_std = cxx_opts.and_then(|o| o.std);
        let cpp_runtime = cxx_opts.and_then(|o| o.runtime);
        let msvc_runtime = cxx_opts.map(|o| o.msvc_runtime).unwrap_or_default();

        ToolchainFingerprint {
            target_triple: target_triple.to_string(),
            compiler_family: compiler_family.to_string(),
            compiler_path_hash: hash_path(compiler_path),
            cxx_compiler_path_hash: hash_path(cxx_compiler_path),
            compiler_version: compiler_version.to_string(),
            cxx_compiler_version: None, // v0: assume same as C compiler
            effective_cpp_std: cpp_std.map(|s| s.as_flag_value().to_string()),
            cpp_runtime: cpp_runtime.map(|r| match r {
                crate::core::manifest::CppRuntime::Libstdcxx => "libstdc++".to_string(),
                crate::core::manifest::CppRuntime::Libcxx => "libc++".to_string(),
            }),
            msvc_runtime: match msvc_runtime {
                MsvcRuntime::Dynamic => "dynamic".to_string(),
                MsvcRuntime::Static => "static".to_string(),
            },
            effective_exceptions: cxx_opts.map(|o| o.exceptions).unwrap_or(true),
            effective_rtti: cxx_opts.map(|o| o.rtti).unwrap_or(true),
            pic: false, // v0: not tracked yet
            profile: profile.to_string(),
            harbour_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    /// Generate a hash representing this fingerprint.
    pub fn hash(&self) -> String {
        let mut fp = HashFingerprint::new();
        fp.update_str(&self.target_triple);
        fp.update_str(&self.compiler_family);
        fp.update_str(&self.compiler_path_hash);
        fp.update_str(&self.cxx_compiler_path_hash);
        fp.update_str(&self.compiler_version);
        if let Some(ref v) = self.effective_cpp_std {
            fp.update_str(v);
        }
        if let Some(ref r) = self.cpp_runtime {
            fp.update_str(r);
        }
        fp.update_str(&self.msvc_runtime);
        fp.update_str(if self.effective_exceptions { "exc" } else { "no-exc" });
        fp.update_str(if self.effective_rtti { "rtti" } else { "no-rtti" });
        fp.update_str(&self.profile);
        fp.update_str(&self.harbour_version);
        fp.finish_short()
    }
}

/// Hash a path for fingerprinting.
fn hash_path(path: &Path) -> String {
    // Canonicalize if possible, otherwise use as-is
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    sha256_str(&canonical.display().to_string())[..16].to_string()
}

/// Fingerprint for a compilation unit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompileFingerprint {
    /// Source file hash
    pub source_hash: String,

    /// Compiler identity
    pub compiler: String,

    /// Effective flags hash
    pub flags_hash: String,

    /// Header dependency hashes
    pub header_hashes: BTreeMap<PathBuf, String>,

    /// Source language (C or C++)
    #[serde(default)]
    pub lang: String,
}

impl CompileFingerprint {
    /// Create a fingerprint for a source file.
    pub fn for_source(
        source: &Path,
        compiler: &str,
        flags: &[String],
        headers: &[PathBuf],
        lang: Language,
    ) -> Result<Self> {
        let source_hash = sha256_file(source)?;

        let mut fp = HashFingerprint::new();
        for flag in flags {
            fp.update_str(flag);
        }
        let flags_hash = fp.finish_short();

        let mut header_hashes = BTreeMap::new();
        for header in headers {
            if header.exists() {
                header_hashes.insert(header.clone(), sha256_file(header)?);
            }
        }

        Ok(CompileFingerprint {
            source_hash,
            compiler: compiler.to_string(),
            flags_hash,
            header_hashes,
            lang: lang.as_str().to_string(),
        })
    }

    /// Check if the fingerprint matches (nothing has changed).
    pub fn matches(&self, other: &CompileFingerprint) -> bool {
        self.source_hash == other.source_hash
            && self.compiler == other.compiler
            && self.flags_hash == other.flags_hash
            && self.header_hashes == other.header_hashes
            && self.lang == other.lang
    }
}

/// Fingerprint for a link step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkFingerprint {
    /// Object file hashes
    pub object_hashes: BTreeMap<PathBuf, String>,

    /// Library hashes
    pub lib_hashes: BTreeMap<PathBuf, String>,

    /// Linker flags hash
    pub flags_hash: String,

    /// ABI identity
    pub abi: String,
}

impl LinkFingerprint {
    /// Create a fingerprint for a link step.
    pub fn for_link(
        objects: &[PathBuf],
        libs: &[PathBuf],
        flags: &[String],
        abi: &AbiIdentity,
    ) -> Result<Self> {
        let mut object_hashes = BTreeMap::new();
        for obj in objects {
            if obj.exists() {
                object_hashes.insert(obj.clone(), sha256_file(obj)?);
            }
        }

        let mut lib_hashes = BTreeMap::new();
        for lib in libs {
            if lib.exists() {
                lib_hashes.insert(lib.clone(), sha256_file(lib)?);
            }
        }

        let mut fp = HashFingerprint::new();
        for flag in flags {
            fp.update_str(flag);
        }
        let flags_hash = fp.finish_short();

        Ok(LinkFingerprint {
            object_hashes,
            lib_hashes,
            flags_hash,
            abi: abi.fingerprint(),
        })
    }

    /// Check if the fingerprint matches.
    pub fn matches(&self, other: &LinkFingerprint) -> bool {
        self.object_hashes == other.object_hashes
            && self.lib_hashes == other.lib_hashes
            && self.flags_hash == other.flags_hash
            && self.abi == other.abi
    }
}

/// Fingerprint cache for a package.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct FingerprintCache {
    /// Compile fingerprints by source path
    pub compile: BTreeMap<PathBuf, CompileFingerprint>,

    /// Link fingerprints by target name
    pub link: BTreeMap<String, LinkFingerprint>,
}

impl FingerprintCache {
    /// Load fingerprint cache from a file.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(FingerprintCache::default());
        }

        let content = std::fs::read_to_string(path)?;
        let cache: FingerprintCache = serde_json::from_str(&content)?;
        Ok(cache)
    }

    /// Save fingerprint cache to a file.
    pub fn save(&self, path: &Path) -> Result<()> {
        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }

    /// Check if a source file needs recompilation.
    pub fn needs_compile(&self, source: &Path, current: &CompileFingerprint) -> bool {
        match self.compile.get(source) {
            Some(cached) => !cached.matches(current),
            None => true,
        }
    }

    /// Check if a target needs relinking.
    pub fn needs_link(&self, target: &str, current: &LinkFingerprint) -> bool {
        match self.link.get(target) {
            Some(cached) => !cached.matches(current),
            None => true,
        }
    }

    /// Update compile fingerprint.
    pub fn update_compile(&mut self, source: PathBuf, fingerprint: CompileFingerprint) {
        self.compile.insert(source, fingerprint);
    }

    /// Update link fingerprint.
    pub fn update_link(&mut self, target: String, fingerprint: LinkFingerprint) {
        self.link.insert(target, fingerprint);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_compile_fingerprint() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("test.c");
        std::fs::write(&source, "int main() {}").unwrap();

        let fp1 = CompileFingerprint::for_source(
            &source,
            "gcc",
            &["-Wall".to_string()],
            &[],
            Language::C,
        )
        .unwrap();

        let fp2 = CompileFingerprint::for_source(
            &source,
            "gcc",
            &["-Wall".to_string()],
            &[],
            Language::C,
        )
        .unwrap();

        assert!(fp1.matches(&fp2));

        // Change flags
        let fp3 = CompileFingerprint::for_source(
            &source,
            "gcc",
            &["-Wall".to_string(), "-O2".to_string()],
            &[],
            Language::C,
        )
        .unwrap();

        assert!(!fp1.matches(&fp3));
    }

    #[test]
    fn test_fingerprint_cache() {
        let tmp = TempDir::new().unwrap();
        let cache_path = tmp.path().join("fingerprints.json");

        let mut cache = FingerprintCache::default();

        let source = tmp.path().join("test.c");
        std::fs::write(&source, "int main() {}").unwrap();

        let fp = CompileFingerprint::for_source(&source, "gcc", &[], &[], Language::C).unwrap();

        cache.update_compile(source.clone(), fp.clone());
        cache.save(&cache_path).unwrap();

        let loaded = FingerprintCache::load(&cache_path).unwrap();
        assert!(!loaded.needs_compile(&source, &fp));
    }

    #[test]
    fn test_language_affects_fingerprint() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("test.cpp");
        std::fs::write(&source, "int main() {}").unwrap();

        let fp_c = CompileFingerprint::for_source(
            &source,
            "gcc",
            &[],
            &[],
            Language::C,
        )
        .unwrap();

        let fp_cxx = CompileFingerprint::for_source(
            &source,
            "g++",
            &[],
            &[],
            Language::Cxx,
        )
        .unwrap();

        // Different language = different fingerprint
        assert!(!fp_c.matches(&fp_cxx));
    }
}
