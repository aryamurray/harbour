//! Build fingerprinting for incremental builds.
//!
//! Fingerprints capture all inputs to a build step, allowing us to skip
//! rebuilding when nothing has changed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::core::abi::AbiIdentity;
use crate::util::hash::{sha256_file, Fingerprint as HashFingerprint};

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
}

impl CompileFingerprint {
    /// Create a fingerprint for a source file.
    pub fn for_source(
        source: &Path,
        compiler: &str,
        flags: &[String],
        headers: &[PathBuf],
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
        })
    }

    /// Check if the fingerprint matches (nothing has changed).
    pub fn matches(&self, other: &CompileFingerprint) -> bool {
        self.source_hash == other.source_hash
            && self.compiler == other.compiler
            && self.flags_hash == other.flags_hash
            && self.header_hashes == other.header_hashes
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
        )
        .unwrap();

        let fp2 = CompileFingerprint::for_source(
            &source,
            "gcc",
            &["-Wall".to_string()],
            &[],
        )
        .unwrap();

        assert!(fp1.matches(&fp2));

        // Change flags
        let fp3 = CompileFingerprint::for_source(
            &source,
            "gcc",
            &["-Wall".to_string(), "-O2".to_string()],
            &[],
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

        let fp = CompileFingerprint::for_source(&source, "gcc", &[], &[]).unwrap();

        cache.update_compile(source.clone(), fp.clone());
        cache.save(&cache_path).unwrap();

        let loaded = FingerprintCache::load(&cache_path).unwrap();
        assert!(!loaded.needs_compile(&source, &fp));
    }
}
