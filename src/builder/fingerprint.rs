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

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
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

    /// The deployment target and SDK the compiler resolved, for Apple
    /// targets only. Absent elsewhere, and then absent from the hash too, so
    /// a Linux or Windows fingerprint is exactly what it was before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apple: Option<crate::builder::apple_env::AppleBuildEnv>,
}

impl ToolchainFingerprint {
    /// Record the Apple deployment target and SDK this build resolved.
    pub fn with_apple(mut self, apple: Option<crate::builder::apple_env::AppleBuildEnv>) -> Self {
        self.apple = apple;
        self
    }

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
            apple: None,
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
        // Only when present, so a target with nothing to record hashes as it
        // always did. The prefix keeps the pair unambiguous.
        if let Some(apple) = &self.apple {
            fp.update_str("apple");
            fp.update_str(&apple.deployment_target);
            fp.update_str(&apple.sdk);
        }
        fp.finish_short()
    }
}

/// Hash a path for fingerprinting.
fn hash_path(path: &Path) -> String {
    // Canonicalize if possible, otherwise use as-is
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    sha256_str(&canonical.display().to_string())[..16].to_string()
}

/// What one file contributes to a fingerprint, remembered for the life of the
/// process.
///
/// Every translation unit hashes and scans every header it includes, and the
/// headers are mostly the same ones: CPython's core is 187 files that each pull
/// in about 150 headers, so an unchanged rebuild read and parsed the same few
/// hundred files some 28,000 times, and a host tool's nested build repeated it.
/// Keyed on the file's modification time and length, so an edit between two
/// uses is seen -- except two writes of equal length within one timestamp tick,
/// which is why [`forget_file_facts`] runs whenever something that writes
/// sources (a generator) has just run.
struct FileFacts {
    modified: Option<std::time::SystemTime>,
    len: u64,
    hash: Option<String>,
    includes: Option<std::sync::Arc<Vec<(String, bool)>>>,
}

/// A compile step that has already been found up to date, or built, in this
/// process, so the passes that follow it need not look again.
///
/// A host tool's nested build re-plans and re-checks the same objects as the
/// build that asked for it, and an unchanged CPython is 187 of them checked
/// three times. Valid only while nothing that feeds the step changed: the output
/// file's stamp and the stamp of the source and of every header it includes are
/// compared on recall (a `stat` each, not a read), and the callers of
/// [`forget_file_facts`] drop the lot after a generator or probe rewrote an
/// input.
struct VerifiedCompile {
    flags_hash: String,
    toolchain_hash: String,
    out_modified: Option<std::time::SystemTime>,
    out_len: u64,
    inputs: Vec<(PathBuf, Option<std::time::SystemTime>, u64)>,
    fingerprint: CompileFingerprint,
}

/// Everything remembered about files and verified compile steps.
///
/// An instance rather than bare statics so that its tests can own one: the
/// process-wide instance is cleared by any generator that runs, including in
/// other tests running in parallel, which would otherwise empty a test's
/// entries between its two assertions.
#[derive(Default)]
struct Memo {
    facts: std::sync::Mutex<HashMap<PathBuf, FileFacts>>,
    verified: std::sync::Mutex<HashMap<PathBuf, VerifiedCompile>>,
}

fn global_memo() -> &'static Memo {
    static MEMO: std::sync::OnceLock<Memo> = std::sync::OnceLock::new();
    MEMO.get_or_init(Memo::default)
}

fn file_stamp(path: &Path) -> Option<(Option<std::time::SystemTime>, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok(), meta.len()))
}

/// Is this modification time too recent for its stamp to be trusted?
///
/// A stamp is `(modified, length)`, and two writes of the same length inside one
/// timestamp tick leave it unchanged: a tick is about 15 ms on Windows and can be
/// seconds on some filesystems. Git has the same problem and the same answer
/// ("racily clean" entries): a file modified within the last couple of seconds is
/// never remembered, because a second edit moments later would be invisible. A
/// header edited an hour ago -- nearly every file of an unchanged rebuild -- is
/// remembered as before. A time that is unknown, or in the future, is racy.
fn is_racy(modified: Option<std::time::SystemTime>) -> bool {
    const RACY_WINDOW: std::time::Duration = std::time::Duration::from_secs(2);
    match modified.and_then(|m| std::time::SystemTime::now().duration_since(m).ok()) {
        Some(age) => age < RACY_WINDOW,
        None => true,
    }
}

impl Memo {
    fn forget(&self) {
        if let Ok(mut facts) = self.facts.lock() {
            facts.clear();
        }
        if let Ok(mut verified) = self.verified.lock() {
            verified.clear();
        }
    }

    /// `sha256_file`, remembered. Falls through to a real hash when the file's
    /// stamp changed or it has not been seen.
    fn hash(&self, path: &Path) -> Result<String> {
        let stamp = file_stamp(path);
        if let (Some((modified, len)), Ok(facts)) = (stamp, self.facts.lock()) {
            if let Some(f) = facts.get(path) {
                if f.modified == modified && f.len == len {
                    if let Some(hash) = &f.hash {
                        return Ok(hash.clone());
                    }
                }
            }
        }
        let hash = sha256_file(path)?;
        if let (Some((modified, len)), Ok(mut facts)) = (stamp, self.facts.lock()) {
            if !is_racy(modified) {
                let entry = Self::entry(&mut facts, path, modified, len);
                entry.hash = Some(hash.clone());
            }
        }
        Ok(hash)
    }

    /// The `#include` directives in `path`, remembered. `None` if it cannot be
    /// read.
    fn includes(&self, path: &Path) -> Option<std::sync::Arc<Vec<(String, bool)>>> {
        let stamp = file_stamp(path);
        if let (Some((modified, len)), Ok(facts)) = (stamp, self.facts.lock()) {
            if let Some(f) = facts.get(path) {
                if f.modified == modified && f.len == len {
                    if let Some(includes) = &f.includes {
                        return Some(includes.clone());
                    }
                }
            }
        }
        let content = std::fs::read_to_string(path).ok()?;
        let includes: std::sync::Arc<Vec<(String, bool)>> = std::sync::Arc::new(
            content
                .lines()
                .filter_map(parse_include_directive)
                .collect(),
        );
        if let (Some((modified, len)), Ok(mut facts)) = (stamp, self.facts.lock()) {
            if !is_racy(modified) {
                let entry = Self::entry(&mut facts, path, modified, len);
                entry.includes = Some(includes.clone());
            }
        }
        Some(includes)
    }

    /// The entry for `path`, replaced by an empty one if its stamp moved.
    fn entry<'a>(
        facts: &'a mut HashMap<PathBuf, FileFacts>,
        path: &Path,
        modified: Option<std::time::SystemTime>,
        len: u64,
    ) -> &'a mut FileFacts {
        let entry = facts.entry(path.to_path_buf()).or_insert(FileFacts {
            modified,
            len,
            hash: None,
            includes: None,
        });
        if entry.modified != modified || entry.len != len {
            *entry = FileFacts {
                modified,
                len,
                hash: None,
                includes: None,
            };
        }
        entry
    }

    fn recall_verified(
        &self,
        output: &Path,
        flags_hash: &str,
        toolchain_hash: &str,
    ) -> Option<CompileFingerprint> {
        let (modified, len) = file_stamp(output)?;
        let verified = self.verified.lock().ok()?;
        let v = verified.get(output)?;
        if v.flags_hash != flags_hash
            || v.toolchain_hash != toolchain_hash
            || v.out_modified != modified
            || v.out_len != len
        {
            return None;
        }
        // Every input must be exactly as it was: present, same stamp.
        let unchanged = v
            .inputs
            .iter()
            .all(|(path, m, l)| file_stamp(path) == Some((*m, *l)));
        unchanged.then(|| v.fingerprint.clone())
    }

    fn remember_verified(&self, output: &Path, source: &Path, fingerprint: &CompileFingerprint) {
        let Some((modified, len)) = file_stamp(output) else {
            return;
        };
        if is_racy(modified) {
            // Just built: another write of the same length in the same tick
            // would not move the stamp. Verify it again next time.
            return;
        }
        // The source and every header the fingerprint covers. If any cannot be
        // stamped there is nothing sound to compare later, so remember nothing.
        let mut inputs = Vec::with_capacity(fingerprint.header_hashes.len() + 1);
        for path in
            std::iter::once(source).chain(fingerprint.header_hashes.keys().map(PathBuf::as_path))
        {
            let Some((m, l)) = file_stamp(path) else {
                return;
            };
            if is_racy(m) {
                return;
            }
            inputs.push((path.to_path_buf(), m, l));
        }
        if let Ok(mut verified) = self.verified.lock() {
            verified.insert(
                output.to_path_buf(),
                VerifiedCompile {
                    flags_hash: fingerprint.flags_hash.clone(),
                    toolchain_hash: fingerprint.toolchain_hash.clone(),
                    out_modified: modified,
                    out_len: len,
                    inputs,
                    fingerprint: fingerprint.clone(),
                },
            );
        }
    }
}

/// Drop everything remembered about files and about compile steps already
/// verified. Call it after anything that may have rewritten an input: a
/// generator, or a probe that changed its generated header.
pub fn forget_file_facts() {
    global_memo().forget();
}

/// The hash of a compile step's flag list, as stored in [`CompileFingerprint`].
pub fn hash_flags(flags: &[String]) -> String {
    let mut fp = HashFingerprint::new();
    for flag in flags {
        fp.update_str(flag);
    }
    fp.finish_short()
}

fn cached_hash(path: &Path) -> Result<String> {
    global_memo().hash(path)
}

fn cached_includes(path: &Path) -> Option<std::sync::Arc<Vec<(String, bool)>>> {
    global_memo().includes(path)
}

/// The fingerprint of `output`'s step, if this process already verified it with
/// these flags and this toolchain and nothing it depends on has moved since.
pub fn recall_verified_compile(
    output: &Path,
    flags_hash: &str,
    toolchain_hash: &str,
) -> Option<CompileFingerprint> {
    global_memo().recall_verified(output, flags_hash, toolchain_hash)
}

/// Record that `output`'s step is up to date as of now.
pub fn remember_verified_compile(output: &Path, source: &Path, fingerprint: &CompileFingerprint) {
    global_memo().remember_verified(output, source, fingerprint);
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
        let Some(includes) = cached_includes(&file) else {
            continue;
        };
        let dir = file
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));

        for (inc, quoted) in includes.iter().map(|(i, q)| (i.clone(), *q)) {
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

        let flags_hash = hash_flags(flags);

        let mut header_hashes = BTreeMap::new();
        for header in headers {
            if header.exists() {
                header_hashes.insert(header.clone(), cached_hash(header)?);
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
                object_hashes.insert(obj.clone(), cached_hash(obj)?);
            }
        }

        let mut lib_hashes = BTreeMap::new();
        for lib in libs {
            if lib.exists() {
                lib_hashes.insert(lib.clone(), cached_hash(lib)?);
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

    /// Check if an object needs recompiling. Keyed by the *object* file, since
    /// two targets can compile one source under different flags.
    pub fn needs_compile(&self, object: &Path, current: &CompileFingerprint) -> bool {
        match self.compile.get(object) {
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
    pub fn update_compile(&mut self, object: PathBuf, fingerprint: CompileFingerprint) {
        self.compile.insert(object, fingerprint);
    }

    /// Update link fingerprint.
    pub fn update_link(&mut self, output: PathBuf, fingerprint: LinkFingerprint) {
        self.link.insert(output, fingerprint);
    }
}

#[cfg(test)]
mod tests {
    /// A file modified moments ago is never trusted from the memo: a second
    /// write of the same length inside one timestamp tick would be invisible.
    /// This is the case two fingerprint tests hit on Windows, where a tick is
    /// about 15 ms.
    #[test]
    fn a_just_modified_file_is_never_served_from_the_memo() {
        let memo = Memo::default();
        let dir = tempfile::tempdir().unwrap();
        let header = dir.path().join("fresh.h");
        std::fs::write(&header, "int one;\n").unwrap();
        let before = memo.hash(&header).unwrap();

        std::fs::write(&header, "int two;\n").unwrap(); // same length, same instant
        assert_ne!(
            before,
            memo.hash(&header).unwrap(),
            "a file modified within the racy window must be re-hashed"
        );

        // The same goes for a verified compile whose input was just written.
        let obj = dir.path().join("fresh.o");
        std::fs::write(&obj, b"o").unwrap();
        set_mtime(&obj, 1_000);
        memo.remember_verified(&obj, &header, &sample_fingerprint("f", "t"));
        assert!(
            memo.recall_verified(&obj, "f", "t").is_none(),
            "an input modified moments ago makes the step unsafe to remember"
        );
    }

    fn sample_fingerprint(flags: &str, toolchain: &str) -> CompileFingerprint {
        CompileFingerprint {
            source_hash: "s".into(),
            toolchain_hash: toolchain.into(),
            flags_hash: flags.into(),
            header_hashes: BTreeMap::new(),
            lang: "c".into(),
        }
    }

    /// A verified step is served back only for the same flags, the same
    /// toolchain and an object that has not been touched since.
    #[test]
    fn a_verified_compile_is_recalled_only_while_nothing_it_depends_on_moved() {
        let memo = Memo::default();
        let dir = tempfile::tempdir().unwrap();
        let obj = dir.path().join("x.o");
        std::fs::write(&obj, b"object").unwrap();
        set_mtime(&obj, 1_000);
        let src = dir.path().join("x.c");
        std::fs::write(&src, b"int x;").unwrap();
        set_mtime(&src, 1_000);
        memo.remember_verified(&obj, &src, &sample_fingerprint("flags", "tc"));

        assert!(memo.recall_verified(&obj, "flags", "tc").is_some());
        assert!(
            memo.recall_verified(&obj, "other-flags", "tc").is_none(),
            "different flags are a different compile"
        );
        assert!(
            memo.recall_verified(&obj, "flags", "other-toolchain")
                .is_none(),
            "a different toolchain is a different compile"
        );

        // An edit to the source between two builds in one process: the case a
        // test, or any long-lived caller, hits.
        std::fs::write(&src, b"int xx;").unwrap();
        set_mtime(&src, 2_000);
        assert!(
            memo.recall_verified(&obj, "flags", "tc").is_none(),
            "an edited source must be compiled, not served as up to date"
        );
        set_mtime(&src, 1_000);
        std::fs::write(&src, b"int x;").unwrap();
        set_mtime(&src, 1_000);
        assert!(memo.recall_verified(&obj, "flags", "tc").is_some());

        std::fs::write(&obj, b"rebuilt").unwrap();
        set_mtime(&obj, 2_000);
        assert!(
            memo.recall_verified(&obj, "flags", "tc").is_none(),
            "an object rewritten since must be checked again"
        );
    }

    #[test]
    fn forgetting_drops_verified_compiles_and_an_absent_object_is_never_recalled() {
        let memo = Memo::default();
        let dir = tempfile::tempdir().unwrap();
        let obj = dir.path().join("y.o");
        std::fs::write(&obj, b"object").unwrap();
        set_mtime(&obj, 1_000);
        let src = dir.path().join("y.c");
        std::fs::write(&src, b"int y;").unwrap();
        set_mtime(&src, 1_000);
        memo.remember_verified(&obj, &src, &sample_fingerprint("f", "t"));
        assert!(memo.recall_verified(&obj, "f", "t").is_some());

        memo.forget();
        assert!(
            memo.recall_verified(&obj, "f", "t").is_none(),
            "a generator ran: nothing verified before it can be trusted"
        );

        assert!(memo
            .recall_verified(&dir.path().join("missing.o"), "f", "t")
            .is_none());
    }

    // ---- the per-process file memo -------------------------------------------

    // Each test owns a `Memo`: the process-wide one is cleared by any generator
    // that runs, including in other tests running in parallel.

    /// Pin a file's modification time so a test controls what the memo sees.
    fn set_mtime(path: &Path, secs: u64) {
        let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        file.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs))
            .unwrap();
    }

    #[test]
    fn the_memo_notices_an_edit_that_changes_the_timestamp() {
        let memo = Memo::default();
        let dir = tempfile::tempdir().unwrap();
        let header = dir.path().join("a.h");
        std::fs::write(&header, "int one;\n").unwrap();
        set_mtime(&header, 1_000);
        let before = memo.hash(&header).unwrap();
        assert_eq!(
            before,
            memo.hash(&header).unwrap(),
            "a second look is the same hash"
        );

        std::fs::write(&header, "int two;\n").unwrap(); // same length, new content
        set_mtime(&header, 2_000);
        assert_ne!(
            before,
            memo.hash(&header).unwrap(),
            "an edit that moved the modification time must not be served from the memo"
        );
    }

    /// The case the memo cannot see by itself, and the reason
    /// [`forget_file_facts`] exists: two writes of the same length inside one
    /// timestamp tick leave the stamp unchanged. Whatever runs generators calls
    /// `forget_file_facts` afterwards, and this is what that buys.
    #[test]
    fn forgetting_makes_an_equal_length_rewrite_within_one_tick_visible() {
        let memo = Memo::default();
        let dir = tempfile::tempdir().unwrap();
        let header = dir.path().join("b.h");
        std::fs::write(&header, "int one;\n").unwrap();
        set_mtime(&header, 1_000);
        let before = memo.hash(&header).unwrap();

        std::fs::write(&header, "int two;\n").unwrap();
        set_mtime(&header, 1_000); // the same tick
        assert_eq!(
            before,
            memo.hash(&header).unwrap(),
            "documents the hazard: an unchanged stamp is trusted"
        );

        memo.forget();
        assert_ne!(
            before,
            memo.hash(&header).unwrap(),
            "after forget_file_facts the rewrite must be seen"
        );
    }

    #[test]
    fn the_memo_serves_parsed_includes_and_drops_them_when_the_file_changes() {
        let memo = Memo::default();
        let dir = tempfile::tempdir().unwrap();
        let header = dir.path().join("c.h");
        std::fs::write(&header, "#include \"x.h\"\n#include <y.h>\n").unwrap();
        set_mtime(&header, 1_000);
        let first = memo.includes(&header).unwrap();
        assert_eq!(
            *first,
            vec![("x.h".to_string(), true), ("y.h".to_string(), false)]
        );

        std::fs::write(&header, "#include \"z.h\"\n").unwrap();
        set_mtime(&header, 2_000);
        assert_eq!(
            *memo.includes(&header).unwrap(),
            vec![("z.h".to_string(), true)]
        );
        assert!(memo.includes(&dir.path().join("missing.h")).is_none());
    }

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
    fn apple_environment_changes_the_hash_only_when_present() {
        use crate::builder::apple_env::AppleBuildEnv;
        let base = || {
            ToolchainFingerprint::new(
                "aarch64-apple-darwin",
                "clang",
                Path::new("/usr/bin/cc"),
                Path::new("/usr/bin/c++"),
                "21.0",
                None,
                "debug",
            )
        };
        let env = |target: &str, sdk: &str| AppleBuildEnv {
            deployment_target: target.to_string(),
            sdk: sdk.to_string(),
        };

        // Absent: not serialized and not hashed, so every non-Apple
        // fingerprint is what it was before the field existed.
        let none = base();
        assert!(!serde_json::to_string(&none).unwrap().contains("\"apple\":"));
        assert_eq!(none.hash(), base().with_apple(None).hash());
        let old: ToolchainFingerprint =
            serde_json::from_str(&serde_json::to_string(&none).unwrap()).unwrap();
        assert_eq!(old, none, "a record written without the field still loads");

        let a = base().with_apple(Some(env("macosx12.0.0", "/sdk@27.0")));
        let b = base().with_apple(Some(env("macosx15.0.0", "/sdk@27.0")));
        let c = base().with_apple(Some(env("macosx12.0.0", "/sdk@26.5")));
        assert_ne!(a.hash(), none.hash());
        assert_ne!(a.hash(), b.hash(), "deployment target must matter");
        assert_ne!(a.hash(), c.hash(), "SDK must matter");
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
