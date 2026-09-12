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
use crate::builder::toolchain::{CompileInput, Toolchain};
use crate::core::package_id::PackageId;
use crate::core::probe::{ProbeKind, ProbeSet};
use crate::core::surface::Define;
use crate::core::target::Language;
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

    /// How this reads in a report or a generated header.
    pub fn describe(self) -> String {
        match self {
            ProbeValue::Present => "yes".to_string(),
            ProbeValue::Absent => "no".to_string(),
            ProbeValue::Size(n) => n.to_string(),
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
}

impl ProbeResults {
    /// The defines these answers contribute, in declaration order.
    pub fn defines(&self) -> Vec<Define> {
        self.answers
            .iter()
            .filter_map(|(name, value)| value.to_define(name))
            .collect()
    }
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
pub fn answer_for_target(
    ctx: &crate::builder::BuildContext,
    pkg_id: &PackageId,
    target_name: &str,
    probes: &ProbeSet,
    compile_surface: &EffectiveCompileSurface,
) -> Result<ProbeResults> {
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
        scratch: probe_dir(ctx, pkg_id, target_name),
        toolchain_key: ctx.toolchain_fingerprint().hash(),
    };
    let label = format!("{}/{}", pkg_id.name(), target_name);
    run_probes(&env, probes, &label)
}

/// Hash the pre-probe compile surface.
///
/// Order-sensitive on purpose: `-I` is first-match-wins, so two orders are
/// genuinely two different questions.
fn surface_key(
    include_dirs: &[PathBuf],
    defines: &[(String, Option<String>)],
    target_cflags: &[String],
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

    let surface = surface_key(&env.include_dirs, &env.defines, &env.target_cflags);
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
            check_baseline(env, label)?;
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

    Ok(ProbeResults { answers, measured })
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

/// Compile the emptiest possible program with exactly the flags probes use.
///
/// If this fails, every probe would answer `false` and the package would
/// configure itself for a machine that does not exist. Fail here instead,
/// with the compiler's own output, which names the real cause.
fn check_baseline(env: &ProbeEnv<'_>, label: &str) -> Result<()> {
    let dir = env.scratch.join("baseline");
    let outcome = compile(env, &dir, "int main(void) { return 0; }\n", &[])?;
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
        output: object,
        include_dirs: env.include_dirs.clone(),
        defines: env.defines.clone(),
        cflags,
    };

    // Built by the same `Toolchain::compile_command` the real build uses, so
    // MSVC's `/c /Fo` and GCC's `-c -o` are both handled without this module
    // knowing which it is talking to, and a probe cannot be compiled by a
    // different argv builder than the package.
    let spec = env.toolchain.compile_command(&input, Language::C, None);

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
        .with_context(|| format!("probe compile could not be run: {command}"))?;

    // `code() == None` means the child was killed by a signal. That is not an
    // answer about the target; it is a broken or resource-starved machine.
    if output.status.code().is_none() {
        bail!(
            "probe compile was killed by a signal, so its result is not an \
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
        let base = surface_key(&dirs(&["a", "b"]), &[], &[]);

        // `-I` is first-match-wins, so two orders are two genuinely
        // different questions and must not share a cache entry.
        assert_ne!(base, surface_key(&dirs(&["b", "a"]), &[], &[]));

        // A dependency that starts exporting a new `-I` changes what
        // `HAVE_FOO_H` answers.
        assert_ne!(base, surface_key(&dirs(&["a", "b", "c"]), &[], &[]));

        // A define can gate a header's contents (`_GNU_SOURCE`).
        assert_ne!(
            base,
            surface_key(
                &dirs(&["a", "b"]),
                &[("_GNU_SOURCE".to_string(), None)],
                &[]
            )
        );

        // `--sysroot` decides which headers exist at all.
        assert_ne!(
            base,
            surface_key(&dirs(&["a", "b"]), &[], &["--sysroot=/x".to_string()])
        );
    }
}
