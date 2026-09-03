//! Build fingerprinting for incremental builds.
//!
//! Fingerprints capture all inputs to a build step, allowing us to skip
//! rebuilding when nothing has changed.
//!
//! # Header dependency tracking
//!
//! The hard part of incremental C/C++ builds is knowing that a `.c`/`.cpp`
//! file must be recompiled when a header it (transitively) includes changes,
//! even though the source file itself did not change.
//!
//! This module does **not** shell out to the compiler for dependency
//! information (`-MMD`/`-MF` on GCC/Clang, `/showIncludes` on MSVC). Doing
//! that properly means running the compiler's preprocessor and belongs in
//! the toolchain layer (`src/builder/toolchain/*.rs`), which is out of scope
//! for this change. Instead, [`collect_header_deps`] performs a conservative
//! textual scan for `#include` directives and resolves them against the
//! target's include directories (and, for quoted includes, the including
//! file's own directory), recursively.
//!
//! This textual scan is deliberately over-inclusive rather than
//! under-inclusive:
//! - It does not evaluate `#if`/`#ifdef`/`#elif` conditionals, so it will
//!   sometimes find and hash headers that the preprocessor would not
//!   actually have included for a given configuration. That can only cause
//!   an unnecessary rebuild, never a missed one.
//! - Includes that cannot be resolved against the known include directories
//!   (typically system headers such as `<stdio.h>`) are left untracked. This
//!   mirrors the pre-existing behavior in [`CompileFingerprint::for_source`]
//!   of silently skipping headers that don't exist on disk. System headers
//!   are not expected to change between builds of the same project, so this
//!   is an accepted, documented gap rather than a silent-stale-binary risk.
//! - It cannot follow `#include SOME_MACRO` (computed includes), which are
//!   rare in practice.
//!
//! If a project relies on computed includes and hits a stale build as a
//! result, that is the known limitation of this pass; everything else
//! (source content, flags, compiler identity/version, target triple,
//! profile) is tracked precisely.

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::builder::toolchain::CxxOptions;
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
    #[allow(clippy::too_many_arguments)]
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
    ///
    /// Every field is included. In particular `pic` and
    /// `cxx_compiler_version` are included even though they are currently
    /// always `false`/`None` respectively (see the `v0` comments in `new`);
    /// omitting them from the hash would be a silent trap for whoever wires
    /// up real values later, since the hash would then not change even
    /// though the field would start varying.
    pub fn hash(&self) -> String {
        let mut fp = HashFingerprint::new();
        fp.update_str(&self.target_triple);
        fp.update_str(&self.compiler_family);
        fp.update_str(&self.compiler_path_hash);
        fp.update_str(&self.cxx_compiler_path_hash);
        fp.update_str(&self.compiler_version);
        fp.update_opt(self.cxx_compiler_version.as_deref());
        fp.update_opt(self.effective_cpp_std.as_deref());
        fp.update_opt(self.cpp_runtime.as_deref());
        fp.update_str(&self.msvc_runtime);
        fp.update_bool(self.effective_exceptions);
        fp.update_bool(self.effective_rtti);
        fp.update_bool(self.pic);
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

/// Parse a `#include "foo.h"` / `#include <foo.h>` line.
///
/// Returns `Some((path, is_quoted))` on a match. `is_quoted` distinguishes
/// `"..."` includes (which search the including file's own directory first,
/// per the C/C++ standard) from `<...>` includes (which only search the
/// configured include directories).
fn parse_include_directive(line: &str) -> Option<(String, bool)> {
    let trimmed = line.trim_start();
    let rest = trimmed.strip_prefix('#')?;
    let rest = rest.trim_start();
    let rest = rest.strip_prefix("include")?;
    // Require a separator so `#includefoo` isn't mistaken for a directive.
    if rest
        .chars()
        .next()
        .is_some_and(|c| !c.is_whitespace() && c != '"' && c != '<')
    {
        return None;
    }
    let rest = rest.trim_start();

    if let Some(inner) = rest.strip_prefix('"') {
        let end = inner.find('"')?;
        Some((inner[..end].to_string(), true))
    } else if let Some(inner) = rest.strip_prefix('<') {
        let end = inner.find('>')?;
        Some((inner[..end].to_string(), false))
    } else {
        None
    }
}

/// Recursively collect header files transitively included by `source`.
///
/// See the module documentation for the precision/safety tradeoffs of this
/// textual scan versus compiler-emitted dependency information.
pub fn collect_header_deps(source: &Path, include_dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut discovered: BTreeSet<PathBuf> = BTreeSet::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut queue: VecDeque<PathBuf> = VecDeque::new();

    let source_canon = source
        .canonicalize()
        .unwrap_or_else(|_| source.to_path_buf());
    seen.insert(source_canon.clone());
    queue.push_back(source_canon);

    while let Some(file) = queue.pop_front() {
        let Ok(content) = std::fs::read_to_string(&file) else {
            continue;
        };
        let dir = file
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));

        for line in content.lines() {
            let Some((inc, quoted)) = parse_include_directive(line) else {
                continue;
            };

            // Quote-includes search the including file's own directory
            // first, then the configured include dirs; angle-includes only
            // search the configured include dirs. First match wins, mirroring
            // compiler search order.
            let mut search_dirs: Vec<&Path> = Vec::with_capacity(include_dirs.len() + 1);
            if quoted {
                search_dirs.push(dir.as_path());
            }
            search_dirs.extend(include_dirs.iter().map(PathBuf::as_path));

            for cdir in search_dirs {
                let candidate = cdir.join(&inc);
                if candidate.is_file() {
                    let canon = candidate.canonicalize().unwrap_or(candidate);
                    if seen.insert(canon.clone()) {
                        discovered.insert(canon.clone());
                        queue.push_back(canon);
                    }
                    break;
                }
            }
        }
    }

    discovered.into_iter().collect()
}

/// Fingerprint for a compilation unit.
///
/// Deliberately does **not** carry a "compiler" string on its own; instead
/// `toolchain_hash` is the hash of the full [`ToolchainFingerprint`], so that
/// a compiler upgrade, a target triple change, a profile switch, or a C++
/// ABI-relevant setting (exceptions/RTTI/std/MSVC runtime) all invalidate the
/// fingerprint even if the raw per-file flags list is identical.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompileFingerprint {
    /// Source file hash
    pub source_hash: String,

    /// Hash of the full toolchain fingerprint (compiler identity/version,
    /// target triple, profile, C++ ABI-relevant settings)
    pub toolchain_hash: String,

    /// Effective flags hash (include dirs, defines, cflags -- everything
    /// that varies per compile step)
    pub flags_hash: String,

    /// Header dependency hashes (transitive, see [`collect_header_deps`])
    pub header_hashes: BTreeMap<PathBuf, String>,

    /// Source language (C or C++)
    #[serde(default)]
    pub lang: String,
}

impl CompileFingerprint {
    /// Create a fingerprint for a source file.
    ///
    /// `flags` should be the *complete* set of per-file compile inputs that
    /// affect the compiler invocation: include directories, preprocessor
    /// defines, and cflags (both profile-derived and target-derived). The
    /// caller is responsible for assembling that list; see
    /// `NativeBuilder::compile_fingerprint_flags` in `native.rs`.
    ///
    /// `headers` should be the transitive header dependencies as produced by
    /// [`collect_header_deps`].
    pub fn for_source(
        source: &Path,
        toolchain: &ToolchainFingerprint,
        flags: &[String],
        headers: &[PathBuf],
        lang: Language,
    ) -> Result<Self> {
        let source_hash = sha256_file(source)?;
        let toolchain_hash = toolchain.hash();

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
            toolchain_hash,
            flags_hash,
            header_hashes,
            lang: lang.as_str().to_string(),
        })
    }

    /// Check if the fingerprint matches (nothing has changed).
    pub fn matches(&self, other: &CompileFingerprint) -> bool {
        self.source_hash == other.source_hash
            && self.toolchain_hash == other.toolchain_hash
            && self.flags_hash == other.flags_hash
            && self.header_hashes == other.header_hashes
            && self.lang == other.lang
    }
}

/// Fingerprint for a link step (link, or archive).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkFingerprint {
    /// Object file hashes
    pub object_hashes: BTreeMap<PathBuf, String>,

    /// Library hashes (resolved dependency library files, where resolvable)
    pub lib_hashes: BTreeMap<PathBuf, String>,

    /// Linker flags hash (also covers unresolved library references, so a
    /// change in *which* libraries are requested is still caught even when
    /// we can't resolve them to a file on disk)
    pub flags_hash: String,

    /// ABI identity (target/compiler/kind/pic/defines)
    pub abi: String,
}

impl LinkFingerprint {
    /// Create a fingerprint for a link step.
    pub fn for_link(
        objects: &[PathBuf],
        libs: &[PathBuf],
        flags: &[String],
        abi: &crate::core::abi::AbiIdentity,
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
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct FingerprintCache {
    /// Compile fingerprints by source path
    pub compile: BTreeMap<PathBuf, CompileFingerprint>,

    /// Link fingerprints by output path (archives and links share this map;
    /// output paths are unique per build plan)
    pub link: BTreeMap<PathBuf, LinkFingerprint>,
}

impl FingerprintCache {
    /// Load fingerprint cache from a file.
    ///
    /// A missing, unreadable, or unparseable cache is treated as "empty" --
    /// not as an error -- since the safe fallback is simply to rebuild
    /// everything, which is what an empty cache produces.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(FingerprintCache::default());
        }

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => return Ok(FingerprintCache::default()),
        };
        let cache = serde_json::from_str(&content).unwrap_or_default();
        Ok(cache)
    }

    /// Save fingerprint cache to a file.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
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

    /// Check if a target needs relinking/rearchiving.
    pub fn needs_link(&self, output: &Path, current: &LinkFingerprint) -> bool {
        match self.link.get(output) {
            Some(cached) => !cached.matches(current),
            None => true,
        }
    }

    /// Update compile fingerprint.
    pub fn update_compile(&mut self, source: PathBuf, fingerprint: CompileFingerprint) {
        self.compile.insert(source, fingerprint);
    }

    /// Update link fingerprint.
    pub fn update_link(&mut self, output: PathBuf, fingerprint: LinkFingerprint) {
        self.link.insert(output, fingerprint);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn toolchain_fp(profile: &str) -> ToolchainFingerprint {
        ToolchainFingerprint::new(
            "x86_64-unknown-linux-gnu",
            "gcc",
            Path::new("/usr/bin/gcc"),
            Path::new("/usr/bin/g++"),
            "13.2",
            None,
            profile,
        )
    }

    #[test]
    fn test_compile_fingerprint_stable_when_unchanged() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("test.c");
        std::fs::write(&source, "int main() {}").unwrap();

        let tc = toolchain_fp("debug");

        let fp1 =
            CompileFingerprint::for_source(&source, &tc, &["-Wall".to_string()], &[], Language::C)
                .unwrap();

        let fp2 =
            CompileFingerprint::for_source(&source, &tc, &["-Wall".to_string()], &[], Language::C)
                .unwrap();

        assert!(fp1.matches(&fp2));
    }

    #[test]
    fn test_flag_change_invalidates_fingerprint() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("test.c");
        std::fs::write(&source, "int main() {}").unwrap();
        let tc = toolchain_fp("debug");

        let fp1 =
            CompileFingerprint::for_source(&source, &tc, &["-Wall".to_string()], &[], Language::C)
                .unwrap();

        let fp2 = CompileFingerprint::for_source(
            &source,
            &tc,
            &["-Wall".to_string(), "-O2".to_string()],
            &[],
            Language::C,
        )
        .unwrap();

        assert!(!fp1.matches(&fp2));
    }

    #[test]
    fn test_source_content_change_invalidates_fingerprint() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("test.c");
        std::fs::write(&source, "int main() { return 0; }").unwrap();
        let tc = toolchain_fp("debug");

        let fp1 = CompileFingerprint::for_source(&source, &tc, &[], &[], Language::C).unwrap();

        std::fs::write(&source, "int main() { return 1; }").unwrap();
        let fp2 = CompileFingerprint::for_source(&source, &tc, &[], &[], Language::C).unwrap();

        assert!(!fp1.matches(&fp2));
    }

    #[test]
    fn test_compiler_version_change_invalidates_fingerprint() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("test.c");
        std::fs::write(&source, "int main() {}").unwrap();

        let tc1 = ToolchainFingerprint::new(
            "x86_64-unknown-linux-gnu",
            "gcc",
            Path::new("/usr/bin/gcc"),
            Path::new("/usr/bin/g++"),
            "12.0",
            None,
            "debug",
        );
        let tc2 = ToolchainFingerprint::new(
            "x86_64-unknown-linux-gnu",
            "gcc",
            Path::new("/usr/bin/gcc"),
            Path::new("/usr/bin/g++"),
            "13.0",
            None,
            "debug",
        );

        let fp1 = CompileFingerprint::for_source(&source, &tc1, &[], &[], Language::C).unwrap();
        let fp2 = CompileFingerprint::for_source(&source, &tc2, &[], &[], Language::C).unwrap();

        assert!(!fp1.matches(&fp2));
        assert_ne!(tc1.hash(), tc2.hash());
    }

    #[test]
    fn test_target_triple_change_invalidates_fingerprint() {
        let tc1 = ToolchainFingerprint::new(
            "x86_64-unknown-linux-gnu",
            "gcc",
            Path::new("/usr/bin/gcc"),
            Path::new("/usr/bin/g++"),
            "13.0",
            None,
            "debug",
        );
        let tc2 = ToolchainFingerprint::new(
            "aarch64-unknown-linux-gnu",
            "gcc",
            Path::new("/usr/bin/gcc"),
            Path::new("/usr/bin/g++"),
            "13.0",
            None,
            "debug",
        );

        assert_ne!(tc1.hash(), tc2.hash());
    }

    #[test]
    fn test_profile_change_invalidates_fingerprint() {
        let tc1 = toolchain_fp("debug");
        let tc2 = toolchain_fp("release");

        assert_ne!(tc1.hash(), tc2.hash());
    }

    #[test]
    fn test_cxx_opts_change_invalidates_fingerprint() {
        use crate::core::target::CppStandard;

        let tc1 = ToolchainFingerprint::new(
            "x86_64-unknown-linux-gnu",
            "gcc",
            Path::new("/usr/bin/gcc"),
            Path::new("/usr/bin/g++"),
            "13.0",
            None,
            "debug",
        );

        let opts = CxxOptions {
            std: Some(CppStandard::Cpp20),
            exceptions: false,
            ..Default::default()
        };
        let tc2 = ToolchainFingerprint::new(
            "x86_64-unknown-linux-gnu",
            "gcc",
            Path::new("/usr/bin/gcc"),
            Path::new("/usr/bin/g++"),
            "13.0",
            Some(&opts),
            "debug",
        );

        assert_ne!(tc1.hash(), tc2.hash());
    }

    #[test]
    fn test_header_change_invalidates_fingerprint() {
        let tmp = TempDir::new().unwrap();
        let header = tmp.path().join("foo.h");
        std::fs::write(&header, "#define X 1\n").unwrap();
        let source = tmp.path().join("test.c");
        std::fs::write(&source, "#include \"foo.h\"\nint main() {}\n").unwrap();
        let tc = toolchain_fp("debug");

        let headers = vec![header.clone()];
        let fp1 = CompileFingerprint::for_source(&source, &tc, &[], &headers, Language::C).unwrap();

        std::fs::write(&header, "#define X 2\n").unwrap();
        let fp2 = CompileFingerprint::for_source(&source, &tc, &[], &headers, Language::C).unwrap();

        assert!(!fp1.matches(&fp2));
    }

    #[test]
    fn test_collect_header_deps_direct_and_transitive() {
        let tmp = TempDir::new().unwrap();
        let inc_dir = tmp.path().join("include");
        std::fs::create_dir_all(&inc_dir).unwrap();

        let grandchild = inc_dir.join("grandchild.h");
        std::fs::write(&grandchild, "int gc;\n").unwrap();

        let child = inc_dir.join("child.h");
        std::fs::write(&child, "#include <grandchild.h>\nint c;\n").unwrap();

        let source = tmp.path().join("main.c");
        std::fs::write(&source, "#include \"child.h\"\nint main() { return 0; }\n").unwrap();

        let deps = collect_header_deps(&source, std::slice::from_ref(&inc_dir));

        let child_canon = child.canonicalize().unwrap();
        let grandchild_canon = grandchild.canonicalize().unwrap();

        assert!(deps.contains(&child_canon));
        assert!(deps.contains(&grandchild_canon));
        assert_eq!(deps.len(), 2);
    }

    #[test]
    fn test_collect_header_deps_quote_include_searches_own_dir_first() {
        let tmp = TempDir::new().unwrap();
        let header = tmp.path().join("local.h");
        std::fs::write(&header, "int x;\n").unwrap();

        let source = tmp.path().join("main.c");
        std::fs::write(&source, "#include \"local.h\"\n").unwrap();

        // No include_dirs at all -- must still find local.h via the
        // including file's own directory.
        let deps = collect_header_deps(&source, &[]);
        assert_eq!(deps, vec![header.canonicalize().unwrap()]);
    }

    #[test]
    fn test_collect_header_deps_unresolvable_system_header_is_skipped() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("main.c");
        std::fs::write(&source, "#include <stdio.h>\nint main() {}\n").unwrap();

        let deps = collect_header_deps(&source, &[]);
        assert!(deps.is_empty());
    }

    #[test]
    fn test_language_affects_fingerprint() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("test.cpp");
        std::fs::write(&source, "int main() {}").unwrap();
        let tc = toolchain_fp("debug");

        let fp_c = CompileFingerprint::for_source(&source, &tc, &[], &[], Language::C).unwrap();
        let fp_cxx = CompileFingerprint::for_source(&source, &tc, &[], &[], Language::Cxx).unwrap();

        // Different language = different fingerprint
        assert!(!fp_c.matches(&fp_cxx));
    }

    #[test]
    fn test_fingerprint_cache_round_trip() {
        let tmp = TempDir::new().unwrap();
        let cache_path = tmp.path().join("fingerprints.json");

        let mut cache = FingerprintCache::default();

        let source = tmp.path().join("test.c");
        std::fs::write(&source, "int main() {}").unwrap();
        let tc = toolchain_fp("debug");

        let fp = CompileFingerprint::for_source(&source, &tc, &[], &[], Language::C).unwrap();

        cache.update_compile(source.clone(), fp.clone());
        cache.save(&cache_path).unwrap();

        let loaded = FingerprintCache::load(&cache_path).unwrap();
        assert!(!loaded.needs_compile(&source, &fp));
    }

    #[test]
    fn test_fingerprint_cache_missing_file_needs_everything() {
        let tmp = TempDir::new().unwrap();
        let cache_path = tmp.path().join("does-not-exist.json");
        let cache = FingerprintCache::load(&cache_path).unwrap();

        let source = tmp.path().join("test.c");
        std::fs::write(&source, "int main() {}").unwrap();
        let tc = toolchain_fp("debug");
        let fp = CompileFingerprint::for_source(&source, &tc, &[], &[], Language::C).unwrap();

        assert!(cache.needs_compile(&source, &fp));
    }

    #[test]
    fn test_link_fingerprint_object_change_invalidates() {
        use crate::core::abi::{AbiIdentity, CompilerIdentity};
        use crate::core::target::{TargetKind, TargetTriple};

        let tmp = TempDir::new().unwrap();
        let obj = tmp.path().join("a.o");
        std::fs::write(&obj, "obj-v1").unwrap();

        let abi = AbiIdentity::new(
            TargetTriple::host(),
            CompilerIdentity::new("gcc", "13.0"),
            TargetKind::Exe,
        );

        let objects = vec![obj.clone()];
        let fp1 = LinkFingerprint::for_link(&objects, &[], &[], &abi).unwrap();

        std::fs::write(&obj, "obj-v2").unwrap();
        let fp2 = LinkFingerprint::for_link(&objects, &[], &[], &abi).unwrap();

        assert!(!fp1.matches(&fp2));
    }
}
