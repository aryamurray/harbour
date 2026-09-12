//! The probe engine: answers the questions declared in
//! [`crate::core::probe`] by compiling programs Harbour writes.
//!
//! Design document: `docs/superpowers/specs/2026-09-11-native-probes-design.md`.
//!
//! Two things about this module are load-bearing and easy to get wrong:
//!
//! 1. **Nothing is ever run.** Every answer comes from a compiler exit code.
//!    That is what makes probes work when cross-compiling, and it is why
//!    `sizeof` is obtained by bisecting a compile-time predicate rather than
//!    by printing a number from a test program.
//! 2. **A broken toolchain is an error, not a wall of `false`.** A compiler
//!    that cannot compile `int main(void){return 0;}` would make every probe
//!    answer false, and the package would then configure itself for a machine
//!    that does not exist and *compile*. That is the classic catastrophic
//!    configure failure, and the baseline check below is the whole defence
//!    against it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::builder::surface_resolver::EffectiveCompileSurface;
use crate::builder::toolchain::{CompileInput, LinkInput, Toolchain};
use crate::core::package_id::PackageId;
use crate::core::probe::{ProbeEmit, ProbeKind, ProbeSet};
use crate::core::surface::Define;
use crate::core::target::{CStandardSpec, Language, Target};
use crate::util::hash::Fingerprint;
use crate::util::process::ProcessBuilder;

/// The name of the probe cache file, inside the probe directory.
pub const PROBE_CACHE_FILE: &str = "probes.json";

/// The largest `sizeof` a probe will report.
///
/// 64 bytes covers every scalar and pointer type, and `long double` on every
/// platform. Bisecting `[0, 64]` costs 7 compiles. A struct larger than this
/// exists, but no `SIZEOF_*` in a real `config.h` asks for one -- and
/// reporting a wrong small number would be worse than failing, so exceeding
/// the bound is an error (see [`ProbeError::SizeOutOfRange`]).
const MAX_SIZEOF: u64 = 64;

/// A probe's answer.
///
/// `Absent` is a real answer, distinct from "could not be determined", which
/// is not representable here at all -- it is an `Err`. Collapsing the two is
/// the single most damaging mistake a configure system can make.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProbeValue {
    /// The thing exists.
    Present,
    /// The thing does not exist. A real answer.
    Absent,
    /// The size, in bytes.
    Size(u64),
}

impl ProbeValue {
    /// The define this answer contributes, if any.
    ///
    /// A false boolean probe emits **nothing**, not `NAME=0`, because C code
    /// writes `#ifdef HAVE_X` and `#define HAVE_X 0` would satisfy it. A
    /// `sizeof` always emits, because there is no "absent" size -- a type
    /// that does not exist is an error, not a zero.
    pub fn to_define(self, name: &str) -> Option<Define> {
        match self {
            ProbeValue::Present => Some(Define::key_value(name, "1")),
            ProbeValue::Absent => None,
            ProbeValue::Size(n) => Some(Define::key_value(name, n.to_string())),
        }
    }
}

/// Why a probe could not be answered at all.
#[derive(Debug)]
enum ProbeError {
    /// `sizeof(T)` exceeded [`MAX_SIZEOF`], or `T` does not exist.
    SizeOutOfRange { ty: String, stderr: String },
}

impl std::fmt::Display for ProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProbeError::SizeOutOfRange { ty, stderr } => write!(
                f,
                "could not determine `sizeof({ty})`: it is either larger than \
                 {MAX_SIZEOF} bytes or the type does not exist.\n\
                 hint: if the type comes from a header, name that header in \
                 this probe's `prelude`; a type that does not exist at all \
                 has no size, and asking for it is not the same question as \
                 asking whether it exists\n\
                 the compiler said:\n{stderr}"
            ),
        }
    }
}

/// Everything a probe needs in order to ask its question the same way the
/// real build will ask it.
pub struct ProbeEnv<'a> {
    /// The detected toolchain -- the same one that will compile the package.
    pub toolchain: &'a dyn Toolchain,

    /// Include directories from the target's resolved compile surface.
    ///
    /// Not optional for correctness: "does `zlib.h` exist" is meaningless
    /// without the `-I` a dependency contributes.
    pub include_dirs: Vec<PathBuf>,

    /// Defines from the target's resolved compile surface.
    pub defines: Vec<(String, Option<String>)>,

    /// Flags the *target triple* requires (`-target`, `-mcpu`, `--sysroot`).
    ///
    /// These are mandatory for a cross build -- `src/builder/context.rs`
    /// explains why at length -- so a probe without them is asking about the
    /// wrong machine.
    ///
    /// Deliberately **not** including the manifest's own `cflags` or the
    /// profile's. Two separate reasons:
    ///
    /// - A package with `-Werror` in its `cflags` (common) would make every
    ///   probe fail on an incidental warning, and every `HAVE_*` false. The
    ///   snippets below are written to be warning-free, but a package can
    ///   always add a `-W` that fires on something.
    /// - `-O2` and `-g` do not change whether a header exists, and including
    ///   them would make the debug and release builds keep two probe caches
    ///   with identical contents.
    ///
    /// The cost, stated because it is a real gap: a manifest that puts `-I`
    /// or `--sysroot` in `cflags` instead of `include_dirs` is invisible to
    /// probes. `MANIFEST.md` already tells authors not to do that, for an
    /// unrelated and equally good reason.
    pub target_cflags: Vec<String>,

    /// Link flags the *target triple* requires.
    ///
    /// Used only by `symbol` probes, which are the only kind that links.
    /// Present for the same reason `target_cflags` is: Apple's `-arch` has
    /// to appear on the link step as well as the compile step, and a cross
    /// link without a sysroot resolves against the host's libraries -- which
    /// would answer `yes` for a symbol the target does not have, the worst
    /// possible failure for this kind.
    pub target_ldflags: Vec<String>,

    /// The target's `c_std`, if it pinned one.
    ///
    /// Included, unlike the two categories above, and the test that decides
    /// it is the one this struct's own documentation already applies: does
    /// the flag change the *answer*?
    ///
    /// `-O2` and `-g` do not, so they are out. A dialect does, and not
    /// marginally: `-std=c99` defines `__STRICT_ANSI__`, at which point
    /// glibc's `features.h` stops defining `_DEFAULT_SOURCE`/`__USE_MISC`
    /// and Apple's headers drop to `__DARWIN_C_ANSI`. Whole families of
    /// declarations disappear. Measured under the compiler's default
    /// dialect, `sizeof(u_int)` answers 4 on this machine; measured under
    /// `-std=c99`, the type does not exist. A package pinning
    /// `c_std = "99"` and probed under `gnu*` would get a config header
    /// describing a translation unit it is not going to have.
    ///
    /// It also does not carry the `-Werror` hazard that keeps the package's
    /// own `cflags` out: this is one enumerated selector the compiler always
    /// accepts, not arbitrary author-supplied flags. What it *can* do is
    /// make a probe snippet stop compiling under a strict dialect, and that
    /// is why [`check_baseline`] exists: the failure is a hard error quoting
    /// the compiler, not a config full of `no`.
    ///
    /// Part of the cache key too (see [`surface_key`]). A dialect that
    /// reaches the compiler but not the key would mean editing `c_std` and
    /// getting yesterday's answers -- the one-field-two-consumers defect
    /// this subsystem was built to avoid.
    pub c_std: Option<CStandardSpec>,

    /// Directory for generated snippets, objects and the cache. Created if
    /// absent.
    pub scratch: PathBuf,

    /// `ToolchainFingerprint::hash()`, reused verbatim as the cache key's
    /// toolchain half so there is only one definition of "the toolchain
    /// changed".
    pub toolchain_key: String,
}

/// The cache file's on-disk form.
///
/// `BTreeMap` so the serialized JSON is byte-stable; the *result* order comes
/// from the [`ProbeSet`]'s declaration order, never from this map.
#[derive(Debug, Default, Serialize, Deserialize)]
struct ProbeCacheFile {
    /// `ToolchainFingerprint::hash()` when these answers were measured.
    toolchain_key: String,
    /// Hash of the pre-probe compile surface when these answers were measured.
    surface_key: String,
    /// name -> (spec hash, answer)
    probes: BTreeMap<String, CachedProbe>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedProbe {
    spec: String,
    value: ProbeValue,
}

/// The answers for one target, in the order the manifest declared them.
#[derive(Debug, Clone, Default)]
pub struct ProbeResults {
    /// `(name, answer)` in declaration order. A `Vec`, not a map, because
    /// order is the property that matters downstream and a map would invite
    /// someone to iterate it.
    pub answers: Vec<(String, ProbeValue)>,
    /// How many answers came from the compiler rather than the cache.
    pub measured: usize,

    /// When the target emits a generated header, the directory to put on
    /// its private include path.
    ///
    /// Returned rather than applied here, because applying it is the build
    /// plan's job and `harbour flags` needs the same value to report the
    /// same `-I`. One producer, two readers, no second derivation.
    pub include_dir: Option<PathBuf>,

    /// The generated header's path, for diagnostics.
    pub header: Option<PathBuf>,
}

impl ProbeResults {
    /// The defines these answers contribute, in declaration order.
    ///
    /// Not the thing to call directly -- use [`ProbeResults::contribution`],
    /// which is the only place `emit` is interpreted. Calling this on a
    /// target that emits a header would put every answer on the command line
    /// *as well as* in the file: two sources for one fact, and the exact
    /// shape of every defect the 2026-09-07 audit found.
    pub fn defines(&self) -> Vec<Define> {
        self.answers
            .iter()
            .filter_map(|(name, value)| value.to_define(name))
            .collect()
    }

    /// What this target's probes contribute to its compile surface.
    ///
    /// **The single interpretation of `emit`.** `BuildPlan` and
    /// `harbour flags` both call this and then apply the result to their own
    /// surface representation (plain, and attributed). The *decision* -- is
    /// this a define list or an include directory -- is made once, here, so
    /// the build and the command documented as authoritative about the build
    /// cannot disagree about it. They already did once, in the first probe
    /// PR, which is why this exists as a function rather than as a `match`
    /// in two files.
    pub fn contribution(&self, probes: &ProbeSet) -> ProbeContribution {
        match &probes.emit {
            ProbeEmit::Defines => ProbeContribution::Defines(self.defines()),
            ProbeEmit::Header { .. } => match &self.include_dir {
                Some(dir) => ProbeContribution::IncludeDir(dir.clone()),
                // Only reachable if the set is empty, in which case
                // `answer_for_target` returned before generating anything
                // and there is nothing to contribute.
                None => ProbeContribution::Defines(Vec::new()),
            },
        }
    }
}

/// What a target's probes add to its compile surface.
///
/// Deliberately an enum rather than a struct with two optional fields: the
/// two are alternatives, not a combination, and a struct would let a caller
/// apply both. Answers belong either on the command line or in the generated
/// header, never in both places.
#[derive(Debug, Clone)]
pub enum ProbeContribution {
    /// `-D` flags, in declaration order.
    Defines(Vec<Define>),
    /// A directory holding the generated header, for the private include
    /// path.
    IncludeDir(PathBuf),
}

/// The `-I` directory the generated header is written into.
///
/// A subdirectory of the probe directory rather than the probe directory
/// itself, because that one also holds `probes.json` and a `p<N>/` tree of
/// snippets and objects. Putting the header in its own directory means the
/// `-I` Harbour adds cannot make `probe.c` or `probes.json` reachable by
/// `#include`.
pub fn probe_include_dir(
    ctx: &crate::builder::BuildContext,
    pkg_id: &PackageId,
    target: &str,
) -> PathBuf {
    probe_dir(ctx, pkg_id, target).join("include")
}

/// Render the generated config header.
///
/// Byte-stable for a given (manifest, toolchain, target): the literal
/// defines first in declaration order, then the probed answers in
/// declaration order, and nothing iterated out of a hash map. That is a
/// requirement rather than a nicety -- the file's content enters the compile
/// fingerprint through `collect_header_deps`, so a line that moved between
/// runs would recompile every translation unit that includes it, on every
/// build, forever.
///
/// A false answer is written as a commented-out `#undef`, which is what
/// autoconf and CMake both produce. It is not decoration: it is the record
/// that the question was *asked and answered no*, which is the difference
/// between a probe subsystem and a header that forgot something. A reader
/// diffing this against a vendored `curl_config.h` sees the same shape.
fn render_header(
    header_name: &Path,
    label: &str,
    triple: &str,
    toolchain: &str,
    literals: &[Define],
    answers: &[(String, ProbeValue)],
) -> String {
    let guard = include_guard(header_name);
    let mut out = String::new();
    out.push_str("/* Generated by Harbour. Do not edit. */\n");
    out.push_str(&format!("/* target:    {label} */\n"));
    out.push_str(&format!("/* triple:    {triple} */\n"));
    out.push_str(&format!("/* toolchain: {toolchain} */\n"));
    out.push_str(&format!("#ifndef {guard}\n#define {guard}\n"));

    if !literals.is_empty() {
        out.push_str("\n/* Declared in Harbour.toml, not measured. */\n");
        for d in literals {
            match d.value() {
                Some(v) => out.push_str(&format!("#define {} {}\n", d.name(), v)),
                None => out.push_str(&format!("#define {} 1\n", d.name())),
            }
        }
    }

    if !answers.is_empty() {
        out.push_str("\n/* Measured from the toolchain. */\n");
        for (name, value) in answers {
            match value {
                ProbeValue::Present => out.push_str(&format!("#define {name} 1\n")),
                ProbeValue::Size(n) => out.push_str(&format!("#define {name} {n}\n")),
                // Asked, and the answer was no. Recorded rather than
                // omitted, so the file distinguishes "no" from "never
                // asked".
                ProbeValue::Absent => out.push_str(&format!("/* #undef {name} */\n")),
            }
        }
    }

    out.push_str(&format!("\n#endif /* {guard} */\n"));
    out
}

/// `curl_config.h` -> `HARBOUR_PROBE_CURL_CONFIG_H`.
///
/// Prefixed, because the obvious guard (`CURL_CONFIG_H`) is one a package's
/// own vendored copy may already define -- and a header whose guard is
/// already defined expands to nothing at all, which is a build that fails on
/// missing macros with no mention of this file.
fn include_guard(header_name: &Path) -> String {
    let mut out = String::from("HARBOUR_PROBE_");
    for c in header_name.to_string_lossy().chars() {
        out.push(if c.is_ascii_alphanumeric() {
            c.to_ascii_uppercase()
        } else {
            '_'
        });
    }
    out
}

/// Write the generated header, returning the directory to put on the
/// include path.
///
/// Written unconditionally, but only *touched* when the content differs.
/// That matters more than it looks: the file's bytes are a compile
/// fingerprint input, and rewriting identical content would be harmless
/// while rewriting it with a different byte anywhere recompiles everything
/// that includes it. Comparing first also means `harbour flags` and
/// `harbour build --plan`, which both run probes, do not perturb the build
/// tree.
fn write_header(dir: &Path, header_name: &Path, contents: &str) -> Result<PathBuf> {
    let path = dir.join(header_name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create the generated-header directory {}",
                parent.display()
            )
        })?;
    }
    let unchanged = std::fs::read_to_string(&path)
        .map(|existing| existing == contents)
        .unwrap_or(false);
    if !unchanged {
        std::fs::write(&path, contents)
            .with_context(|| format!("failed to write the generated header {}", path.display()))?;
    }
    Ok(path)
}

/// Where a (package, target)'s probe snippets and cache live.
///
/// Derived only from things every caller already has, rather than from the
/// `target_output_dir` the build plan computes. `harbour flags` must arrive
/// at the *same* directory as `harbour build` or the two would keep separate
/// caches and could disagree, which is the defect class that gave the
/// 2026-09-07 audit its §2.4 -- `harbour flags` reporting flags the build did
/// not use.
///
/// Three properties of the path are deliberate:
///
/// - Under `ctx.output_dir`, which is already per-triple and per-profile, so
///   two triples built from one checkout cannot stomp each other's answers.
///   That is the `SIZEOF_LONG 8` on 32-bit Linux bug, prevented structurally
///   rather than remembered.
/// - Keyed on name *and version*, because two versions of one package in a
///   graph are two sets of answers.
/// - Per target, because `curl_config.h` is a name two targets can want.
pub fn probe_dir(ctx: &crate::builder::BuildContext, pkg_id: &PackageId, target: &str) -> PathBuf {
    ctx.output_dir
        .join("probe")
        .join(format!("{}-{}", pkg_id.name(), pkg_id.version()))
        .join(target)
}

/// Answer a target's probes against its resolved compile surface.
///
/// **The single entry point.** `harbour build` (via `BuildPlan`) and
/// `harbour flags` both call this, and neither assembles a [`ProbeEnv`]
/// itself. That is not tidiness: the audit's headline finding was that every
/// one of its ten defects was one field with two independent consumers that
/// had drifted, and "what flags does this file get" already had three
/// implementations in this codebase. A probe subsystem with two is a probe
/// subsystem whose `harbour flags` output is fiction.
///
/// Returns an empty result, touching no disk and spawning no compiler, when
/// the target declares no probes.
///
/// Takes the [`Target`] rather than its name and probe set. It used to take
/// `target_name`, and then `c_std` had to be plumbed in: with a name there
/// is no way for this function to reach the dialect the package compiles
/// in, and a caller that has to pass it separately is a caller that can
/// forget. The whole point of one entry point is that there is nothing left
/// for two callers to disagree about.
pub fn answer_for_target(
    ctx: &crate::builder::BuildContext,
    pkg_id: &PackageId,
    target: &Target,
    compile_surface: &EffectiveCompileSurface,
) -> Result<ProbeResults> {
    let probes = &target.probes;
    if probes.is_empty() {
        return Ok(ProbeResults::default());
    }
    let env = ProbeEnv {
        toolchain: ctx.toolchain(),
        include_dirs: compile_surface.include_dirs.clone(),
        defines: compile_surface
            .defines
            .iter()
            .map(|d| (d.name().to_string(), d.value().map(|v| v.to_string())))
            .collect(),
        target_cflags: ctx.target_cflags.clone(),
        target_ldflags: ctx.target_ldflags.clone(),
        c_std: target.c_std,
        scratch: probe_dir(ctx, pkg_id, target.name.as_str()),
        toolchain_key: ctx.toolchain_fingerprint().hash(),
    };
    let label = format!("{}/{}", pkg_id.name(), target.name);
    let mut results = run_probes(&env, probes, &label)?;

    if let ProbeEmit::Header { header } = &probes.emit {
        // Rendered from the same `results` the defines path would use, so
        // the header and a `-D` list can never disagree about an answer.
        let contents = render_header(
            header,
            &label,
            &ctx.target.canonical(),
            &ctx.compiler.to_string(),
            &probes.defines,
            &results.answers,
        );
        let dir = probe_include_dir(ctx, pkg_id, target.name.as_str());
        let path = write_header(&dir, header, &contents)?;
        tracing::debug!(
            target = %label,
            header = %path.display(),
            answers = results.answers.len(),
            "probe config header generated"
        );
        results.include_dir = Some(dir);
        results.header = Some(path);
    }

    Ok(results)
}

/// Hash the pre-probe compile surface.
///
/// Order-sensitive on purpose: `-I` is first-match-wins, so two orders are
/// genuinely two different questions.
fn surface_key(
    include_dirs: &[PathBuf],
    defines: &[(String, Option<String>)],
    target_cflags: &[String],
    c_std: Option<CStandardSpec>,
) -> String {
    let mut fp = Fingerprint::new();
    for dir in include_dirs {
        // `to_string_lossy` rather than a byte-exact encoding: the same
        // choice `ToolchainFingerprint` already makes for paths, and a path
        // that is not valid UTF-8 would not survive the command line either.
        fp.update_str(&dir.to_string_lossy());
    }
    for (name, value) in defines {
        fp.update_str(name);
        fp.update_opt(value.as_deref());
    }
    for flag in target_cflags {
        fp.update_str(flag);
    }
    // The dialect the answers were measured in. `gnu99` and `99` are two
    // different questions about the same machine.
    fp.update_opt(c_std.map(|s| s.as_flag_value()));
    fp.finish_short()
}

/// Hash one probe's spec, so editing one probe in a 400-probe manifest
/// re-runs one probe.
fn spec_key(kind: &ProbeKind) -> String {
    let mut fp = Fingerprint::new();
    fp.update_str(kind.kind_name());
    match kind {
        ProbeKind::Header { header, prelude } => {
            fp.update_str(header);
            for p in prelude {
                fp.update_str(p);
            }
        }
        ProbeKind::Symbol {
            symbol,
            prelude,
            libs,
        } => {
            fp.update_str(symbol);
            for p in prelude {
                fp.update_str(p);
            }
            // `libs` is part of the question, not an implementation detail:
            // "does `dlopen` resolve" and "does `dlopen` resolve with
            // `-ldl`" have different answers on glibc, and serving one from
            // the other's cache entry would be wrong.
            for l in libs {
                fp.update_str(l);
            }
        }
        ProbeKind::Type {
            ty,
            member,
            prelude,
        } => {
            fp.update_str(ty);
            // `member` is part of the question: "does `struct sockaddr_in6`
            // exist" and "does it have `sin6_scope_id`" are two questions
            // with two answers, and serving one from the other's cache entry
            // is how adding a `member` to an existing probe would silently
            // keep yesterday's `yes`.
            fp.update_opt(member.as_deref());
            for p in prelude {
                fp.update_str(p);
            }
        }
        ProbeKind::Constant { constant, prelude } => {
            fp.update_str(constant);
            for p in prelude {
                fp.update_str(p);
            }
        }
        ProbeKind::Flag { flag } => {
            fp.update_str(flag);
        }
        ProbeKind::Sizeof { ty, prelude } => {
            fp.update_str(ty);
            for p in prelude {
                fp.update_str(p);
            }
        }
    }
    fp.finish_short()
}

/// Answer every probe in `set`, using the cache where it is valid.
///
/// Returns answers in the set's declaration order.
pub fn run_probes(env: &ProbeEnv<'_>, set: &ProbeSet, label: &str) -> Result<ProbeResults> {
    if set.is_empty() {
        return Ok(ProbeResults::default());
    }

    std::fs::create_dir_all(&env.scratch).with_context(|| {
        format!(
            "failed to create the probe directory {}",
            env.scratch.display()
        )
    })?;

    let surface = surface_key(
        &env.include_dirs,
        &env.defines,
        &env.target_cflags,
        env.c_std,
    );
    let cache_path = env.scratch.join(PROBE_CACHE_FILE);
    let mut cache = load_cache(&cache_path, &env.toolchain_key, &surface);

    // Only pay for the baseline if at least one probe must actually be
    // measured. A fully warm cache should cost no compiler spawns at all.
    let mut baseline_checked = false;

    let mut answers = Vec::with_capacity(set.probes.len());
    let mut measured = 0usize;

    for (index, (name, kind)) in set.probes.iter().enumerate() {
        let spec = spec_key(kind);
        if let Some(hit) = cache.probes.get(name) {
            if hit.spec == spec {
                answers.push((name.clone(), hit.value));
                continue;
            }
        }

        if !baseline_checked {
            check_baseline(env, label, set.needs_linker())?;
            baseline_checked = true;
        }

        let dir = env.scratch.join(format!("p{index}"));
        let value = answer(env, &dir, name, kind)?;
        measured += 1;
        cache
            .probes
            .insert(name.clone(), CachedProbe { spec, value });
        answers.push((name.clone(), value));
    }

    // Drop answers for probes the manifest no longer declares, so the file
    // cannot accumulate two generations of a renamed probe.
    let live: std::collections::BTreeSet<&String> = set.probes.keys().collect();
    cache.probes.retain(|name, _| live.contains(name));
    cache.toolchain_key = env.toolchain_key.clone();
    cache.surface_key = surface;
    save_cache(&cache_path, &cache)?;

    Ok(ProbeResults {
        answers,
        measured,
        include_dir: None,
        header: None,
    })
}

/// Load the cache, discarding it wholesale if either key differs.
///
/// Wholesale, never merged: a cache holding answers measured under two
/// different toolchains is the shape of the bug fixed in `e860627`, where two
/// spellings of "canonical" put two entries per artifact in the fingerprint
/// cache and every rebuild missed. A probe cache with two generations in it
/// would instead produce a *wrong `#define`*, which is worse.
fn load_cache(path: &Path, toolchain_key: &str, surface: &str) -> ProbeCacheFile {
    let fresh = || ProbeCacheFile {
        toolchain_key: toolchain_key.to_string(),
        surface_key: surface.to_string(),
        probes: BTreeMap::new(),
    };

    let Ok(text) = std::fs::read_to_string(path) else {
        return fresh();
    };
    // A corrupt cache is not an error: it is a cache. Re-measure.
    let Ok(cached) = serde_json::from_str::<ProbeCacheFile>(&text) else {
        return fresh();
    };
    if cached.toolchain_key != toolchain_key || cached.surface_key != surface {
        return fresh();
    }
    cached
}

fn save_cache(path: &Path, cache: &ProbeCacheFile) -> Result<()> {
    let json = serde_json::to_string_pretty(cache)?;
    std::fs::write(path, json)
        .with_context(|| format!("failed to write the probe cache {}", path.display()))
}

/// Compile -- and, when any probe needs a linker, link -- the emptiest
/// possible program with exactly the flags probes use.
///
/// This one check is worth more than the rest of the error handling
/// combined. "Every probe is `no` because the compiler is broken, the
/// sysroot is missing, or a `--sysroot` in `target_cflags` is wrong" is
/// `configure`'s most common catastrophic mode, and it produces a build that
/// *succeeds* with a config describing a machine that does not exist.
///
/// The link half exists for `symbol` probes, and it is the reason
/// [`ProbeSet::needs_linker`] exists rather than this always linking. The
/// two failures are genuinely different and a package should only be held to
/// the one it depends on:
///
/// - A cross toolchain that can compile but not link is common and usable --
///   there is no sysroot with libraries in it, or no cross linker on
///   `PATH`. A package whose probes are all `header` and `sizeof` is
///   perfectly answerable there, and refusing it would be wrong.
/// - The same toolchain cannot answer a single `symbol` probe. Letting it
///   try would make every `HAVE_<function>` false, which for curl is 107 of
///   its 157 real questions -- a config that says the platform has no
///   sockets, no `poll`, and no `strerror_r`, and which then compiles.
///
/// So: link only when something links, and when it must link and cannot,
/// stop with the linker's own diagnostics attached. The alternatives were
/// considered and rejected in the design document -- answering `false` is
/// the disaster case, and silently degrading `symbol` to a compile-only
/// check answers a different question under the same name.
fn check_baseline(env: &ProbeEnv<'_>, label: &str, needs_linker: bool) -> Result<()> {
    const EMPTY: &str = "int main(void) { return 0; }\n";

    let dir = env.scratch.join("baseline");
    let outcome = compile(env, &dir, EMPTY, &[])?;
    if !outcome.ok {
        bail!(
            "the probe baseline failed for {label}: the toolchain cannot compile \
             an empty program with the flags probes use, so every probe would \
             report `no` and the package would be configured for a machine that \
             does not exist.\n\
             command: {}\n\
             the compiler said:\n{}",
            outcome.command,
            outcome.stderr
        );
    }

    if needs_linker {
        let dir = env.scratch.join("baseline-link");
        let outcome = compile_and_link(env, &dir, EMPTY, &[])?;
        if !outcome.ok {
            bail!(
                "the probe link baseline failed for {label}: the toolchain can \
                 compile but cannot link an empty program, and this target has \
                 `symbol` probes, which have to link to mean anything.\n\
                 Every `symbol` probe would report `no`, and a package \
                 configured as though the platform had none of the functions it \
                 asked about would still compile -- so this is an error rather \
                 than an answer.\n\
                 hint: cross-compiling needs a sysroot with libraries in it, not \
                 just a cross compiler. `header` and `sizeof` probes need only \
                 the compiler and are unaffected.\n\
                 command: {}\n\
                 the linker said:\n{}",
                outcome.command,
                outcome.stderr
            );
        }
    }
    Ok(())
}

fn answer(env: &ProbeEnv<'_>, dir: &Path, name: &str, kind: &ProbeKind) -> Result<ProbeValue> {
    match kind {
        ProbeKind::Header { header, prelude } => {
            let src = header_snippet(header, prelude);
            let outcome = compile(env, dir, &src, &[])?;
            tracing::debug!(
                probe = name,
                header = header.as_str(),
                answer = outcome.ok,
                "header probe"
            );
            Ok(if outcome.ok {
                ProbeValue::Present
            } else {
                ProbeValue::Absent
            })
        }
        ProbeKind::Symbol {
            symbol,
            prelude,
            libs,
        } => {
            let src = symbol_snippet(symbol, prelude);
            let outcome = compile_and_link(env, dir, &src, libs)?;
            tracing::debug!(
                probe = name,
                symbol = symbol.as_str(),
                answer = outcome.ok,
                "symbol probe"
            );
            Ok(if outcome.ok {
                ProbeValue::Present
            } else {
                ProbeValue::Absent
            })
        }
        ProbeKind::Type {
            ty,
            member,
            prelude,
        } => {
            let src = type_snippet(ty, member.as_deref(), prelude);
            let outcome = compile(env, dir, &src, &[])?;
            tracing::debug!(
                probe = name,
                r#type = ty.as_str(),
                member = member.as_deref(),
                answer = outcome.ok,
                "type probe"
            );
            Ok(if outcome.ok {
                ProbeValue::Present
            } else {
                ProbeValue::Absent
            })
        }
        ProbeKind::Constant { constant, prelude } => {
            let src = constant_snippet(constant, prelude);
            let outcome = compile(env, dir, &src, &[])?;
            tracing::debug!(
                probe = name,
                constant = constant.as_str(),
                answer = outcome.ok,
                "constant probe"
            );
            Ok(if outcome.ok {
                ProbeValue::Present
            } else {
                ProbeValue::Absent
            })
        }
        ProbeKind::Flag { flag } => {
            let mut cflags = flag_guard_flags(env.toolchain.platform());
            cflags.push(effective_flag(env.toolchain.platform(), flag));
            let outcome = compile(env, dir, FLAG_SNIPPET, &cflags)?;
            tracing::debug!(
                probe = name,
                flag = flag.as_str(),
                probed_as = cflags.last().map(String::as_str),
                answer = outcome.ok,
                "flag probe"
            );
            Ok(if outcome.ok {
                ProbeValue::Present
            } else {
                ProbeValue::Absent
            })
        }
        ProbeKind::Sizeof { ty, prelude } => {
            let size = bisect_sizeof(env, dir, ty, prelude)?;
            tracing::debug!(probe = name, r#type = ty.as_str(), size, "sizeof probe");
            Ok(ProbeValue::Size(size))
        }
    }
}

/// `#include` the prerequisites, then the header under test.
fn header_snippet(header: &str, prelude: &[String]) -> String {
    let mut src = String::new();
    for p in prelude {
        src.push_str(&format!("#include <{p}>\n"));
    }
    src.push_str(&format!("#include <{header}>\n"));
    // No unused variables, no implicit return: the snippet must be
    // warning-free, because a package's own `-W` flags could otherwise turn
    // an incidental warning into a `HAVE_*` of `no`.
    src.push_str("int main(void) { return 0; }\n");
    src
}

/// Reference `symbol` so the linker has to resolve it.
///
/// Four cases have to work, and the `#if defined` is what makes the first
/// one work at all:
///
/// 1. **The symbol is a macro.** Measured, not hypothesised: on macOS
///    `htonl` is a macro and `<arpa/inet.h>` declares no function of that
///    name at all, so `&htonl` does not compile. Without this branch
///    `HAVE_HTONL` answers `no` on every Mac for something the package can
///    call perfectly well -- verified by deleting the branch and watching
///    the answer flip. A macro also needs nothing linked, which is why this
///    returns immediately rather than falling through to a link.
/// 2. **The symbol is declared and provided.** The address is taken and the
///    link resolves it. The ordinary case.
/// 3. **The symbol is provided but *not* declared.** `fdatasync` on macOS
///    is exactly this: it links, and `<unistd.h>` does not declare it. With
///    no `prelude` the fallback declaration finds it and the answer is
///    `yes`; with `prelude = ["unistd.h"]` the compile fails and the answer
///    is `no`. **Both are correct**, because they are different questions --
///    "can I call this if I declare it myself" and "can I call this the way
///    the header offers it". That is why `prelude` is part of the cache key
///    rather than an implementation detail.
/// 4. **The symbol is declared and *not* provided.** The classic
///    `configure` trap, and the reason this kind links while `header` does
///    not: a compile-only check answers `yes` and the package then fails at
///    link time on a symbol in a file nobody wrote. Handled by construction
///    -- the compile succeeds, the link fails, the answer is `no`. Not
///    reproduced on either platform tested here, so it is claimed as a
///    property of the mechanism rather than as a measurement.
///
/// With no `prelude` a fallback declaration is emitted instead of relying on
/// a header -- the autoconf trick for checking a symbol whose real prototype
/// you do not know. Taking the address of a mis-declared function still
/// forces the linker to resolve the name, which is the question being asked.
/// With a `prelude`, the header's own declaration is used, so the probe asks
/// about the symbol *as the package will see it*.
///
/// Two details that look like fussiness and are not:
///
/// - The reference goes through a `volatile` pointer. Without it the
///   compiler may fold `&name != 0` to `1` and never emit a relocation, at
///   which point the link succeeds for a symbol that does not exist. A
///   false `yes` is the worst answer this subsystem can give.
/// - It is cast to `const void *` rather than to a function-pointer type,
///   so a symbol that turns out to be a *variable* (`environ`,
///   `sys_errlist`) works through the same snippet. ISO C calls
///   function-pointer-to-`void *` conditionally supported; every platform
///   Harbour targets supports it, because POSIX `dlsym` requires it, and it
///   is diagnosed only under `-Wpedantic`, which probes do not pass.
fn symbol_snippet(symbol: &str, prelude: &[String]) -> String {
    let mut src = String::new();
    for p in prelude {
        src.push_str(&format!("#include <{p}>\n"));
    }
    if prelude.is_empty() {
        src.push_str(
            "/* No declaring header was given, so declare it here. The\n\
             prototype is deliberately not the real one: taking the address\n\
             still makes the linker resolve the name. */\n",
        );
        src.push_str(&format!("char {symbol}(void);\n"));
    }
    src.push_str("int main(void) {\n");
    src.push_str(&format!("#if defined({symbol})\n"));
    src.push_str("    /* A macro. Usable, and nothing to link. */\n");
    src.push_str("    return 0;\n");
    src.push_str("#else\n");
    src.push_str("    static const void *volatile probe_ref;\n");
    src.push_str(&format!("    probe_ref = (const void *) &{symbol};\n"));
    src.push_str("    return probe_ref == 0;\n");
    src.push_str("#endif\n");
    src.push_str("}\n");
    src
}

/// Declare a variable of the type, so the compiler has to have a complete
/// definition of it.
///
/// ```c
/// #include <sys/time.h>
/// int main(void) { struct timeval probe_value; (void) sizeof(probe_value); return 0; }
/// ```
///
/// Declaring a variable rather than writing `sizeof(struct timeval)` is not
/// a stylistic choice. `sizeof` a type name is the same test, but the
/// declaration form is what makes the `member` case work with one snippet
/// instead of two, and `sizeof` on an incomplete type is an error either
/// way -- which is the answer wanted: a forward-declared `struct foo;` with
/// no definition in scope is not a type you can use.
///
/// With a `member` it becomes the standard "has this field" idiom:
///
/// ```c
/// int main(void) { struct sockaddr_in6 probe_value; (void) sizeof(probe_value.sin6_scope_id); return 0; }
/// ```
///
/// which correctly answers **no** for a type that exists *without* the
/// member -- the property that makes `member` worth having rather than
/// being a decoration on a type check. That distinction has a test
/// (`a_type_probe_with_a_member_rejects_a_type_that_lacks_it`); without it,
/// `member` could stop reaching the snippet and every member probe would
/// answer `yes`.
///
/// One honest limitation: `sizeof` is not applicable to a bit-field, so a
/// `member` naming one answers `no` for a field that is really there. No
/// package on the roadmap asks about a bit-field, and the alternative
/// (`&probe_value.member`) fails on bit-fields too, so this is recorded
/// rather than worked around.
fn type_snippet(ty: &str, member: Option<&str>, prelude: &[String]) -> String {
    let mut src = String::new();
    for p in prelude {
        src.push_str(&format!("#include <{p}>\n"));
    }
    src.push_str("int main(void) {\n");
    src.push_str(&format!("    {ty} probe_value;\n"));
    match member {
        Some(m) => src.push_str(&format!("    (void) sizeof(probe_value.{m});\n")),
        None => src.push_str("    (void) sizeof(probe_value);\n"),
    }
    src.push_str("    return 0;\n}\n");
    src
}

/// Use the constant where **only an integer constant expression** is legal.
///
/// ```c
/// #include <fcntl.h>
/// int main(void) {
///     enum { harbour_probe_constant = (int) (O_NONBLOCK) };
///     (void) harbour_probe_constant;
///     return 0;
/// }
/// ```
///
/// An enumerator's initialiser must be an integer constant expression (C,
/// §6.7.2.2), and that requirement is the entire point of the snippet
/// rather than an implementation detail:
///
/// - A **macro** expanding to an integer constant passes. That is the
///   common case: `O_NONBLOCK`, `FIONBIO`, `SO_NONBLOCK`.
/// - An **enumerator** passes. This is the case that rules out reusing the
///   `symbol` kind: `CLOCK_MONOTONIC` is a macro on Linux and an
///   enumeration constant on macOS, and `symbol`'s `#if defined(...)`
///   branch only sees the first.
/// - A **function or variable** of that name does **not** pass, because
///   neither is a constant expression. That is what keeps `constant` from
///   quietly becoming a compile-only `symbol` check, and it is what
///   `a_constant_probe_says_no_to_a_function_of_the_same_name` pins.
///
/// The `(int)` cast is there so a constant of an unsigned or wider type
/// (`FIONBIO` is an `unsigned long` on the platforms that have it) is still
/// an integer constant expression of type `int`, and so that the snippet
/// does not depend on the constant's own type fitting an enumerator. A cast
/// is explicit, so no truncation warning fires.
///
/// What this deliberately does not answer: whether a *non-integer* constant
/// exists -- a string macro, a floating-point limit. Those are not what
/// `config.h` checks ask about, and the kind is named for the question it
/// answers rather than being widened until it answers nothing precisely.
fn constant_snippet(constant: &str, prelude: &[String]) -> String {
    let mut src = String::new();
    for p in prelude {
        src.push_str(&format!("#include <{p}>\n"));
    }
    src.push_str("int main(void) {\n");
    src.push_str(
        "    /* An enumerator's initialiser must be an integer constant\n\
         \x20      expression, so a name that is merely declared -- a function,\n\
         \x20      a variable -- does not pass here. That is what distinguishes\n\
         \x20      this kind from `symbol`. */\n",
    );
    src.push_str(&format!(
        "    enum {{ harbour_probe_constant = (int) ({constant}) }};\n"
    ));
    src.push_str("    (void) harbour_probe_constant;\n");
    src.push_str("    return 0;\n}\n");
    src
}

/// What a `flag` probe compiles. The flag is the question; the source is
/// only there so the compiler has something to do.
const FLAG_SNIPPET: &str = "int main(void) { return 0; }\n";

/// The flags that make "unknown option" fatal for this compiler family.
///
/// **Without these the `flag` kind answers `yes` to everything**, which is
/// the single failure mode it has. Each family accepts flags it does not
/// understand, in its own way:
///
/// - **clang / apple-clang** treat an unknown *warning* flag as a warning
///   (`-Wunknown-warning-option`) and an unused one as another
///   (`-Wunused-command-line-argument`). Promoting exactly those two to
///   errors is enough, and is narrower than a blanket `-Werror`, which
///   would make an unrelated warning in the empty program -- there are none
///   today, but a future flag could introduce one -- look like a rejected
///   flag.
/// - **GCC** errors on an unknown `-f`/`--param` by itself, so `-Werror`
///   plus the rewrite in [`effective_flag`] covers it. `-Werror` is needed
///   because GCC reports an unrecognised `-W` option as a *warning* in some
///   versions.
/// - **MSVC** emits `D9002: ignoring unknown option` as a warning and exits
///   0, so `/WX` is what makes it fatal. **Unverified: written from the
///   design document, which itself flags every MSVC claim in it as
///   unverified for want of a Windows host.**
///
/// These guards are on the **`flag` kind's compile alone** and deliberately
/// not on `header`, `symbol` or `sizeof`. That is the same boundary
/// `ProbeEnv`'s documentation draws when it keeps the package's own `cflags`
/// out: a `-Werror` anywhere near the other three kinds makes them answer
/// `no` on an incidental warning, which is the catastrophic direction. The
/// MSVC probe spike (`docs/superpowers/specs/2026-09-12-msvc-probes-spike.md`,
/// "What is explicitly not in the plan") asks for exactly this scoping and
/// gives a concrete reason: `symbol_snippet` casts a function pointer to
/// `const void *`, which `cl` is expected to diagnose as `C4054`, so a
/// blanket `/WX` would make every `symbol` probe answer `no` on Windows.
fn flag_guard_flags(platform: crate::builder::toolchain::ToolchainPlatform) -> Vec<String> {
    use crate::builder::toolchain::ToolchainPlatform as P;
    match platform {
        P::Clang | P::AppleClang => vec![
            "-Werror=unknown-warning-option".to_string(),
            "-Werror=unused-command-line-argument".to_string(),
        ],
        P::Gcc => vec!["-Werror".to_string()],
        P::Msvc => vec!["/WX".to_string()],
    }
}

/// The flag to actually put on the probe's command line, which is not always
/// the flag being asked about.
///
/// **This is the `-Wno-*`-under-GCC wrinkle, and it is fixable rather than
/// merely documentable.** The design document, following
/// `AX_CHECK_COMPILE_FLAG`'s documented experience, records it as a known
/// limitation of the kind: GCC accepts *any* `-Wno-whatever` silently and
/// only diagnoses it if some other diagnostic fires, so `-Werror` does not
/// help and a probe for `-Wno-nonsense` answers `yes`.
///
/// The asymmetry is the way out. GCC is silent about an unknown
/// `-Wno-<name>` and *loud* about an unknown `-W<name>`, and the two names
/// come from the same table -- GCC has no warning it can disable but not
/// enable. So the probe asks about the positive form and reports the answer
/// for the negative one. `-Wno-nonsense` becomes `-Wnonsense`, which GCC
/// rejects; `-Wno-unused` becomes `-Wunused`, which it accepts. This is the
/// same trick CMake's `check_c_compiler_flag` documentation tells its users
/// to perform by hand, done once here instead.
///
/// Measured, not reasoned: `a_gcc_wno_flag_probe_is_not_fooled_by_gccs_silence`
/// runs both spellings under real GCC in the Linux container, and it fails
/// if this rewrite is removed.
///
/// Only GCC. clang diagnoses `-Wno-nonsense` directly under
/// `-Werror=unknown-warning-option`, so rewriting there would substitute a
/// different question for one that is already answerable.
fn effective_flag(platform: crate::builder::toolchain::ToolchainPlatform, flag: &str) -> String {
    use crate::builder::toolchain::ToolchainPlatform as P;
    if platform == P::Gcc {
        if let Some(rest) = flag.strip_prefix("-Wno-") {
            return format!("-W{rest}");
        }
    }
    flag.to_string()
}

/// `sizeof(T)` without running anything, by bisecting a compile-time
/// predicate.
///
/// The predicate is a negative array bound:
///
/// ```c
/// char probe[(sizeof(long) <= 8) ? 1 : -1];
/// ```
///
/// which is ill-formed iff `sizeof(long) > 8`. It is monotone in the bound,
/// so binary search over `[0, MAX_SIZEOF]` finds the exact value in
/// `log2(MAX_SIZEOF) + 1` compiles.
///
/// Why the negative array and not the obvious alternatives:
///
/// - **Running a program that prints the number** is correct and useless when
///   cross-compiling. That is the property this whole subsystem exists to
///   avoid.
/// - **Parsing the compiler's error message** makes the build depend on
///   compiler *prose*, which differs per vendor and changes per release.
/// - **`_Static_assert`** reads better but is C11. Harbour supports
///   `c_std = "89"`, and the negative array means the same thing in C89, C23
///   and C++ alike.
/// - **Encoding the value in the object file and reading it out** (what
///   CMake's `check_type_size` does) is one compile instead of seven and is
///   genuinely better, but it needs a per-format object reader. It is the
///   intended follow-up; this is cached after its first run, so the seven
///   compiles are paid once.
fn bisect_sizeof(env: &ProbeEnv<'_>, dir: &Path, ty: &str, prelude: &[String]) -> Result<u64> {
    // `sizeof` is never 0, so the predicate is false at 0 by definition and
    // that end of the invariant needs no compile.
    let mut lo = 0u64;
    let mut hi = MAX_SIZEOF;

    let top = fits(env, dir, ty, prelude, hi)?;
    if !top.0 {
        return Err(anyhow::anyhow!(ProbeError::SizeOutOfRange {
            ty: ty.to_string(),
            stderr: top.1,
        }));
    }

    // Invariant: predicate(lo) is false, predicate(hi) is true.
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if fits(env, dir, ty, prelude, mid)?.0 {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    Ok(hi)
}

/// Does `sizeof(ty) <= bound` hold? Returns the answer and, when it does not
/// hold, the compiler's output, so the out-of-range error can quote it.
fn fits(
    env: &ProbeEnv<'_>,
    dir: &Path,
    ty: &str,
    prelude: &[String],
    bound: u64,
) -> Result<(bool, String)> {
    let mut src = sizeof_preamble();
    for header in prelude {
        src.push_str(&format!("#include <{header}>\n"));
    }
    src.push_str(&format!(
        "int main(void) {{\n\
         \x20   char probe[(sizeof({ty}) <= {bound}) ? 1 : -1];\n\
         \x20   (void) probe;\n\
         \x20   return 0;\n\
         }}\n"
    ));
    let outcome = compile(env, &dir.join(format!("le{bound}")), &src, &[])?;
    Ok((outcome.ok, outcome.stderr))
}

/// The headers a `sizeof` probe gets for free.
///
/// The first run of this subsystem asked for `sizeof(time_t)` with only
/// `<stddef.h>` included and failed with "use of undeclared identifier
/// 'time_t'". `SIZEOF_TIME_T` and `SIZEOF_OFF_T` are two of curl's seven
/// `SIZEOF_*` values, so a `sizeof` probe that can only see `<stddef.h>` is
/// not useful for the package this subsystem exists for. The design document
/// said `prelude` was rejected on `sizeof` probes; running it proved that
/// wrong within a minute.
///
/// Each header past `<stddef.h>` is guarded by `__has_include`, so a target
/// that lacks one (freestanding, or an unusual libc) gets a probe that still
/// answers rather than an error about a prerequisite it never asked for.
/// `__has_include` is C23, but GCC 5+, clang and MSVC 2017+ all support it as
/// an extension, and the `#ifdef __has_include` guard means a compiler
/// without it simply gets `<stddef.h>` -- degrading to the previous
/// behaviour, not to a failure.
///
/// `<stddef.h>` is unguarded because a freestanding implementation is
/// required to provide it (C, section 4).
fn sizeof_preamble() -> String {
    let mut src = String::from("#include <stddef.h>\n#ifdef __has_include\n");
    for header in ["stdint.h", "time.h", "sys/types.h"] {
        src.push_str(&format!(
            "#  if __has_include(<{header}>)\n#    include <{header}>\n#  endif\n"
        ));
    }
    src.push_str("#endif\n");
    src
}

/// The result of one probe compile.
struct Outcome {
    ok: bool,
    stderr: String,
    command: String,
}

/// Write `src` into `dir` and compile it, returning whether the compiler
/// exited 0.
///
/// Three outcomes, and the third one is why this returns `Result<Outcome>`
/// rather than `Result<bool>` or `bool`:
///
/// - exit 0 -> `ok: true`
/// - non-zero exit -> `ok: false`, a **real answer**
/// - could not spawn, or died on a signal -> `Err`, **not an answer**
///
/// Conflating the last two is how a broken toolchain becomes a config full of
/// `no`.
fn compile(env: &ProbeEnv<'_>, dir: &Path, src: &str, extra_cflags: &[String]) -> Result<Outcome> {
    let (outcome, _) = compile_to_object(env, dir, src, extra_cflags)?;
    Ok(outcome)
}

/// Compile `src` in `dir`, returning the outcome and where the object went.
fn compile_to_object(
    env: &ProbeEnv<'_>,
    dir: &Path,
    src: &str,
    extra_cflags: &[String],
) -> Result<(Outcome, PathBuf)> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("failed to create probe directory {}", dir.display()))?;

    // A directory per probe, so parallel probes cannot race over one path.
    // This repo has already shipped that bug once: `7fbdcc7`, where MSVC
    // detection raced itself over a shared temp file, corrupted archives and
    // forced full rebuilds.
    let source = dir.join("probe.c");
    let object = dir.join(format!("probe.{}", env.toolchain.object_extension()));
    std::fs::write(&source, src)
        .with_context(|| format!("failed to write probe source {}", source.display()))?;

    let mut cflags = env.target_cflags.clone();
    cflags.extend(extra_cflags.iter().cloned());

    let input = CompileInput {
        source: source.clone(),
        output: object.clone(),
        include_dirs: env.include_dirs.clone(),
        defines: env.defines.clone(),
        cflags,
        // The package's dialect, so a probe is answered about the
        // translation unit the package is actually going to have. See
        // `ProbeEnv::c_std`.
        c_std: env.c_std,
    };

    // Built by the same `Toolchain::compile_command` the real build uses, so
    // MSVC's `/c /Fo` and GCC's `-c -o` are both handled without this module
    // knowing which it is talking to, and a probe cannot be compiled by a
    // different argv builder than the package.
    let spec = env.toolchain.compile_command(&input, Language::C, None);
    let outcome = run(spec, "compile")?;
    Ok((outcome, object))
}

/// Compile `src` and then **link** it into an executable, with `libs` on the
/// link line. The executable is never run.
///
/// A `symbol` probe has to link, and that is the whole reason the kind
/// exists separately from `header`: a header that *declares* something the
/// libc does not *provide* is the classic `configure` trap. `fdatasync` is
/// declared on macOS and not implemented; a compile-only check answers `yes`
/// and the package then fails to link, at which point the error names a
/// symbol in a file nobody wrote.
///
/// Reported as a single outcome: a compile failure and a link failure are
/// both "no, you cannot call this", and distinguishing them would invite a
/// caller to treat one of them as a different answer. What is *not* folded
/// in is a failure to run the tools at all -- `run` still errors for that.
fn compile_and_link(env: &ProbeEnv<'_>, dir: &Path, src: &str, libs: &[String]) -> Result<Outcome> {
    let (compiled, object) = compile_to_object(env, dir, src, &[])?;
    if !compiled.ok {
        return Ok(compiled);
    }

    let exe = dir.join(format!("probe{}", env.toolchain.exe_extension()));
    let input = LinkInput {
        objects: vec![object],
        output: exe,
        lib_dirs: Vec::new(),
        libs: libs.to_vec(),
        // The target's own link flags, for the same reason the compile gets
        // `target_cflags`: Apple's `-arch` has to be on both steps, and a
        // cross link without `--sysroot` finds the host's libraries. The
        // profile's `ldflags` are excluded on the same grounds as its
        // cflags -- they do not change whether a symbol resolves.
        ldflags: env.target_ldflags.clone(),
        frameworks: Vec::new(),
    };
    let spec = env.toolchain.link_exe_command(&input, Language::C, None);
    run(spec, "link")
}

/// Run a probe command, distinguishing "answered no" from "could not ask".
///
/// Three outcomes, and the third is why this returns `Result<Outcome>`
/// rather than `bool`:
///
/// - exit 0 -> `ok: true`
/// - non-zero exit -> `ok: false`, a **real answer**
/// - could not spawn, or died on a signal -> `Err`, **not an answer**
///
/// Conflating the last two is how a broken toolchain becomes a config full
/// of `no`.
fn run(spec: crate::builder::toolchain::CommandSpec, phase: &str) -> Result<Outcome> {
    let mut cmd = ProcessBuilder::new(&spec.program);
    for arg in &spec.args {
        cmd = cmd.arg(arg);
    }
    for (key, value) in &spec.env {
        cmd = cmd.env(key, value);
    }

    let command = cmd.display_command();
    let output = cmd
        .exec()
        .with_context(|| format!("probe {phase} could not be run: {command}"))?;

    // `code() == None` means the child was killed by a signal. That is not an
    // answer about the target; it is a broken or resource-starved machine.
    if output.status.code().is_none() {
        bail!(
            "probe {phase} was killed by a signal, so its result is not an \
             answer about the target.\ncommand: {command}"
        );
    }

    Ok(Outcome {
        ok: output.status.success(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        command,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_false_boolean_probe_emits_no_define_at_all() {
        // `#define HAVE_X 0` satisfies `#ifdef HAVE_X`, which is what every
        // real config header tests. Emitting it would invert the answer.
        assert!(ProbeValue::Absent.to_define("HAVE_X").is_none());
        assert_eq!(
            ProbeValue::Present
                .to_define("HAVE_X")
                .expect("present emits")
                .to_flag(),
            "-DHAVE_X=1"
        );
    }

    #[test]
    fn a_sizeof_probe_always_emits_its_value() {
        assert_eq!(
            ProbeValue::Size(8)
                .to_define("SIZEOF_LONG")
                .expect("sizes always emit")
                .to_flag(),
            "-DSIZEOF_LONG=8"
        );
    }

    #[test]
    fn results_produce_defines_in_declaration_order_skipping_absent_ones() {
        let results = ProbeResults {
            answers: vec![
                ("HAVE_B".to_string(), ProbeValue::Present),
                ("HAVE_A".to_string(), ProbeValue::Absent),
                ("SIZEOF_LONG".to_string(), ProbeValue::Size(8)),
            ],
            measured: 3,
            include_dir: None,
            header: None,
        };
        let flags: Vec<String> = results.defines().iter().map(|d| d.to_flag()).collect();
        // B before A: declaration order, not sorted. Sorting build output is
        // what `ac859c1` removed.
        assert_eq!(flags, vec!["-DHAVE_B=1", "-DSIZEOF_LONG=8"]);
    }

    #[test]
    fn the_spec_key_changes_with_every_part_of_the_spec() {
        let a = spec_key(&ProbeKind::Header {
            header: "poll.h".into(),
            prelude: vec![],
        });
        let b = spec_key(&ProbeKind::Header {
            header: "sys/poll.h".into(),
            prelude: vec![],
        });
        let c = spec_key(&ProbeKind::Header {
            header: "poll.h".into(),
            prelude: vec!["sys/types.h".into()],
        });
        let d = spec_key(&ProbeKind::Sizeof {
            ty: "poll.h".into(),
            prelude: vec![],
        });
        let e = spec_key(&ProbeKind::Sizeof {
            ty: "time_t".into(),
            prelude: vec!["time.h".into()],
        });
        let f = spec_key(&ProbeKind::Sizeof {
            ty: "time_t".into(),
            prelude: vec![],
        });
        assert_ne!(a, b, "the header must be in the key");
        assert_ne!(a, c, "the prelude must be in the key");
        assert_ne!(a, d, "the kind must be in the key");
        assert_ne!(
            e, f,
            "a `sizeof` probe's prelude must be in the key: it decides whether \
             the type is visible at all, so two preludes are two questions"
        );
    }

    #[test]
    fn the_spec_key_separates_the_three_new_kinds_from_each_other() {
        // `HAVE_FOO` asked as a type, a constant and a flag are three
        // different questions, and the cache is keyed by name. Without the
        // kind in the key, changing `type = "X"` to `constant = "X"` would
        // be served yesterday's answer.
        let t = spec_key(&ProbeKind::Type {
            ty: "X".into(),
            member: None,
            prelude: vec![],
        });
        let t_member = spec_key(&ProbeKind::Type {
            ty: "X".into(),
            member: Some("m".into()),
            prelude: vec![],
        });
        let t_prelude = spec_key(&ProbeKind::Type {
            ty: "X".into(),
            member: None,
            prelude: vec!["h.h".into()],
        });
        let c = spec_key(&ProbeKind::Constant {
            constant: "X".into(),
            prelude: vec![],
        });
        let f = spec_key(&ProbeKind::Flag { flag: "X".into() });
        assert_ne!(t, c, "a type and a constant of one name are two questions");
        assert_ne!(t, f);
        assert_ne!(c, f);
        assert_ne!(
            t, t_member,
            "adding a `member` must re-run the probe: `struct sockaddr_in6` \
             existing and having `sin6_scope_id` are two answers"
        );
        assert_ne!(
            t, t_prelude,
            "the prelude decides whether the type is visible"
        );
        assert_ne!(
            f,
            spec_key(&ProbeKind::Flag { flag: "Y".into() }),
            "the flag must be in the key"
        );
    }

    #[test]
    fn the_type_snippet_declares_a_variable_and_uses_the_member_when_given() {
        let plain = type_snippet("struct timeval", None, &["sys/time.h".to_string()]);
        assert!(plain.contains("#include <sys/time.h>"), "{plain}");
        assert!(plain.contains("struct timeval probe_value;"), "{plain}");
        assert!(plain.contains("sizeof(probe_value)"), "{plain}");

        let with_member = type_snippet("struct sockaddr_in6", Some("sin6_scope_id"), &[]);
        // The member has to reach the snippet, or every member probe
        // degrades to a plain type check and answers `yes` for a type that
        // lacks the field. That degradation is the whole hazard of this
        // field, so it is asserted on the generated text as well as on the
        // behaviour (see the integration test).
        assert!(
            with_member.contains("sizeof(probe_value.sin6_scope_id)"),
            "{with_member}"
        );
    }

    #[test]
    fn the_constant_snippet_demands_an_integer_constant_expression() {
        let src = constant_snippet("O_NONBLOCK", &["fcntl.h".to_string()]);
        assert!(src.contains("#include <fcntl.h>"), "{src}");
        // An enumerator's initialiser. This is the load-bearing detail: a
        // function or a variable of the same name is *not* a constant
        // expression, which is what keeps this kind from silently becoming a
        // compile-only `symbol` check.
        assert!(src.contains("enum {"), "{src}");
        assert!(src.contains("(int) (O_NONBLOCK)"), "{src}");
        // And not a plain expression statement, which a function name would
        // satisfy.
        assert!(
            !src.contains("(void) (O_NONBLOCK)"),
            "a bare expression statement would accept a function name: {src}"
        );
    }

    #[test]
    fn the_flag_guards_are_per_family_because_one_families_guards_break_another() {
        use crate::builder::toolchain::ToolchainPlatform as P;
        // Measured, in the Linux container: `gcc -Werror=unknown-warning-option`
        // fails with "no option '-Wunknown-warning-option'" -- so handing
        // clang's guards to GCC would make *every* flag probe answer `no`,
        // and handing GCC's `-Werror` to clang would not catch
        // `-Wno-nonsense` on its own. The guards must be keyed on the
        // family, and this test is what stops them being unified.
        assert!(flag_guard_flags(P::Clang).contains(&"-Werror=unknown-warning-option".to_string()));
        assert_eq!(flag_guard_flags(P::AppleClang), flag_guard_flags(P::Clang));
        assert_eq!(flag_guard_flags(P::Gcc), vec!["-Werror".to_string()]);
        assert_eq!(flag_guard_flags(P::Msvc), vec!["/WX".to_string()]);
        // No family gets an empty guard list. An empty list is the "answers
        // yes to everything" configuration.
        for p in [P::Clang, P::AppleClang, P::Gcc, P::Msvc] {
            assert!(
                !flag_guard_flags(p).is_empty(),
                "{p:?} must have something making an unknown flag fatal"
            );
        }
    }

    #[test]
    fn a_gcc_wno_flag_is_probed_by_its_positive_spelling() {
        use crate::builder::toolchain::ToolchainPlatform as P;
        // GCC accepts *any* `-Wno-whatever` silently and only diagnoses it
        // when some other diagnostic fires, so `-Werror` does not help.
        // Measured under GCC 13 in the container:
        //   gcc -Werror -Wno-harbour-nonsense  -> accepted   (the trap)
        //   gcc -Werror -Wharbour-nonsense     -> rejected   (the fix)
        // Removing this rewrite makes `HAVE_FLAG_WNO_NONSENSE` true under
        // GCC, which is the design document's documented limitation of the
        // kind -- and it turns out to be fixable rather than merely
        // documentable.
        assert_eq!(effective_flag(P::Gcc, "-Wno-unused"), "-Wunused");
        assert_eq!(
            effective_flag(P::Gcc, "-Wno-error=unused"),
            "-Werror=unused",
            "GCC diagnoses an unknown `-Werror=X` immediately too"
        );
        // Anything that is not a `-Wno-` flag is asked about as written.
        assert_eq!(effective_flag(P::Gcc, "-pthread"), "-pthread");
        assert_eq!(
            effective_flag(P::Gcc, "-fno-strict-aliasing"),
            "-fno-strict-aliasing"
        );
        // Only GCC. clang diagnoses `-Wno-nonsense` directly under
        // `-Werror=unknown-warning-option`, so rewriting there would
        // substitute a different question for one already answerable.
        for p in [P::Clang, P::AppleClang, P::Msvc] {
            assert_eq!(
                effective_flag(p, "-Wno-unused"),
                "-Wno-unused",
                "{p:?} must be asked about the flag the manifest named"
            );
        }
    }

    #[test]
    fn the_rendered_header_records_no_answers_as_commented_undefs() {
        let out = render_header(
            Path::new("curl_config.h"),
            "curl/curl",
            "aarch64-apple-darwin",
            "apple-clang-21.0",
            &[
                Define::key_value("CURL_DISABLE_LDAP", "1"),
                Define::flag("CURL_STATICLIB"),
            ],
            &[
                ("HAVE_SYS_SOCKET_H".to_string(), ProbeValue::Present),
                ("HAVE_WINDOWS_H".to_string(), ProbeValue::Absent),
                ("SIZEOF_LONG".to_string(), ProbeValue::Size(8)),
            ],
        );

        // A false answer is a commented `#undef`, matching autoconf and
        // CMake. It records that the question was asked and answered no,
        // which is what makes this file auditable against a vendored one.
        assert!(out.contains("/* #undef HAVE_WINDOWS_H */"), "{out}");
        // And never `=0`, which `#ifdef` accepts.
        assert!(!out.contains("HAVE_WINDOWS_H 0"), "{out}");

        assert!(out.contains("#define HAVE_SYS_SOCKET_H 1"), "{out}");
        assert!(out.contains("#define SIZEOF_LONG 8"), "{out}");
        // A value-less literal becomes 1, matching `-DFOO`.
        assert!(out.contains("#define CURL_STATICLIB 1"), "{out}");
        assert!(out.contains("#define CURL_DISABLE_LDAP 1"), "{out}");

        // Literals come first, so a probed answer cannot be shadowed by a
        // declared one arriving later in the same file.
        let literal = out.find("CURL_DISABLE_LDAP").expect("literal");
        let measured = out.find("HAVE_SYS_SOCKET_H").expect("measured");
        assert!(
            literal < measured,
            "declared values must precede measured ones:\n{out}"
        );

        // Provenance, so a reader of a generated file knows what produced it
        // and for which target. This is also what makes two triples' headers
        // visibly different rather than mysteriously different.
        assert!(out.contains("aarch64-apple-darwin"), "{out}");
        assert!(out.contains("apple-clang-21.0"), "{out}");
        assert!(out.contains("Do not edit"), "{out}");
    }

    #[test]
    fn the_include_guard_is_prefixed_so_it_cannot_collide() {
        // `CURL_CONFIG_H` is a guard curl's own vendored copy may already
        // define, and a header whose guard is already defined expands to
        // *nothing* -- a build that then fails on missing macros without
        // ever mentioning this file.
        assert_eq!(
            include_guard(Path::new("curl_config.h")),
            "HARBOUR_PROBE_CURL_CONFIG_H"
        );
        assert_eq!(
            include_guard(Path::new("lib/config-mac.h")),
            "HARBOUR_PROBE_LIB_CONFIG_MAC_H"
        );
    }

    #[test]
    fn rendering_is_a_pure_function_of_its_inputs() {
        let render = || {
            render_header(
                Path::new("c.h"),
                "p/t",
                "x",
                "y",
                &[Define::flag("A")],
                &[
                    ("B".to_string(), ProbeValue::Present),
                    ("C".to_string(), ProbeValue::Absent),
                ],
            )
        };
        assert_eq!(render(), render(), "the renderer must be deterministic");

        // And order-sensitive, so the caller's declaration order is what
        // lands in the file rather than something the renderer chose.
        let swapped = render_header(
            Path::new("c.h"),
            "p/t",
            "x",
            "y",
            &[Define::flag("A")],
            &[
                ("C".to_string(), ProbeValue::Absent),
                ("B".to_string(), ProbeValue::Present),
            ],
        );
        assert_ne!(render(), swapped);
    }

    #[test]
    fn a_rewrite_with_identical_content_leaves_the_file_alone() {
        // The file's bytes are a compile fingerprint input, and probes run
        // during planning -- which means `harbour flags` and
        // `harbour build --plan` regenerate it too. Touching it when nothing
        // changed would make an inspection command perturb the build tree.
        let dir = tempfile::tempdir().expect("tempdir");
        let name = Path::new("gen.h");
        let path = write_header(dir.path(), name, "first\n").expect("write");
        let first = std::fs::metadata(&path)
            .expect("meta")
            .modified()
            .expect("mtime");

        let again = write_header(dir.path(), name, "first\n").expect("rewrite");
        assert_eq!(path, again);
        assert_eq!(
            std::fs::metadata(&again)
                .expect("meta")
                .modified()
                .expect("mtime"),
            first,
            "identical content must not rewrite the file"
        );

        // Different content must land.
        write_header(dir.path(), name, "second\n").expect("write");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "second\n");
    }

    #[test]
    fn a_nested_header_name_creates_its_parent_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path =
            write_header(dir.path(), Path::new("lib/inner/gen.h"), "x\n").expect("nested write");
        assert!(path.is_file(), "{}", path.display());
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "x\n");
    }

    #[test]
    fn a_cache_from_a_different_toolchain_is_discarded_wholesale() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(PROBE_CACHE_FILE);
        let mut probes = BTreeMap::new();
        probes.insert(
            "HAVE_POLL_H".to_string(),
            CachedProbe {
                spec: "abc".to_string(),
                value: ProbeValue::Present,
            },
        );
        save_cache(
            &path,
            &ProbeCacheFile {
                toolchain_key: "tc-old".to_string(),
                surface_key: "sf".to_string(),
                probes,
            },
        )
        .expect("save");

        // Same toolchain and surface: the entry survives.
        assert_eq!(load_cache(&path, "tc-old", "sf").probes.len(), 1);
        // Toolchain changed: nothing survives. This is the stale-answer bug
        // the design calls out -- a stale `HAVE_X` is a wrong `#define`, not
        // merely a stale object.
        assert!(load_cache(&path, "tc-new", "sf").probes.is_empty());
        // Surface changed (a dependency started exporting a new `-I`, so
        // `HAVE_FOO_H` may now answer differently): nothing survives.
        assert!(load_cache(&path, "tc-old", "sf2").probes.is_empty());
    }

    #[test]
    fn a_corrupt_cache_is_re_measured_rather_than_fatal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(PROBE_CACHE_FILE);
        std::fs::write(&path, "{ not json").expect("write");
        assert!(load_cache(&path, "tc", "sf").probes.is_empty());
    }

    #[test]
    fn the_surface_key_is_order_sensitive_and_covers_every_part() {
        let dirs = |v: &[&str]| v.iter().map(PathBuf::from).collect::<Vec<_>>();
        let base = surface_key(&dirs(&["a", "b"]), &[], &[], None);

        // `-I` is first-match-wins, so two orders are two genuinely
        // different questions and must not share a cache entry.
        assert_ne!(base, surface_key(&dirs(&["b", "a"]), &[], &[], None));

        // A dependency that starts exporting a new `-I` changes what
        // `HAVE_FOO_H` answers.
        assert_ne!(base, surface_key(&dirs(&["a", "b", "c"]), &[], &[], None));

        // A define can gate a header's contents (`_GNU_SOURCE`).
        assert_ne!(
            base,
            surface_key(
                &dirs(&["a", "b"]),
                &[("_GNU_SOURCE".to_string(), None)],
                &[],
                None
            )
        );

        // `--sysroot` decides which headers exist at all.
        assert_ne!(
            base,
            surface_key(&dirs(&["a", "b"]), &[], &["--sysroot=/x".to_string()], None)
        );

        // And the dialect, which decides which declarations the libc
        // headers expose at all. Answers measured under `gnu99` must not be
        // served to a build compiling under `99`: `__STRICT_ANSI__` hides
        // whole families of types, so the two are different questions about
        // one machine. Without this, editing `c_std` would silently reuse
        // the previous dialect's answers.
        let gnu99 = surface_key(
            &dirs(&["a", "b"]),
            &[],
            &[],
            Some(CStandardSpec::gnu(crate::core::target::CStandard::C99)),
        );
        let c99 = surface_key(
            &dirs(&["a", "b"]),
            &[],
            &[],
            Some(CStandardSpec::iso(crate::core::target::CStandard::C99)),
        );
        assert_ne!(base, gnu99, "pinning a dialect is a change");
        assert_ne!(base, c99);
        assert_ne!(gnu99, c99, "`gnu99` and `99` are not the same question");
    }
}
