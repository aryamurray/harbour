//! CLI integration tests for Harbour.
//!
//! These tests verify the full CLI workflow from project creation through building.
//!
//! ## Hermeticity
//!
//! These tests must never touch the network or the developer's real home
//! directory / global Harbour cache:
//!
//! - Every invocation of `harbour` goes through [`harbour`], which points
//!   `HOME` / the XDG base directories at a per-test temporary directory
//!   (see [`harbour_home`]), so nothing is ever read from or written to the
//!   real `~/.harbour` cache.
//! - Any test that needs `harbour add` to resolve a registry dependency
//!   points it at a local, git-backed fixture registry (see
//!   `harbour::test_support::fixtures::local_registry`) via the
//!   `HARBOUR_TEST_REGISTRY_URL` environment variable, instead of the real
//!   `https://github.com/aryamurray/harbour-registry`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::prelude::*;
use predicates::prelude::*;
use tempfile::TempDir;

use harbour::test_support::fixtures::local_registry;

/// Get the harbour binary command, isolated from the developer's real home
/// directory / global Harbour cache and from any ambient vcpkg/registry
/// configuration.
///
/// `home` should be a directory inside the test's own [`TempDir`] (see
/// [`harbour_home`]) so that nothing `harbour` does ever escapes the test's
/// temp directory, and so that parallel test runs cannot interfere with
/// each other via a shared cache.
fn harbour(home: &std::path::Path) -> Command {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("harbour"));
    cmd
        // Unix / macOS: `directories` resolves the cache/config dirs from
        // these.
        .env("HOME", home)
        .env("XDG_CACHE_HOME", home.join("xdg-cache"))
        .env("XDG_CONFIG_HOME", home.join("xdg-config"))
        .env("XDG_DATA_HOME", home.join("xdg-data"))
        // Windows: best-effort isolation (the `directories` crate mostly
        // resolves special folders via the OS rather than these env vars,
        // so this is not a complete guarantee on that platform).
        .env("APPDATA", home.join("AppData/Roaming"))
        .env("LOCALAPPDATA", home.join("AppData/Local"))
        // Make sure no ambient vcpkg installation on the host leaks into
        // the test and changes `harbour add`'s fallback behavior.
        .env_remove("VCPKG_ROOT")
        .env_remove("HARBOUR_TEST_REGISTRY_URL");
    cmd
}

/// Create a temporary directory for test projects.
fn temp_dir() -> TempDir {
    TempDir::new().unwrap()
}

/// Derive an isolated "home" directory from a test's temp dir, used to keep
/// `harbour`'s global cache/config out of the developer's real home
/// directory (see [`harbour`]).
fn harbour_home(tmp: &TempDir) -> PathBuf {
    let home = tmp.path().join(".harbour-home");
    fs::create_dir_all(&home).unwrap();
    home
}

// ============================================================================
// Shared harness
//
// Three lessons from bugs that shipped are baked in here, because each one
// escaped a test suite that looked like it covered the area:
//
// 1. Every one of those bugs produced a *successful build with wrong
//    output*, so `.assert().success()` could never have caught any of them.
//    Anything touching compile, archive or link behaviour has to run the
//    artifact and assert on what it prints -- see [`run_built_exe`] --
//    or inspect the artifact itself -- see [`archive_members`].
//
// 2. Some bugs are invisible on a single build. Sources produced by a
//    `prebuild` step are missed on a clean build and picked up on the
//    second, and a fingerprint that fails to invalidate only shows up when
//    you build, change something, and build again. [`build_twice`] and
//    [`rebuild_and_diff`] make that the cheap thing to do.
//
// 3. Failures have to be self-describing. Diagnosing the Windows archive
//    bug took three attempts: the first captured only stderr, and the
//    second could not tell "never recompiled" from "recompiled but not
//    relinked". What worked was diffing the build tree across the rebuild,
//    so [`RunLog`] keeps both streams and [`TreeDiff`] renders exactly
//    which artifacts moved.
// ============================================================================

/// A finished process, with **both** output streams retained.
///
/// Keeping only one stream is what made the first Windows diagnosis useless.
/// Harbour spreads its build narration across both: the per-file decisions
/// (`Compiling N file(s)`, `All N file(s) up to date`) are `tracing` records
/// on stderr, `--message-format json` writes to stdout, and a compiler's own
/// diagnostics can land on either. Assertion helpers below always render the
/// whole thing.
#[derive(Debug)]
struct RunLog {
    what: String,
    status: std::process::ExitStatus,
    stdout: String,
    stderr: String,
}

impl RunLog {
    /// Run `cmd` to completion, labelling it `what` in failure messages.
    fn capture(what: impl Into<String>, cmd: &mut Command) -> RunLog {
        let what = what.into();
        let out = cmd
            .output()
            .unwrap_or_else(|e| panic!("failed to spawn {what}: {e}"));
        RunLog {
            what,
            status: out.status,
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    /// Both streams concatenated, for substring checks that should not care
    /// which one a message went to.
    fn combined(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }

    /// The process's stdout with surrounding whitespace removed -- the usual
    /// form for "what did the built program print".
    fn out(&self) -> &str {
        self.stdout.trim()
    }

    /// Assert the process exited zero, reporting both streams if not.
    fn success(self) -> RunLog {
        assert!(
            self.status.success(),
            "expected success but the process failed\n{self}"
        );
        self
    }
}

impl std::fmt::Display for RunLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "$ {}\n  status: {}\n  --- stdout ---\n{}\n  --- stderr ---\n{}\n  --------------",
            self.what, self.status, self.stdout, self.stderr
        )
    }
}

/// `<dir>/.harbour/target` -- the tree [`snapshot_tree`] watches.
fn target_dir(dir: &std::path::Path) -> PathBuf {
    dir.join(".harbour").join("target")
}

/// Run `harbour <args>` in `dir` and capture the result without asserting.
fn harbour_run(home: &std::path::Path, dir: &std::path::Path, args: &[&str]) -> RunLog {
    RunLog::capture(
        format!("harbour {}", args.join(" ")),
        harbour(home).args(args).current_dir(dir),
    )
}

/// As [`harbour_run`], with extra environment variables -- used to change
/// the compiler between two builds of the same tree on purpose.
fn harbour_run_env(
    home: &std::path::Path,
    dir: &std::path::Path,
    args: &[&str],
    env: &[(&str, &str)],
) -> RunLog {
    let mut cmd = harbour(home);
    cmd.args(args).current_dir(dir);
    for (k, v) in env {
        cmd.env(k, v);
    }
    RunLog::capture(
        format!(
            "{} harbour {}",
            env.iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(" "),
            args.join(" ")
        ),
        &mut cmd,
    )
}

/// `harbour build` in `dir`, asserting it succeeded.
fn build_ok(home: &std::path::Path, dir: &std::path::Path) -> RunLog {
    harbour_run(home, dir, &["build"]).success()
}

/// Run a binary Harbour just built and assert the binary itself ran.
///
/// The failure message names the artifact and shows its output, so
/// "the program crashed" and "the program printed the wrong number" are
/// never confused with "the build failed".
fn run_built_exe(app_dir: &std::path::Path, name: &str) -> RunLog {
    run_built_exe_in(app_dir, "debug", name)
}

/// As [`run_built_exe`], for a named profile (`debug` / `release`).
fn run_built_exe_in(app_dir: &std::path::Path, profile: &str, name: &str) -> RunLog {
    let exe = built_exe_path_in(app_dir, profile, name);
    assert!(
        exe.exists(),
        "expected a built executable at {}, but nothing is there; \
         the build reported success without producing an artifact",
        exe.display()
    );
    RunLog::capture(exe.display().to_string(), &mut Command::new(&exe)).success()
}

/// Every file under `dir` with its length and modification time.
///
/// Used to tell "the object was recompiled" from "the object was reused"
/// without relying on log output: log lines change wording, are suppressed
/// by `--quiet`, and split across two streams, whereas an object file that
/// did not move is unambiguous.
type TreeSnapshot = std::collections::BTreeMap<PathBuf, (u64, std::time::SystemTime)>;

fn snapshot_tree(dir: &std::path::Path) -> TreeSnapshot {
    let mut out = TreeSnapshot::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            match entry.metadata() {
                Ok(m) if m.is_dir() => stack.push(path),
                Ok(m) => {
                    let mtime = m.modified().unwrap_or(std::time::UNIX_EPOCH);
                    out.insert(path, (m.len(), mtime));
                }
                Err(_) => {}
            }
        }
    }
    out
}

/// What a build did to the build tree, as paths relative to it.
///
/// This is the discriminator the Windows investigation needed and did not
/// have: "the object never changed" (a fingerprint that failed to
/// invalidate) and "the object changed but the executable did not" (a
/// relink that never happened) are different bugs with different fixes, and
/// an assertion on the program's output alone cannot tell them apart.
#[derive(Debug, Default)]
struct TreeDiff {
    created: Vec<String>,
    modified: Vec<String>,
    removed: Vec<String>,
    unchanged: Vec<String>,
    /// The tree the snapshots were taken of, kept so a failure can read the
    /// fingerprint cache and say *why* a rebuild was not a no-op.
    root: PathBuf,
}

/// Compare two [`snapshot_tree`] results taken around a build.
fn describe_artifact_changes(
    root: &std::path::Path,
    before: &TreeSnapshot,
    after: &TreeSnapshot,
) -> TreeDiff {
    let rel = |p: &PathBuf| {
        p.strip_prefix(root)
            .unwrap_or(p)
            .display()
            .to_string()
            .replace('\\', "/")
    };
    let mut diff = TreeDiff {
        root: root.to_path_buf(),
        ..Default::default()
    };
    for (path, stat) in after {
        match before.get(path) {
            Some(old) if old == stat => diff.unchanged.push(rel(path)),
            Some(_) => diff.modified.push(rel(path)),
            None => diff.created.push(rel(path)),
        }
    }
    for path in before.keys() {
        if !after.contains_key(path) {
            diff.removed.push(rel(path));
        }
    }
    diff
}

/// Files Harbour rewrites on every build whether or not it did any work --
/// the fingerprint database, `compile_commands.json` and the like.
///
/// They are still shown in failure output, because "the fingerprint file
/// did not change" is itself a useful clue, but they are excluded from
/// "did this build redo work", which is a question about artifacts. Without
/// this an incremental build can never look like a no-op and the freshness
/// assertion would be untestable.
fn is_bookkeeping(path: &str) -> bool {
    path.rsplit('/')
        .next()
        .map(|f| f.starts_with('.') || f == "compile_commands.json")
        .unwrap_or(false)
}

impl TreeDiff {
    /// Artifacts the build created or rewrote, bookkeeping excluded.
    fn touched(&self) -> impl Iterator<Item = &str> {
        self.created
            .iter()
            .chain(self.modified.iter())
            .map(String::as_str)
            .filter(|p| !is_bookkeeping(p))
    }

    fn touched_any(&self, needle: &str) -> bool {
        self.touched().any(|p| p.contains(needle))
    }

    /// Assert the build rewrote something whose path contains `needle`
    /// (e.g. `"main.o"`, or a target name to catch its relink).
    fn assert_touched(&self, needle: &str, why: &str) {
        assert!(
            self.touched_any(needle),
            "expected the build to rewrite an artifact matching `{needle}`: {why}\n{self}"
        );
    }

    /// Assert the build left everything matching `needle` alone -- the
    /// "this really was cached" half of an incremental assertion.
    fn assert_untouched(&self, needle: &str, why: &str) {
        assert!(
            !self.touched_any(needle),
            "expected the build to reuse every artifact matching `{needle}`: {why}\n{self}"
        );
    }

    /// The link half of the fingerprint cache, for failure messages.
    ///
    /// A rebuild that relinks when it should not is a cache decision, so the
    /// cache is the evidence. Reading it beats guessing, which has already
    /// cost two disproved hypotheses on this exact failure.
    fn link_cache_report(&self) -> String {
        let mut out = String::from("link fingerprint cache:");
        let mut found = false;
        let mut stack = vec![self.root.clone()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p
                    .file_name()
                    .is_some_and(|f| f == ".harbour-fingerprints.json")
                {
                    found = true;
                    match fs::read_to_string(&p) {
                        Ok(t) => {
                            let keys: Vec<&str> = t
                                .split('"')
                                .filter(|s| s.contains("/bin/") || s.contains("\\bin\\"))
                                .collect();
                            out.push_str(&format!(
                                "\n  {}: {} bytes, link keys under bin/: {:?}",
                                p.display(),
                                t.len(),
                                keys
                            ));
                        }
                        Err(err) => {
                            out.push_str(&format!("\n  {}: unreadable ({err})", p.display()))
                        }
                    }
                }
            }
        }
        if !found {
            out.push_str(
                "\n  (no .harbour-fingerprints.json found -- the cache was never written)",
            );
        }
        out
    }

    /// Assert the build produced no new or rewritten artifact at all.
    ///
    /// A rebuild that changes nothing must reuse every object, archive
    /// and binary. When it does not, the failure message dumps the link
    /// fingerprint cache, because a rebuild that redoes work despite an
    /// unchanged tree is a cache decision and the cache is the evidence --
    /// that is what identified two keys for one binary on Windows, fixed
    /// by keying the cache before the artifact's directory exists.
    fn assert_nothing_touched(&self, why: &str) {
        let redone: Vec<&str> = self.touched().collect();
        let gone: Vec<&str> = self
            .removed
            .iter()
            .map(String::as_str)
            .filter(|p| !is_bookkeeping(p))
            .collect();
        assert!(
            redone.is_empty() && gone.is_empty(),
            "expected the build to reuse every artifact, but it redid \
             {redone:?} and removed {gone:?}: {why}\n{self}\n{}",
            self.link_cache_report()
        );
    }
}

impl std::fmt::Display for TreeDiff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let section = |f: &mut std::fmt::Formatter<'_>, label: &str, items: &[String]| {
            writeln!(f, "  {label} ({}):", items.len())?;
            for item in items {
                let note = if is_bookkeeping(item) {
                    " (bookkeeping, not counted as work)"
                } else {
                    ""
                };
                writeln!(f, "    {item}{note}")?;
            }
            Ok(())
        };
        writeln!(f, "build tree changes:")?;
        section(f, "created", &self.created)?;
        section(f, "modified", &self.modified)?;
        section(f, "removed", &self.removed)?;
        section(f, "unchanged", &self.unchanged)
    }
}

/// Snapshot the build tree, run `harbour build`, and report what moved.
///
/// The pattern for "change one thing, rebuild, assert exactly the right
/// artifacts were redone".
fn rebuild_and_diff(home: &std::path::Path, dir: &std::path::Path) -> (RunLog, TreeDiff) {
    rebuild_and_diff_env(home, dir, &[])
}

/// As [`rebuild_and_diff`], with extra environment variables.
fn rebuild_and_diff_env(
    home: &std::path::Path,
    dir: &std::path::Path,
    env: &[(&str, &str)],
) -> (RunLog, TreeDiff) {
    let root = target_dir(dir);
    let before = snapshot_tree(&root);
    let log = harbour_run_env(home, dir, &["build"], env).success();
    let after = snapshot_tree(&root);
    (log, describe_artifact_changes(&root, &before, &after))
}

/// Every distinct toolchain hash recorded in the build tree's fingerprint
/// database.
///
/// Harbour hashes the whole `ToolchainFingerprint` -- compiler family,
/// path, version, target triple, profile -- into each compile fingerprint,
/// so the compiler that produced a build is observable from the outside.
/// [`Rebuild::assert_reused_everything`] uses that to distinguish "the
/// fingerprint failed to reuse an object" from "the compiler changed
/// underneath the build", which are different bugs; both fail, but the
/// failure message says which.
///
/// The production type is deserialised rather than the JSON being scraped,
/// so a change to the cache format breaks this loudly instead of silently
/// returning an empty set.
fn recorded_toolchain_hashes(dir: &std::path::Path) -> std::collections::BTreeSet<String> {
    use harbour::builder::fingerprint::FingerprintCache;

    let mut out = std::collections::BTreeSet::new();
    for path in snapshot_tree(&target_dir(dir)).into_keys() {
        if path.file_name().and_then(|f| f.to_str()) != Some(".harbour-fingerprints.json") {
            continue;
        }
        let cache = FingerprintCache::load(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        out.extend(cache.compile.values().map(|fp| fp.toolchain_hash.clone()));
    }
    out
}

/// One build of an already-built tree, with everything needed to say
/// whether it was entitled to reuse anything.
struct Rebuild {
    log: RunLog,
    diff: TreeDiff,
    toolchain_before: std::collections::BTreeSet<String>,
    toolchain_after: std::collections::BTreeSet<String>,
}

/// Rebuild `dir`, recording both what moved on disk and whether the
/// toolchain changed underneath the build.
fn rebuild(home: &std::path::Path, dir: &std::path::Path) -> Rebuild {
    let toolchain_before = recorded_toolchain_hashes(dir);
    let (log, diff) = rebuild_and_diff(home, dir);
    let toolchain_after = recorded_toolchain_hashes(dir);
    Rebuild {
        log,
        diff,
        toolchain_before,
        toolchain_after,
    }
}

impl Rebuild {
    /// Assert this build recompiled and relinked nothing.
    ///
    /// Nothing asserted anything about incremental freshness before this
    /// existed, so a fingerprint that always reported "dirty" -- rebuilding
    /// the world on every invocation -- was invisible, and so was the
    /// reverse.
    ///
    /// Strict on every platform, including Windows, and deliberately not
    /// gated off MSVC. The reasoning for a gate was that MSVC detection
    /// fails intermittently there (`vcvarsall.bat failed: The batch file
    /// cannot be found`), flipping the compiler identity and the object
    /// extension between two builds of the same tree, so recompiling
    /// everything would be correct rather than a freshness bug. That turned
    /// out to be a plain race and not an environmental fact: detection wrote
    /// its wrapper to a fixed `%TEMP%\harbour_vcvars.bat`, so two concurrent
    /// `harbour` processes fought over one filename -- and `cargo test` runs
    /// integration tests in parallel, which means this suite *is* the load
    /// that triggered it. Fixed in #71. A bug is not a reason to weaken the
    /// invariant that would have caught it.
    ///
    /// A toolchain change between two builds of an unchanged tree is
    /// therefore treated as a failure of this assertion, not as an excuse to
    /// skip it: the recorded toolchain hash is reported so the failure says
    /// which of the two things went wrong instead of leaving it to be
    /// guessed.
    fn assert_reused_everything(&self, context: &str) {
        assert_eq!(
            self.toolchain_before, self.toolchain_after,
            "the toolchain fingerprint changed between two builds of an \
             unchanged tree, so the freshness invariant could not be \
             evaluated. On Windows this is the MSVC detection race (a fixed \
             `%TEMP%` path raced between parallel `harbour` processes; #71), \
             not a fingerprinting bug -- but it is still a failure, because \
             a build whose compiler identity changes underneath it cannot \
             reuse anything.\n\n{context}\n\nrebuild:\n{}\n{}",
            self.log, self.diff
        );

        self.diff
            .assert_nothing_touched(&format!("{context}\n\nrebuild:\n{}", self.log));

        // Secondary check, guarded: where Harbour's per-file decision log is
        // present it must say the files were reused, which catches a build
        // that touched nothing because it never looked at the sources. The
        // guard exists because those lines are `tracing` records, absent
        // under `--quiet`, and a missing log line is not a freshness bug.
        //
        // `file(s) up to date` rather than `up to date`: the compiling line
        // reads `Compiling 1 file(s) (0 up to date)`, so the shorter needle
        // is present even when everything was recompiled -- an assertion
        // that cannot fail.
        let log = self.log.combined();
        if log.contains("file(s)") {
            assert!(
                log.contains("file(s) up to date"),
                "the rebuild touched no artifacts but its decision log does \
                 not report them as up to date, so it may not have \
                 considered the sources at all\n\nrebuild:\n{}",
                self.log
            );
        }
    }
}

/// A clean build followed immediately by a second build with nothing
/// changed.
///
/// Building twice is the only way a whole class of bug is visible at all:
/// sources a `prebuild` step generates were missed on the clean build and
/// compiled on the second (fixed in #63), because globs were expanded when
/// the plan was built and the generator ran later. A single-build test sees
/// a green checkmark either way.
struct BuildTwice {
    clean: RunLog,
    second: Rebuild,
}

fn build_twice(home: &std::path::Path, dir: &std::path::Path) -> BuildTwice {
    let clean = build_ok(home, dir);
    let second = rebuild(home, dir);
    BuildTwice { clean, second }
}

impl BuildTwice {
    /// See [`Rebuild::assert_reused_everything`].
    fn assert_incremental_is_a_no_op(&self) {
        self.second.assert_reused_everything(&format!(
            "nothing changed between the two builds, so every object and \
             artifact must have been reused\n\nclean build:\n{}",
            self.clean
        ));
    }
}

/// File names of the members inside a static archive, parsed straight out
/// of the file.
///
/// GNU `ar` archives, BSD/macOS archives and MSVC `.lib` files are all
/// `!<arch>` archives, so one parser covers every platform without needing
/// `ar` or `lib.exe` on PATH.
///
/// Only the final path component is returned. GNU `ar` stores bare file
/// names, but MSVC's `lib.exe` stores each member under the path it was
/// given on the command line -- and Harbour passes absolute paths -- so
/// comparing raw member names would work on one toolchain and not the
/// other. The file name is the identity that matters anyway: it is what
/// `ar r` matches on, and therefore what decides whether a stale object
/// survives.
///
/// This inspects archive *contents* rather than which definition happened to
/// win symbol resolution. A stale member the linker did not pick this time is
/// still a bug -- that is precisely how the archive bug hid, surfacing only on
/// Windows and only intermittently, when MSVC detection flipped the object
/// extension and both `foo.o` and `foo.obj` sat in the archive.
fn archive_members(path: &std::path::Path) -> Vec<String> {
    let data =
        fs::read(path).unwrap_or_else(|e| panic!("cannot read archive {}: {e}", path.display()));
    assert!(
        data.starts_with(b"!<arch>\n"),
        "{} is not an `!<arch>` static archive (first bytes: {:?})",
        path.display(),
        &data[..data.len().min(16)]
    );

    let field = |bytes: &[u8]| String::from_utf8_lossy(bytes).trim().to_string();
    let base = |name: &str| {
        name.trim_end_matches('\0')
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(name)
            .to_string()
    };
    let mut members = Vec::new();
    let mut long_names: Vec<u8> = Vec::new();
    let mut pos = 8;
    while pos + 60 <= data.len() {
        let header = &data[pos..pos + 60];
        let raw_name = field(&header[0..16]);
        let size: usize = match field(&header[48..58]).parse() {
            Ok(n) => n,
            Err(_) => break,
        };
        let body = &data[pos + 60..(pos + 60 + size).min(data.len())];

        if raw_name == "//" {
            // GNU/MSVC long-name string table; always precedes its users.
            long_names = body.to_vec();
        } else if let Some(len) = raw_name
            .strip_prefix("#1/")
            .and_then(|n| n.parse::<usize>().ok())
        {
            // BSD/macOS: the name is the first `len` bytes of the body.
            let name = base(&field(&body[..len.min(body.len())]));
            if !name.starts_with("__.SYMDEF") {
                members.push(name);
            }
        } else if let Some(offset) = raw_name
            .strip_prefix('/')
            .and_then(|n| n.parse::<usize>().ok())
        {
            let tail = &long_names[offset.min(long_names.len())..];
            let end = tail
                .iter()
                .position(|b| *b == b'/' || *b == b'\n' || *b == 0)
                .unwrap_or(tail.len());
            members.push(base(&field(&tail[..end])));
        } else if raw_name != "/" && raw_name != "/SYM64/" && !raw_name.starts_with("__.SYMDEF") {
            // `/` is the symbol table on GNU and the first linker member on
            // MSVC; neither is a real member.
            members.push(base(raw_name.trim_end_matches('/')));
        }

        pos += 60 + size + (size % 2);
    }
    members.sort();
    members
}

/// The stems of an archive's members -- `one.o` and `one.obj` both become
/// `one`.
///
/// Tests assert on stems rather than on file names because the object
/// extension is not a stable function of the platform: MSVC detection on
/// Windows fails intermittently (`vcvarsall.bat failed: The batch file
/// cannot be found`), and when it does the extension flips from `.obj` to
/// `.o` on the same machine, in the same tree, between two builds. That
/// flakiness is the root cause of the stale-archive bug; a test that hard-
/// codes either extension is asserting on toolchain-detection luck.
fn archive_member_stems(path: &std::path::Path) -> Vec<String> {
    let mut stems: Vec<String> = archive_members(path)
        .iter()
        .map(|m| m.rsplit_once('.').map(|(s, _)| s).unwrap_or(m).to_string())
        .collect();
    stems.sort();
    stems
}

/// The archive Harbour produced for a staticlib target, wherever it landed.
///
/// A root target's archive is written to `debug/lib`, a path dependency's to
/// `debug/deps/<pkg>-<version>/lib`, and the extension is `.a` or `.lib`
/// depending on the toolchain -- so this searches rather than guessing.
fn built_archive_path(dir: &std::path::Path, name: &str) -> PathBuf {
    let root = target_dir(dir);
    let wanted = [format!("lib{name}.a"), format!("{name}.lib")];
    let found: Vec<PathBuf> = snapshot_tree(&root)
        .into_keys()
        .filter(|p| {
            p.file_name()
                .map(|f| wanted.iter().any(|w| w.as_str() == f))
                .unwrap_or(false)
        })
        .collect();
    match found.as_slice() {
        [one] => one.clone(),
        [] => panic!(
            "no archive named {wanted:?} anywhere under {}; the build reported \
             success without producing one.\nbuild tree:\n{:#?}",
            root.display(),
            snapshot_tree(&root).into_keys().collect::<Vec<_>>()
        ),
        many => panic!("expected one archive for `{name}`, found {many:?}"),
    }
}

// ============================================================================
// harbour new
// ============================================================================

#[test]
fn test_new_creates_executable_project() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let project_dir = tmp.path().join("myapp");

    harbour(&home)
        .args(["new", "myapp"])
        .current_dir(tmp.path())
        .assert()
        .success();

    // Check project structure
    assert!(project_dir.join("Harbour.toml").exists());
    assert!(project_dir.join("src").exists());
    assert!(project_dir.join("src/main.c").exists());

    // Check manifest content
    let manifest = fs::read_to_string(project_dir.join("Harbour.toml")).unwrap();
    assert!(manifest.contains("name = \"myapp\""));
    assert!(manifest.contains("kind = \"exe\""));
}

#[test]
fn test_new_creates_library_project() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let project_dir = tmp.path().join("mylib");

    harbour(&home)
        .args(["new", "mylib", "--lib"])
        .current_dir(tmp.path())
        .assert()
        .success();

    // Check project structure
    assert!(project_dir.join("Harbour.toml").exists());
    assert!(project_dir.join("src").exists());
    assert!(project_dir.join("include").exists());

    // Check manifest content
    let manifest = fs::read_to_string(project_dir.join("Harbour.toml")).unwrap();
    assert!(manifest.contains("name = \"mylib\""));
    assert!(manifest.contains("kind = \"staticlib\""));
}

#[test]
fn test_new_fails_if_directory_exists() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let project_dir = tmp.path().join("existing");
    fs::create_dir(&project_dir).unwrap();

    harbour(&home)
        .args(["new", "existing"])
        .current_dir(tmp.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"));
}

// ============================================================================
// harbour init
// ============================================================================

#[test]
fn test_init_in_empty_directory() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["init"])
        .current_dir(tmp.path())
        .assert()
        .success();

    assert!(tmp.path().join("Harbour.toml").exists());
    assert!(tmp.path().join("src").exists());
}

#[test]
fn test_init_fails_if_manifest_exists() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    fs::write(
        tmp.path().join("Harbor.toml"),
        "[package]\nname = \"test\"\n",
    )
    .unwrap();

    harbour(&home)
        .args(["init"])
        .current_dir(tmp.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"));
}

// ============================================================================
// harbour build
// ============================================================================

/// The smoke test: a scaffolded project builds, *runs*, prints what the
/// template says it prints, and a second build with nothing changed is a
/// no-op.
///
/// It used to assert only that `harbour build` exited zero and that a
/// `debug/` directory appeared. Every bug this suite exists to catch
/// produced a successful build, so neither of those could fail on a wrong
/// binary -- or on no binary at all, since the directory exists as soon as
/// the first object is written. Nothing asserted anything about incremental
/// freshness either: a fingerprint that always reported "dirty", or one that
/// wrongly reported "clean", was invisible across the whole suite.
#[test]
fn test_build_simple_project_runs_and_rebuild_is_a_no_op() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "buildtest"])
        .current_dir(tmp.path())
        .assert()
        .success();

    let project_dir = tmp.path().join("buildtest");

    let builds = build_twice(&home, &project_dir);
    assert!(
        builds.clean.combined().contains("Finished"),
        "{}",
        builds.clean
    );

    assert_eq!(
        run_built_exe(&project_dir, "buildtest").out(),
        "Hello, Harbour!",
        "`harbour new` scaffolds a program that prints this; a build that \
         succeeds without producing a working binary must fail here"
    );

    builds.assert_incremental_is_a_no_op();
}

/// `--release` produces a binary that runs and behaves the same.
///
/// Asserting only that a `release/` directory appeared would pass even if
/// the release profile's extra flags (`-O2`, LTO, `NDEBUG`) produced a
/// program that crashed or computed something different -- and the
/// directory appears as soon as the first object is written, whether or not
/// the link ever happened.
#[test]
fn test_build_release_mode_produces_a_working_binary() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "releasetest"])
        .current_dir(tmp.path())
        .assert()
        .success();

    let project_dir = tmp.path().join("releasetest");

    harbour_run(&home, &project_dir, &["build", "--release"]).success();

    assert_eq!(
        run_built_exe_in(&project_dir, "release", "releasetest").out(),
        "Hello, Harbour!",
        "the release profile must produce a binary that behaves like the debug one"
    );
}

#[test]
fn test_build_fails_without_manifest() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["build"])
        .current_dir(tmp.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("no manifest found"))
        .stderr(predicate::str::contains("Harbour.toml"));
}

// ============================================================================
// harbour tree
// ============================================================================

#[test]
fn test_tree_shows_root_package() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "treetest"])
        .current_dir(tmp.path())
        .assert()
        .success();

    let project_dir = tmp.path().join("treetest");

    harbour(&home)
        .args(["tree"])
        .current_dir(&project_dir)
        .assert()
        .success()
        .stdout(predicate::str::contains("treetest"));
}

#[test]
fn test_tree_fails_without_manifest() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["tree"])
        .current_dir(tmp.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("no manifest found"))
        .stderr(predicate::str::contains("Harbour.toml"));
}

// ============================================================================
// harbour flags
// ============================================================================

#[test]
fn test_flags_shows_compile_and_link() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "flagstest"])
        .current_dir(tmp.path())
        .assert()
        .success();

    let project_dir = tmp.path().join("flagstest");

    harbour(&home)
        .args(["flags", "flagstest"])
        .current_dir(&project_dir)
        .assert()
        .success()
        .stdout(predicate::str::contains("Compile flags"))
        .stdout(predicate::str::contains("Link flags"));
}

#[test]
fn test_flags_unknown_target() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "flagstest2"])
        .current_dir(tmp.path())
        .assert()
        .success();

    let project_dir = tmp.path().join("flagstest2");

    harbour(&home)
        .args(["flags", "nonexistent"])
        .current_dir(&project_dir)
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found"))
        .stderr(predicate::str::contains("harbour tree"));
}

// ============================================================================
// harbour clean
// ============================================================================

/// `clean` removes the build tree, and the next build genuinely redoes the
/// work rather than trusting a fingerprint database that outlived it.
///
/// The old test stopped at "the directory is gone". Harbour's fingerprints
/// live inside the build tree (`debug/.harbour-fingerprints.json`), but a
/// cache that ever moved outside it -- or a `clean` that missed it -- would
/// leave the next build reporting everything up to date with no objects on
/// disk, and only linking would fail, if that. Asserting the rebuild
/// recompiles and then *runs* is what makes that visible.
#[test]
fn test_clean_removes_target_directory_and_next_build_redoes_the_work() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "cleantest"])
        .current_dir(tmp.path())
        .assert()
        .success();

    let project_dir = tmp.path().join("cleantest");

    // Build first to create artifacts
    harbour(&home)
        .args(["build"])
        .current_dir(&project_dir)
        .assert()
        .success();

    let target_dir = project_dir.join(".harbour").join("target");
    assert!(target_dir.exists());

    // Clean
    harbour(&home)
        .args(["clean"])
        .current_dir(&project_dir)
        .assert()
        .success();

    assert!(!target_dir.exists());

    // Rebuild from nothing: every artifact must be created afresh, and the
    // program must work. A build that reported "up to date" here would be
    // trusting a cache that no longer describes anything on disk.
    //
    // Deliberately asserted against the build tree rather than the log:
    // wording like `Compiling 1 file(s) (0 up to date)` contains the phrase
    // an "is it fresh?" grep would look for, and the log lines are
    // `tracing` records that `--quiet` suppresses. An object file that
    // exists again is not open to interpretation.
    let (_, diff) = rebuild_and_diff(&home, &project_dir);
    diff.assert_touched(
        "main",
        "the object was deleted by `clean`, so it must be recompiled",
    );
    diff.assert_touched("bin/", "the executable was deleted, so it must be relinked");
    assert_eq!(
        run_built_exe(&project_dir, "cleantest").out(),
        "Hello, Harbour!"
    );
}

// ============================================================================
// harbour add / remove
// ============================================================================

#[test]
fn test_add_path_dependency() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    // Create main project
    harbour(&home)
        .args(["new", "mainpkg"])
        .current_dir(tmp.path())
        .assert()
        .success();

    // Create dependency project
    harbour(&home)
        .args(["new", "deppkg", "--lib"])
        .current_dir(tmp.path())
        .assert()
        .success();

    let main_dir = tmp.path().join("mainpkg");

    // Add dependency
    harbour(&home)
        .args(["add", "deppkg", "--path", "../deppkg"])
        .current_dir(&main_dir)
        .assert()
        .success();

    // Check manifest was updated
    let manifest = fs::read_to_string(main_dir.join("Harbour.toml")).unwrap();
    assert!(manifest.contains("[dependencies]"));
    assert!(manifest.contains("deppkg"));
}

#[test]
fn test_add_registry_dependency() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    // Build a real, local, git-backed registry fixture that is guaranteed
    // not to contain "somepkg" (or any other package). This lets us
    // exercise harbour's real registry-lookup code path without any
    // network access or dependence on the contents of the real,
    // network-hosted default registry.
    let registry_dir = tmp.path().join("fixture-registry");
    local_registry::init(&registry_dir).expect("failed to init fixture registry");
    let registry_url = local_registry::file_url(&registry_dir);

    harbour(&home)
        .args(["new", "addtest"])
        .current_dir(tmp.path())
        .assert()
        .success();

    let project_dir = tmp.path().join("addtest");

    // Adding without --path or --git should error if not found and vcpkg is not configured.
    //
    // NOTE: on Windows, `harbour add <unknown-package>` is known to exit 0
    // instead of non-zero here (a real, pre-existing CLI defect unrelated
    // to registry hermeticity -- see project tracking for the concurrent
    // investigation). This test intentionally does NOT `#[cfg]`-gate or
    // `#[ignore]` around that: it should keep failing on Windows until the
    // underlying exit-code bug is fixed. Do not "fix" this test by loosening
    // the `.failure()` assertion.
    harbour(&home)
        .env("HARBOUR_TEST_REGISTRY_URL", &registry_url)
        .args(["add", "somepkg"])
        .current_dir(&project_dir)
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found in registries"))
        .stderr(predicate::str::contains("vcpkg is not configured"));
}

#[test]
fn test_add_path_and_git_mutually_exclusive() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "addtest"])
        .current_dir(tmp.path())
        .assert()
        .success();

    let project_dir = tmp.path().join("addtest");

    // Can't specify both --path and --git
    harbour(&home)
        .args([
            "add",
            "somepkg",
            "--path",
            "../foo",
            "--git",
            "https://example.com/pkg",
        ])
        .current_dir(&project_dir)
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot specify both"));
}

#[test]
fn test_remove_dependency() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    // Create projects
    harbour(&home)
        .args(["new", "remmain"])
        .current_dir(tmp.path())
        .assert()
        .success();

    harbour(&home)
        .args(["new", "remdep", "--lib"])
        .current_dir(tmp.path())
        .assert()
        .success();

    let main_dir = tmp.path().join("remmain");

    // Add then remove
    harbour(&home)
        .args(["add", "remdep", "--path", "../remdep"])
        .current_dir(&main_dir)
        .assert()
        .success();

    harbour(&home)
        .args(["remove", "remdep"])
        .current_dir(&main_dir)
        .assert()
        .success();

    let manifest = fs::read_to_string(main_dir.join("Harbour.toml")).unwrap();
    assert!(!manifest.contains("remdep"));
}

// ============================================================================
// harbour linkplan
// ============================================================================

#[test]
fn test_linkplan_shows_output() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "linktest"])
        .current_dir(tmp.path())
        .assert()
        .success();

    let project_dir = tmp.path().join("linktest");

    harbour(&home)
        .args(["linkplan", "linktest"])
        .current_dir(&project_dir)
        .assert()
        .success()
        .stdout(predicate::str::contains("Link order"));
}

// ============================================================================
// harbour explain
// ============================================================================

#[test]
fn test_explain_root_package() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "explaintest"])
        .current_dir(tmp.path())
        .assert()
        .success();

    let project_dir = tmp.path().join("explaintest");

    harbour(&home)
        .args(["explain", "explaintest"])
        .current_dir(&project_dir)
        .assert()
        .success()
        .stdout(predicate::str::contains("explaintest"))
        .stdout(predicate::str::contains("root"));
}

#[test]
fn test_explain_unknown_package() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "explaintest2"])
        .current_dir(tmp.path())
        .assert()
        .success();

    let project_dir = tmp.path().join("explaintest2");

    harbour(&home)
        .args(["explain", "nonexistent"])
        .current_dir(&project_dir)
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found"))
        .stderr(predicate::str::contains("harbour tree"));
}

// ============================================================================
// harbour test
// ============================================================================

#[test]
fn test_test_no_targets_found() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "testnotest"])
        .current_dir(tmp.path())
        .assert()
        .success();

    let project_dir = tmp.path().join("testnotest");

    harbour(&home)
        .args(["test"])
        .current_dir(&project_dir)
        .assert()
        .success()
        .stdout(predicate::str::contains("No test targets found"));
}

#[test]
fn test_test_discovers_test_target() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "testwithtest"])
        .current_dir(tmp.path())
        .assert()
        .success();

    let project_dir = tmp.path().join("testwithtest");

    // Add a test target to the manifest
    let manifest_path = project_dir.join("Harbour.toml");
    let mut manifest = fs::read_to_string(&manifest_path).unwrap();
    manifest.push_str(
        r#"
[targets.unit_test]
kind = "exe"
sources = ["tests/**/*.c"]
"#,
    );
    fs::write(&manifest_path, manifest).unwrap();

    // Create test source
    fs::create_dir_all(project_dir.join("tests")).unwrap();
    fs::write(
        project_dir.join("tests/test_main.c"),
        r#"
int main(void) {
    return 0;  // Success
}
"#,
    )
    .unwrap();

    harbour(&home)
        .args(["test"])
        .current_dir(&project_dir)
        .assert()
        .success()
        .stdout(predicate::str::contains("unit_test"))
        .stdout(predicate::str::contains("ok"));

    // The other half, which nothing covered: a test binary that exits
    // non-zero must make `harbour test` fail. Discovering and building a
    // test target while ignoring its exit status would still print
    // `unit_test` and still exit zero, so the assertions above cannot tell
    // "the test passed" from "the result was never checked".
    fs::write(
        project_dir.join("tests/test_main.c"),
        "int main(void) {\n    return 3;\n}\n",
    )
    .unwrap();

    let failing = harbour_run(&home, &project_dir, &["test"]);
    assert!(
        !failing.status.success(),
        "a test target exiting non-zero must fail `harbour test`\n{failing}"
    );
    assert!(
        failing.combined().contains("FAILED"),
        "the failure must name the failing target\n{failing}"
    );
}

// ============================================================================
// harbour toolchain
// ============================================================================

#[test]
fn test_toolchain_show() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "toolchaintest"])
        .current_dir(tmp.path())
        .assert()
        .success();

    let project_dir = tmp.path().join("toolchaintest");

    harbour(&home)
        .args(["toolchain", "show"])
        .current_dir(&project_dir)
        .assert()
        .success();
}

// ============================================================================
// Full workflow test
// ============================================================================

/// Path to a binary produced under `<app_dir>/.harbour/target/debug/bin`,
/// with the platform-appropriate executable extension.
fn built_exe_path(app_dir: &std::path::Path, name: &str) -> PathBuf {
    built_exe_path_in(app_dir, "debug", name)
}

/// As [`built_exe_path`], for a named profile (`debug` / `release`).
fn built_exe_path_in(app_dir: &std::path::Path, profile: &str, name: &str) -> PathBuf {
    let file_name = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    target_dir(app_dir)
        .join(profile)
        .join("bin")
        .join(file_name)
}

#[test]
fn test_full_workflow_with_dependency() {
    // Regression coverage for the "linkplan lists the archive but the
    // actual link command doesn't" bug: the dependency exposes a real
    // *function* (not just a macro), the app *calls* it, and the test runs
    // the resulting binary and asserts on its output. A build that links
    // successfully but produces a binary that computes the wrong thing (or
    // a build that fails to link the dependency archive at all, as
    // happened before this fix) must fail this test.
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    // 1. Create a library
    harbour(&home)
        .args(["new", "myutil", "--lib"])
        .current_dir(tmp.path())
        .assert()
        .success();

    let lib_dir = tmp.path().join("myutil");

    // Update manifest to expose include dir
    fs::write(
        lib_dir.join("Harbour.toml"),
        r#"[package]
name = "myutil"
version = "0.1.0"

[targets.myutil]
kind = "staticlib"
sources = ["src/**/*.c"]

[targets.myutil.surface.compile.public]
include_dirs = ["include"]
"#,
    )
    .unwrap();

    // Add header declaring a real function, not just a macro -- the
    // original version of this test only exercised the include path, never
    // the linker, and passed even while dependency archives were silently
    // dropped from the link command.
    fs::create_dir_all(lib_dir.join("include")).unwrap();
    fs::write(
        lib_dir.join("include/myutil.h"),
        r#"#ifndef MYUTIL_H
#define MYUTIL_H
int myutil_double(int x);
#endif
"#,
    )
    .unwrap();
    fs::write(
        lib_dir.join("src/lib.c"),
        r#"#include "myutil.h"

int myutil_double(int x) {
    return x * 2;
}
"#,
    )
    .unwrap();

    // 2. Create an application that uses the library
    harbour(&home)
        .args(["new", "myapp"])
        .current_dir(tmp.path())
        .assert()
        .success();

    let app_dir = tmp.path().join("myapp");

    // 3. Add the library as a dependency
    //
    // NOTE: this uses a path dependency, so it never touches the registry
    // or the network. If this test is ever seen failing on Windows CI, that
    // is tracked as the same pre-existing, unrelated CLI exit-code defect
    // referenced in `test_add_registry_dependency` above -- do not paper
    // over it here either.
    harbour(&home)
        .args(["add", "myutil", "--path", "../myutil"])
        .current_dir(&app_dir)
        .assert()
        .success();

    // 4. Update the app to call the library's function and print the result.
    fs::write(
        app_dir.join("src/main.c"),
        r#"#include <stdio.h>
#include "myutil.h"

int main(void) {
    printf("%d\n", myutil_double(21));
    return 0;
}
"#,
    )
    .unwrap();

    // 5. Check the dependency tree
    harbour(&home)
        .args(["tree"])
        .current_dir(&app_dir)
        .assert()
        .success()
        .stdout(predicate::str::contains("myapp"))
        .stdout(predicate::str::contains("myutil"));

    // 6. Check flags show the dependency's include path
    harbour(&home)
        .args(["flags", "myapp"])
        .current_dir(&app_dir)
        .assert()
        .success()
        .stdout(predicate::str::contains("myutil"));

    // 7. Check linkplan shows the dependency
    harbour(&home)
        .args(["linkplan", "myapp"])
        .current_dir(&app_dir)
        .assert()
        .success()
        .stdout(predicate::str::contains("myutil"));

    // 8. Build the application. This must actually link `libmyutil.a` into
    // `myapp` -- not just resolve its include path.
    harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .success()
        .stderr(predicate::str::contains("Finished"));

    // 9. Verify outputs exist
    let target_dir = app_dir.join(".harbour").join("target").join("debug");
    assert!(target_dir.exists());

    // 10. Run the built binary and check its actual output. This is the
    // assertion that catches both "didn't link at all" (the binary
    // wouldn't exist / build would have failed at step 8) and "linked but
    // computed the wrong thing" (wrong output here).
    let exe = built_exe_path(&app_dir, "myapp");
    assert!(exe.exists(), "built executable not found at {exe:?}");
    let output = Command::new(&exe).output().unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "42");
}

/// Transitive dependency: `app -> libb -> liba`, where `libb` calls into
/// `liba`. This is the scenario from the bug report's "second, related
/// bug" (link order): without the fix, `linkplan` emitted `liba` before
/// `libb`, which is backwards for static linking (`liba` gets linked
/// before anything has asked it to resolve `_liba_answer`, so a
/// traditional left-to-right static linker never pulls its objects in).
///
/// `app` declares only `libb`; `liba` is reached transitively. That is the
/// point of the test as much as the linking is -- an earlier version had to
/// declare `liba` on `app` as well, because only root-declared path
/// dependencies were resolvable.
#[test]
fn test_transitive_dependency_links_and_runs() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    // liba: leaf static library.
    harbour(&home)
        .args(["new", "liba", "--lib"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let liba_dir = tmp.path().join("liba");
    fs::write(
        liba_dir.join("Harbour.toml"),
        r#"[package]
name = "liba"
version = "0.1.0"

[targets.liba]
kind = "staticlib"
sources = ["src/**/*.c"]

[targets.liba.surface.compile.public]
include_dirs = ["include"]
"#,
    )
    .unwrap();
    fs::create_dir_all(liba_dir.join("include")).unwrap();
    fs::write(
        liba_dir.join("include/liba.h"),
        r#"#ifndef LIBA_H
#define LIBA_H
int liba_answer(void);
#endif
"#,
    )
    .unwrap();
    fs::write(
        liba_dir.join("src/lib.c"),
        r#"#include "liba.h"

int liba_answer(void) {
    return 42;
}
"#,
    )
    .unwrap();

    // libb: static library that calls into liba.
    harbour(&home)
        .args(["new", "libb", "--lib"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let libb_dir = tmp.path().join("libb");
    fs::write(
        libb_dir.join("Harbour.toml"),
        r#"[package]
name = "libb"
version = "0.1.0"

[targets.libb]
kind = "staticlib"
sources = ["src/**/*.c"]

[targets.libb.surface.compile.public]
include_dirs = ["include"]
"#,
    )
    .unwrap();
    fs::create_dir_all(libb_dir.join("include")).unwrap();
    fs::write(
        libb_dir.join("include/libb.h"),
        r#"#ifndef LIBB_H
#define LIBB_H
int libb_double_answer(void);
#endif
"#,
    )
    .unwrap();
    fs::write(
        libb_dir.join("src/lib.c"),
        r#"#include "libb.h"
#include "liba.h"

int libb_double_answer(void) {
    return liba_answer() * 2;
}
"#,
    )
    .unwrap();
    harbour(&home)
        .args(["add", "liba", "--path", "../liba"])
        .current_dir(&libb_dir)
        .assert()
        .success();

    // app: depends on libb only. liba arrives transitively.
    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");
    harbour(&home)
        .args(["add", "libb", "--path", "../libb"])
        .current_dir(&app_dir)
        .assert()
        .success();
    fs::write(
        app_dir.join("src/main.c"),
        r#"#include <stdio.h>
#include "libb.h"

int main(void) {
    printf("%d\n", libb_double_answer());
    return 0;
}
"#,
    )
    .unwrap();

    harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .success()
        .stderr(predicate::str::contains("Finished"));

    let exe = built_exe_path(&app_dir, "app");
    assert!(exe.exists(), "built executable not found at {exe:?}");
    let output = Command::new(&exe).output().unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "84");
}

/// Diamond dependency shape: `app -> b -> d` and `app -> c -> d`. `d` must
/// be linked exactly once, positioned after both `b` and `c` on the link
/// line, and the computed result must be correct -- catching both
/// "dropped" (missing symbol at link time) and "duplicated" (which some
/// naive link-order fixes could produce for a diamond) failure modes.
///
/// Same shape as
/// `test_transitive_dependency_links_and_runs` above: `d` is also declared
/// directly on `app` until the resolver fix lands.
#[test]
fn test_diamond_dependency_links_once() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let make_lib = |name: &str, header_body: &str, source_body: &str| {
        harbour(&home)
            .args(["new", name, "--lib"])
            .current_dir(tmp.path())
            .assert()
            .success();
        let dir = tmp.path().join(name);
        fs::write(
            dir.join("Harbour.toml"),
            format!(
                r#"[package]
name = "{name}"
version = "0.1.0"

[targets.{name}]
kind = "staticlib"
sources = ["src/**/*.c"]

[targets.{name}.surface.compile.public]
include_dirs = ["include"]
"#
            ),
        )
        .unwrap();
        fs::create_dir_all(dir.join("include")).unwrap();
        fs::write(dir.join(format!("include/{name}.h")), header_body).unwrap();
        fs::write(dir.join("src/lib.c"), source_body).unwrap();
        dir
    };

    // d: the shared tail of the diamond.
    make_lib(
        "libd",
        "#ifndef LIBD_H\n#define LIBD_H\nint libd_value(void);\n#endif\n",
        "#include \"libd.h\"\n\nint libd_value(void) {\n    return 7;\n}\n",
    );

    // b: app -> b -> d
    let libb_dir = make_lib(
        "libb",
        "#ifndef LIBB_H\n#define LIBB_H\nint libb_via_d(void);\n#endif\n",
        "#include \"libb.h\"\n#include \"libd.h\"\n\nint libb_via_d(void) {\n    return libd_value() + 1;\n}\n",
    );
    harbour(&home)
        .args(["add", "libd", "--path", "../libd"])
        .current_dir(&libb_dir)
        .assert()
        .success();

    // c: app -> c -> d
    let libc_dir = make_lib(
        "libc",
        "#ifndef LIBC_H\n#define LIBC_H\nint libc_via_d(void);\n#endif\n",
        "#include \"libc.h\"\n#include \"libd.h\"\n\nint libc_via_d(void) {\n    return libd_value() * 10;\n}\n",
    );
    harbour(&home)
        .args(["add", "libd", "--path", "../libd"])
        .current_dir(&libc_dir)
        .assert()
        .success();

    // app depends on b and c only. d arrives transitively through both.
    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");
    for dep in ["libb", "libc"] {
        harbour(&home)
            .args(["add", dep, "--path", &format!("../{dep}")])
            .current_dir(&app_dir)
            .assert()
            .success();
    }
    fs::write(
        app_dir.join("src/main.c"),
        r#"#include <stdio.h>
#include "libb.h"
#include "libc.h"

int main(void) {
    printf("%d\n", libb_via_d() + libc_via_d());
    return 0;
}
"#,
    )
    .unwrap();

    harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .success()
        .stderr(predicate::str::contains("Finished"));

    let exe = built_exe_path(&app_dir, "app");
    assert!(exe.exists(), "built executable not found at {exe:?}");
    let output = Command::new(&exe).output().unwrap();
    assert!(output.status.success());
    // (7 + 1) + (7 * 10) = 78. If `d` were duplicated or dropped from the
    // link line, this would either fail to link or (in principle, if a
    // buggy dedup silently discarded one of the sibling libraries instead
    // of `d`) produce a different number.
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "78");
}

// ============================================================================
// Features affecting native builds
// ============================================================================

/// A single-source library, deliberately shaped like sqlite's amalgamation:
/// one `.c` file whose behavior branches entirely on whether a preprocessor
/// define is present, and that define is only supplied when a manifest
/// `[features]` toggle is enabled.
///
/// This is the strong form of the validation the change asked for: not "the
/// flag string contains -DENABLE_FTS5", but "the same source, compiled
/// twice with the feature off vs. on, produces a binary that runs
/// differently" -- so a regression that silently stops threading the
/// feature into the compile step (e.g. because `resolved_extra_compile` or
/// the `feature = "..."` predicate on `PlatformCondition` broke) fails this
/// test via a wrong *runtime* answer, not just a missing string in a
/// recorded flag list.
fn write_feature_lib(dir: &std::path::Path) {
    fs::create_dir_all(dir).unwrap();
    fs::write(
        dir.join("Harbour.toml"),
        r#"[package]
name = "sqlike"
version = "0.1.0"

[features]
fts5 = []

[targets.sqlike]
kind = "staticlib"
sources = ["src/**/*.c"]

[targets.sqlike.surface.compile.public]
include_dirs = ["include"]

[[targets.sqlike.when]]
feature = "fts5"
defines = ["ENABLE_FTS5"]
"#,
    )
    .unwrap();
    fs::create_dir_all(dir.join("include")).unwrap();
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join("include/sqlike.h"),
        r#"#ifndef SQLIKE_H
#define SQLIKE_H
int sqlike_has_fts5(void);
#endif
"#,
    )
    .unwrap();
    fs::write(
        dir.join("src/lib.c"),
        r#"#include "sqlike.h"

int sqlike_has_fts5(void) {
#ifdef ENABLE_FTS5
    return 1;
#else
    return 0;
#endif
}
"#,
    )
    .unwrap();
}

#[test]
fn test_feature_toggles_define_and_changes_binary_behavior() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    write_feature_lib(&tmp.path().join("sqlike"));

    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");

    harbour(&home)
        .args(["add", "sqlike", "--path", "../sqlike"])
        .current_dir(&app_dir)
        .assert()
        .success();

    fs::write(
        app_dir.join("src/main.c"),
        r#"#include <stdio.h>
#include "sqlike.h"

int main(void) {
    printf("%d\n", sqlike_has_fts5());
    return 0;
}
"#,
    )
    .unwrap();

    // Feature off (default): `fts5` is not in `[features]`'s implicit
    // default set (there is no `default` key), and the app didn't request
    // it, so the library must be built without ENABLE_FTS5.
    harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .success();
    let exe = built_exe_path(&app_dir, "app");
    let output = Command::new(&exe).output().unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "0",
        "feature off must compile out ENABLE_FTS5"
    );

    // Turn the feature on from the dependent and rebuild. Editing the
    // dependency line directly (rather than through `harbour add
    // --features`, which today only plumbs vcpkg feature selection) matches
    // how `[dependencies].features` is actually meant to be authored for a
    // native package -- see `DetailedDependencySpec::features`.
    let manifest_path = app_dir.join("Harbour.toml");
    let manifest = fs::read_to_string(&manifest_path).unwrap();
    let manifest = manifest.replace(
        r#"sqlike = { path = "../sqlike" }"#,
        r#"sqlike = { path = "../sqlike", features = ["fts5"] }"#,
    );
    assert_ne!(
        manifest,
        fs::read_to_string(&manifest_path).unwrap(),
        "expected `harbour add`'s generated dependency line to match the replaced pattern"
    );
    fs::write(&manifest_path, manifest).unwrap();

    // Captured for the same reason as the `dep/feature` one-hop test: this
    // fails intermittently on Windows with the pre-change output, and the
    // build log is what distinguishes "never recompiled" from "recompiled
    // but not relinked".
    let rebuild = harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .success();
    let rebuild_log = String::from_utf8_lossy(&rebuild.get_output().stderr).into_owned();

    let output = Command::new(&exe).output().unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "1",
        "feature on must define ENABLE_FTS5 and recompile the library.\n\n\
         Rebuild log:\n{rebuild_log}"
    );
}

// ============================================================================
// `dep/feature`: requesting a feature of a dependency's own dependency
// ============================================================================

/// A library with a `[features]` entry whose only job is to gate a
/// preprocessor define -- same shape as `write_feature_lib` above, but
/// parameterized so it can be reused for `inner`/`mid`/`leaf` roles across
/// the tests below.
fn write_relay_lib(dir: &std::path::Path, name: &str, feature: &str, define: &str, value_fn: &str) {
    fs::create_dir_all(dir).unwrap();
    fs::write(
        dir.join("Harbour.toml"),
        format!(
            r#"[package]
name = "{name}"
version = "0.1.0"

[features]
{feature} = []

[targets.{name}]
kind = "staticlib"
sources = ["src/**/*.c"]

[targets.{name}.surface.compile.public]
include_dirs = ["include"]

[[targets.{name}.when]]
feature = "{feature}"
defines = ["{define}"]
"#
        ),
    )
    .unwrap();
    fs::create_dir_all(dir.join("include")).unwrap();
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join(format!("include/{name}.h")),
        format!(
            "#ifndef {name}_H\n#define {name}_H\nint {value_fn}(void);\n#endif\n",
            name = name.to_uppercase()
        ),
    )
    .unwrap();
    fs::write(
        dir.join("src/lib.c"),
        format!(
            r#"#include "{name}.h"

int {value_fn}(void) {{
#ifdef {define}
    return 1;
#else
    return 0;
#endif
}}
"#
        ),
    )
    .unwrap();
}

/// `outer` declares `want = ["inner/deep"]`; the app requests `outer/want`
/// only -- never touching `inner` directly. `inner`'s function must return
/// the enabled value, proving the request reached across the one hop from
/// `outer`'s own `[features]` entry to `inner`'s.
///
/// `app` declares only `outer`; `inner` is reached transitively, so the test
/// also covers that the request crosses a dependency edge the root never names.
#[test]
fn test_dep_feature_propagates_one_hop_and_changes_binary_behavior() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    write_relay_lib(
        &tmp.path().join("inner"),
        "inner",
        "deep",
        "ENABLE_DEEP",
        "inner_value",
    );

    fs::create_dir_all(tmp.path().join("outer")).unwrap();
    fs::write(
        tmp.path().join("outer/Harbour.toml"),
        r#"[package]
name = "outer"
version = "0.1.0"

[features]
want = ["inner/deep"]

[targets.outer]
kind = "staticlib"
sources = ["src/**/*.c"]

[targets.outer.surface.compile.public]
include_dirs = ["include"]
"#,
    )
    .unwrap();
    fs::create_dir_all(tmp.path().join("outer/include")).unwrap();
    fs::create_dir_all(tmp.path().join("outer/src")).unwrap();
    fs::write(
        tmp.path().join("outer/include/outer.h"),
        "#ifndef OUTER_H\n#define OUTER_H\nint outer_value(void);\n#endif\n",
    )
    .unwrap();
    fs::write(
        tmp.path().join("outer/src/lib.c"),
        r#"#include "outer.h"
#include "inner.h"

int outer_value(void) {
    return inner_value();
}
"#,
    )
    .unwrap();
    harbour(&home)
        .args(["add", "inner", "--path", "../inner"])
        .current_dir(tmp.path().join("outer"))
        .assert()
        .success();

    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");
    harbour(&home)
        .args(["add", "outer", "--path", "../outer"])
        .current_dir(&app_dir)
        .assert()
        .success();

    fs::write(
        app_dir.join("src/main.c"),
        r#"#include <stdio.h>
#include "outer.h"

int main(void) {
    printf("%d\n", outer_value());
    return 0;
}
"#,
    )
    .unwrap();

    // First, without requesting `outer/want` at all: `inner` must be built
    // without ENABLE_DEEP.
    harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .success()
        .stderr(predicate::str::contains("Finished"));

    let exe = built_exe_path(&app_dir, "app");
    let output = Command::new(&exe).output().unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "0",
        "feature off (nothing requests `outer/want`): inner must not define ENABLE_DEEP"
    );

    // Now request `outer/want` only -- never `inner` directly -- and
    // rebuild *without* cleaning first. This is the fingerprint-
    // invalidation check requirement 5 in the task asked for: a change in
    // a *propagated* feature set must invalidate `inner`'s compile just as
    // surely as a directly-requested one would, since it flows into the
    // same `defines`/`cflags` that feed `CompileFingerprint::flags_hash`.
    // If propagation updated the in-memory feature set but something
    // upstream cached inner's old fingerprint, this rebuild would wrongly
    // skip recompiling `inner` and the binary would still print `0`.
    let manifest_path = app_dir.join("Harbour.toml");
    let manifest = fs::read_to_string(&manifest_path).unwrap();
    let manifest = manifest.replace(
        r#"outer = { path = "../outer" }"#,
        r#"outer = { path = "../outer", features = ["want"] }"#,
    );
    assert_ne!(manifest, fs::read_to_string(&manifest_path).unwrap());
    fs::write(&manifest_path, manifest).unwrap();

    // Snapshot the build tree across the rebuild so a failure can say
    // *which* stage went wrong, and assert each stage separately. The two
    // candidate causes need different fixes -- `inner` was never recompiled
    // (a fingerprint that failed to invalidate on a propagated feature
    // change), or it was recompiled and the executable was not relinked
    // against the new archive -- and `left: "0"` alone cannot tell them
    // apart. That ambiguity is what made the first two attempts at
    // diagnosing this Windows-only, intermittent failure useless.
    let (rebuild, diff) = rebuild_and_diff(&home, &app_dir);
    diff.assert_touched(
        "inner",
        "a change in a *propagated* feature set must invalidate inner's \
         cached object exactly as a directly requested one would",
    );
    diff.assert_touched(
        "app",
        "inner was recompiled, so the executable must be relinked against \
         the new archive",
    );

    let out = run_built_exe(&app_dir, "app");
    assert_eq!(
        out.out(),
        "1",
        "app now requests `outer/want`; `outer`'s `dep/feature` entry must have \
         propagated `deep` onto `inner` and defined ENABLE_DEEP there.\n\n\
         rebuild:\n{rebuild}\n{diff}"
    );
}

/// A chain three deep: `app -> outer -> mid -> leaf`. `app` requests only
/// `outer/want`; `outer`'s `want` requests `mid/relay`; `mid`'s `relay`
/// requests `leaf/leaf_feat`. Two hops of `dep/feature` back to back,
/// proving propagation is transitive rather than a single-hop special case.
#[test]
fn test_dep_feature_propagates_transitively_through_a_chain() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    write_relay_lib(
        &tmp.path().join("leaf"),
        "leaf",
        "leaf_feat",
        "ENABLE_LEAF",
        "leaf_value",
    );

    fs::create_dir_all(tmp.path().join("mid")).unwrap();
    fs::write(
        tmp.path().join("mid/Harbour.toml"),
        r#"[package]
name = "mid"
version = "0.1.0"

[features]
relay = ["leaf/leaf_feat"]

[targets.mid]
kind = "staticlib"
sources = ["src/**/*.c"]

[targets.mid.surface.compile.public]
include_dirs = ["include"]
"#,
    )
    .unwrap();
    fs::create_dir_all(tmp.path().join("mid/include")).unwrap();
    fs::create_dir_all(tmp.path().join("mid/src")).unwrap();
    fs::write(
        tmp.path().join("mid/include/mid.h"),
        "#ifndef MID_H\n#define MID_H\nint mid_value(void);\n#endif\n",
    )
    .unwrap();
    fs::write(
        tmp.path().join("mid/src/lib.c"),
        r#"#include "mid.h"
#include "leaf.h"

int mid_value(void) {
    return leaf_value();
}
"#,
    )
    .unwrap();
    harbour(&home)
        .args(["add", "leaf", "--path", "../leaf"])
        .current_dir(tmp.path().join("mid"))
        .assert()
        .success();

    fs::create_dir_all(tmp.path().join("outer")).unwrap();
    fs::write(
        tmp.path().join("outer/Harbour.toml"),
        r#"[package]
name = "outer"
version = "0.1.0"

[features]
want = ["mid/relay"]

[targets.outer]
kind = "staticlib"
sources = ["src/**/*.c"]

[targets.outer.surface.compile.public]
include_dirs = ["include"]
"#,
    )
    .unwrap();
    fs::create_dir_all(tmp.path().join("outer/include")).unwrap();
    fs::create_dir_all(tmp.path().join("outer/src")).unwrap();
    fs::write(
        tmp.path().join("outer/include/outer.h"),
        "#ifndef OUTER_H\n#define OUTER_H\nint outer_value(void);\n#endif\n",
    )
    .unwrap();
    fs::write(
        tmp.path().join("outer/src/lib.c"),
        r#"#include "outer.h"
#include "mid.h"

int outer_value(void) {
    return mid_value();
}
"#,
    )
    .unwrap();
    harbour(&home)
        .args(["add", "mid", "--path", "../mid"])
        .current_dir(tmp.path().join("outer"))
        .assert()
        .success();

    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");
    harbour(&home)
        .args(["add", "outer", "--path", "../outer"])
        .current_dir(&app_dir)
        .assert()
        .success();

    let manifest_path = app_dir.join("Harbour.toml");
    let manifest = fs::read_to_string(&manifest_path).unwrap();
    let manifest = manifest.replace(
        r#"outer = { path = "../outer" }"#,
        r#"outer = { path = "../outer", features = ["want"] }"#,
    );
    assert_ne!(manifest, fs::read_to_string(&manifest_path).unwrap());
    fs::write(&manifest_path, manifest).unwrap();

    fs::write(
        app_dir.join("src/main.c"),
        r#"#include <stdio.h>
#include "outer.h"

int main(void) {
    printf("%d\n", outer_value());
    return 0;
}
"#,
    )
    .unwrap();

    harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .success()
        .stderr(predicate::str::contains("Finished"));

    let exe = built_exe_path(&app_dir, "app");
    let output = Command::new(&exe).output().unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "1",
        "app requested only `outer/want`; propagation must cross both \
         `outer -> mid` and `mid -> leaf` dep/feature hops to define \
         ENABLE_LEAF in `leaf`"
    );
}

/// Diamond dependency shape (`app -> b -> d`, `app -> relay_c -> d`), but the
/// requests on the shared tail `d` arrive via `dep/feature` rather than a
/// direct `features = [...]` entry: `b` declares `want_x = ["d/x"]` and
/// `relay_c` declares `want_y = ["d/y"]`; the app requests both.
/// `d`'s final feature set must be the union `{x, y}` -- if unification
/// broke for propagated requests specifically (as opposed to direct ones,
/// already covered by `compute_feature_sets_unifies_disjoint_dependent_requests`
/// in `surface_resolver.rs`), `d` would only ever see one of the two and
/// this test's arithmetic would come out wrong.
#[test]
fn test_dep_feature_diamond_union_via_dep_feature() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    fs::create_dir_all(tmp.path().join("d")).unwrap();
    fs::write(
        tmp.path().join("d/Harbour.toml"),
        r#"[package]
name = "d"
version = "0.1.0"

[features]
x = []
y = []

[targets.d]
kind = "staticlib"
sources = ["src/**/*.c"]

[targets.d.surface.compile.public]
include_dirs = ["include"]

[[targets.d.when]]
feature = "x"
defines = ["ENABLE_X"]

[[targets.d.when]]
feature = "y"
defines = ["ENABLE_Y"]
"#,
    )
    .unwrap();
    fs::create_dir_all(tmp.path().join("d/include")).unwrap();
    fs::create_dir_all(tmp.path().join("d/src")).unwrap();
    fs::write(
        tmp.path().join("d/include/d.h"),
        "#ifndef D_H\n#define D_H\nint d_value(void);\n#endif\n",
    )
    .unwrap();
    fs::write(
        tmp.path().join("d/src/lib.c"),
        r#"#include "d.h"

int d_value(void) {
    int v = 0;
#ifdef ENABLE_X
    v += 1;
#endif
#ifdef ENABLE_Y
    v += 10;
#endif
    return v;
}
"#,
    )
    .unwrap();

    let make_relay = |name: &str, feature: &str, dep_feature: &str, value_fn: &str| {
        let dir = tmp.path().join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("Harbour.toml"),
            format!(
                r#"[package]
name = "{name}"
version = "0.1.0"

[features]
{feature} = ["d/{dep_feature}"]

[targets.{name}]
kind = "staticlib"
sources = ["src/**/*.c"]

[targets.{name}.surface.compile.public]
include_dirs = ["include"]
"#
            ),
        )
        .unwrap();
        fs::create_dir_all(dir.join("include")).unwrap();
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(
            dir.join(format!("include/{name}.h")),
            format!(
                "#ifndef {upper}_H\n#define {upper}_H\nint {value_fn}(void);\n#endif\n",
                upper = name.to_uppercase()
            ),
        )
        .unwrap();
        fs::write(
            dir.join("src/lib.c"),
            format!(
                r#"#include "{name}.h"
#include "d.h"

int {value_fn}(void) {{
    return d_value();
}}
"#
            ),
        )
        .unwrap();
        harbour(&home)
            .args(["add", "d", "--path", "../d"])
            .current_dir(&dir)
            .assert()
            .success();
    };

    make_relay("b", "want_x", "x", "b_value");
    // Not named `c`: the archive would be `libc.a`, and since a dependency's
    // lib dir lands on `-L`, gcc's implicit `-lc` would resolve to it instead
    // of the system C library and the link would fail with undefined
    // `__libc_start_main`/`printf`.
    make_relay("relay_c", "want_y", "y", "c_value");

    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");
    // `app` names only b and c; d is reached transitively through both.
    for dep in ["b", "relay_c"] {
        harbour(&home)
            .args(["add", dep, "--path", &format!("../{dep}")])
            .current_dir(&app_dir)
            .assert()
            .success();
    }

    let manifest_path = app_dir.join("Harbour.toml");
    let manifest = fs::read_to_string(&manifest_path).unwrap();
    let manifest = manifest
        .replace(
            r#"b = { path = "../b" }"#,
            r#"b = { path = "../b", features = ["want_x"] }"#,
        )
        .replace(
            r#"relay_c = { path = "../relay_c" }"#,
            r#"relay_c = { path = "../relay_c", features = ["want_y"] }"#,
        );
    assert_ne!(manifest, fs::read_to_string(&manifest_path).unwrap());
    fs::write(&manifest_path, manifest).unwrap();

    fs::write(
        app_dir.join("src/main.c"),
        r#"#include <stdio.h>
#include "b.h"
#include "relay_c.h"

int main(void) {
    printf("%d\n", b_value() + c_value());
    return 0;
}
"#,
    )
    .unwrap();

    harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .success()
        .stderr(predicate::str::contains("Finished"));

    let exe = built_exe_path(&app_dir, "app");
    let output = Command::new(&exe).output().unwrap();
    assert!(output.status.success());
    // If the union held, `d` is built once with {x, y}, so d_value() == 11
    // everywhere and the sum is 22. If propagated requests failed to unify
    // (e.g. only the last writer won), `d` would see only one of {x, y}
    // and this would come out as 2 (both see only x) or 20 (both see only
    // y) instead.
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "22",
        "d's feature set must be the union {{x, y}} of both dep/feature requests"
    );
}

/// Assembly sources compile, get the C preprocessor (so `-I`/`-D` apply),
/// mix with C in one target, and participate in header-dependency
/// invalidation like any other source.
///
/// Gated to the two architectures Harbour's CI runs on, and off MSVC,
/// which assembles with a separate `ml64.exe`/`armasm64.exe` and is
/// rejected with a dedicated error instead.
#[test]
#[cfg(all(
    not(target_env = "msvc"),
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
fn test_assembly_source_builds_links_and_tracks_headers() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "asmapp"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("asmapp");

    // Apple prefixes C symbols with an underscore; ELF does not.
    let sym = if cfg!(target_vendor = "apple") {
        "_fast_add"
    } else {
        "fast_add"
    };

    #[cfg(target_arch = "aarch64")]
    let body = format!(
        "    .text\n    .globl {sym}\n    .align 2\n{sym}:\n\
         \x20   add w0, w0, w1\n    add w0, w0, #BIAS\n    ret\n"
    );
    #[cfg(target_arch = "x86_64")]
    let body = format!(
        "    .text\n    .globl {sym}\n{sym}:\n\
         \x20   movl %edi, %eax\n    addl %esi, %eax\n    addl $BIAS, %eax\n    ret\n"
    );

    // `.S` (capital) so the C preprocessor runs and resolves the include.
    fs::write(
        app_dir.join("src/fast_add.S"),
        format!("#include \"bias.h\"\n{body}"),
    )
    .unwrap();
    fs::write(app_dir.join("src/bias.h"), "#define BIAS 7\n").unwrap();
    fs::write(
        app_dir.join("src/main.c"),
        r#"#include <stdio.h>
int fast_add(int a, int b);

int main(void) {
    printf("%d\n", fast_add(20, 15));
    return 0;
}
"#,
    )
    .unwrap();

    let manifest_path = app_dir.join("Harbour.toml");
    let manifest = fs::read_to_string(&manifest_path).unwrap().replace(
        r#"sources = ["src/**/*.c"]"#,
        r#"sources = ["src/**/*.c", "src/**/*.S"]"#,
    );
    assert!(
        manifest.contains("*.S"),
        "manifest must opt the assembly source in"
    );
    fs::write(&manifest_path, manifest).unwrap();

    harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .success()
        .stderr(predicate::str::contains("Finished"));

    let exe = built_exe_path(&app_dir, "asmapp");
    let out = Command::new(&exe).output().unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "42",
        "20 + 15 + BIAS(7): a wrong answer means the preprocessor never ran \
         on the .S, so the include and define did not apply"
    );

    // A header included *by assembly* must invalidate that object.
    fs::write(app_dir.join("src/bias.h"), "#define BIAS 8\n").unwrap();
    harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .success();

    let out = Command::new(&exe).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "43",
        "changing a header included by the .S must recompile it"
    );
}

/// A dependency whose archive name collides with a system library must not
/// shadow it.
///
/// A package named `c` builds `libc.a`. While Harbour also put each
/// dependency's artifact directory on the linker search path, that `-L`
/// applied to the libraries the compiler driver links implicitly, so the
/// fixture's `libc.a` won over the real C library and the link died on
/// `__libc_start_main` and `printf`. Passing the archive by absolute path
/// with no matching `-L` is what fixes it. The name is the point of the
/// test, not incidental.
#[test]
fn test_dependency_named_c_does_not_shadow_libc() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let lib = tmp.path().join("c");
    fs::create_dir_all(lib.join("src")).unwrap();
    fs::create_dir_all(lib.join("include")).unwrap();
    fs::write(lib.join("include/c.h"), "int c_value(void);\n").unwrap();
    fs::write(lib.join("src/lib.c"), "int c_value(void) { return 7; }\n").unwrap();
    fs::write(
        lib.join("Harbour.toml"),
        r#"[package]
name = "c"
version = "0.1.0"

[targets.c]
kind = "staticlib"
sources = ["src/**/*.c"]

[targets.c.surface.compile.public]
include_dirs = ["include"]
"#,
    )
    .unwrap();

    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");
    harbour(&home)
        .args(["add", "c", "--path", "../c"])
        .current_dir(&app_dir)
        .assert()
        .success();

    // `printf` matters: it is resolved from the real libc, which is what the
    // fixture's `libc.a` used to displace.
    fs::write(
        app_dir.join("src/main.c"),
        r#"#include <stdio.h>
#include "c.h"

int main(void) {
    printf("%d\n", c_value());
    return 0;
}
"#,
    )
    .unwrap();

    harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .success()
        .stderr(predicate::str::contains("Finished"));

    let exe = built_exe_path(&app_dir, "app");
    let out = Command::new(&exe).output().unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "7");
}

/// A library built by a custom recipe can be consumed by a dependent.
///
/// It could not before: the dependent's link line points at
/// `deps/<pkg>-<ver>/lib/lib<target>.a` and nothing told the recipe where
/// that was, so the escape hatch only worked for a root package nobody
/// depended on. `HARBOUR_ARTIFACT_DIR` closes that gap.
///
/// Unix-only: the fixture drives `make`, `cc` and `ar`.
#[test]
#[cfg(not(windows))]
fn test_custom_recipe_library_is_consumable_by_a_dependent() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let lib_dir = tmp.path().join("foreign");
    fs::create_dir_all(lib_dir.join("src")).unwrap();
    fs::create_dir_all(lib_dir.join("include")).unwrap();
    fs::write(
        lib_dir.join("src/answer.c"),
        "int foreign_answer(void) { return 42; }\n",
    )
    .unwrap();
    fs::write(
        lib_dir.join("include/foreign.h"),
        "int foreign_answer(void);\n",
    )
    .unwrap();
    // Tabs matter to make.
    fs::write(
        lib_dir.join("Makefile"),
        "all:\n\tcc -c src/answer.c -o answer.o\n\tar rcs libforeign.a answer.o\n\
         \tmkdir -p \"$(HARBOUR_ARTIFACT_DIR)\"\n\
         \tcp libforeign.a \"$(HARBOUR_ARTIFACT_DIR)/libforeign.a\"\n",
    )
    .unwrap();
    fs::write(
        lib_dir.join("Harbour.toml"),
        r#"[package]
name = "foreign"
version = "0.1.0"

[targets.foreign]
kind = "staticlib"

[targets.foreign.recipe]
type = "custom"

[[targets.foreign.recipe.steps]]
program = "make"
args = ["all"]
cwd = "."
outputs = ["libforeign.a"]

[targets.foreign.surface.compile.public]
include_dirs = ["include"]
"#,
    )
    .unwrap();

    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");
    harbour(&home)
        .args(["add", "foreign", "--path", "../foreign"])
        .current_dir(&app_dir)
        .assert()
        .success();
    fs::write(
        app_dir.join("src/main.c"),
        r#"#include <stdio.h>
#include "foreign.h"

int main(void) {
    printf("%d\n", foreign_answer());
    return 0;
}
"#,
    )
    .unwrap();

    harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .success()
        .stderr(predicate::str::contains("Finished"));

    let exe = built_exe_path(&app_dir, "app");
    let out = Command::new(&exe).output().unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "42",
        "the recipe's archive must reach the dependent's link line"
    );
}

/// A static archive must be recreated, not updated in place.
///
/// `ar r` matches members by file name, so a member whose name is no longer
/// produced survives forever: renaming a source leaves the old object in the
/// archive, and the linker can resolve a symbol from that stale copy instead
/// of the current one.
///
/// This is what produced a wrong program on Windows. When MSVC detection
/// fails between two builds the object extension flips from `.obj` to `.o`,
/// so a freshly compiled object lands under a *new* member name, both copies
/// sit in the archive, and the stale one wins -- the library reported its
/// pre-change behaviour even though every file had just been recompiled.
/// Renaming a source reproduces it on any platform.
#[test]
fn test_archive_does_not_keep_objects_from_removed_sources() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let lib = tmp.path().join("valuelib");
    fs::create_dir_all(lib.join("src")).unwrap();
    fs::create_dir_all(lib.join("include")).unwrap();
    fs::write(lib.join("include/valuelib.h"), "int value(void);\n").unwrap();
    fs::write(lib.join("src/one.c"), "int value(void) { return 1; }\n").unwrap();
    fs::write(
        lib.join("Harbour.toml"),
        r#"[package]
name = "valuelib"
version = "0.1.0"

[targets.valuelib]
kind = "staticlib"
sources = ["src/**/*.c"]

[targets.valuelib.surface.compile.public]
include_dirs = ["include"]
"#,
    )
    .unwrap();

    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");
    harbour(&home)
        .args(["add", "valuelib", "--path", "../valuelib"])
        .current_dir(&app_dir)
        .assert()
        .success();
    fs::write(
        app_dir.join("src/main.c"),
        r#"#include <stdio.h>
#include "valuelib.h"

int main(void) {
    printf("%d\n", value());
    return 0;
}
"#,
    )
    .unwrap();

    build_ok(&home, &app_dir);
    assert_eq!(
        run_built_exe(&app_dir, "app").out(),
        "1",
        "sanity: the first build must link the original definition"
    );
    let archive = built_archive_path(&app_dir, "valuelib");
    assert!(
        archive_member_stems(&archive).contains(&"one".to_string()),
        "sanity: the first build's archive must contain an object for \
         `one.c` -- if this fails the member assertions below prove \
         nothing. Members: {:?}",
        archive_members(&archive)
    );

    // Same symbol, different file name and different answer. The old
    // object's member name is no longer produced by any source.
    fs::remove_file(lib.join("src/one.c")).unwrap();
    fs::write(lib.join("src/two.c"), "int value(void) { return 2; }\n").unwrap();

    build_ok(&home, &app_dir);

    // Assert on the archive's *contents* first, and on the program's
    // behaviour second. Which definition wins symbol resolution is not
    // something a test should depend on -- it is why this bug reproduced
    // only on Windows and only intermittently -- so the primary assertion
    // is that the stale member is not there at all.
    //
    // Compared by stem, not by file name: the object extension is not a
    // stable function of the platform. MSVC detection flakiness flips it
    // between `.obj` and `.o` on the same machine between two builds, which
    // is the very thing that produced this bug, so `one.o` would be the
    // wrong thing to look for.
    let stems = archive_member_stems(&archive);
    let members = archive_members(&archive);
    assert!(
        !stems.contains(&"one".to_string()),
        "the archive must contain only objects for sources that still \
         exist, but `one.c`'s object is still a member: {members:?}\n\
         hint: `ar r` matches members by name, so an archive that is \
         updated rather than recreated keeps objects forever"
    );
    assert_eq!(
        stems,
        vec!["two".to_string()],
        "the renamed source's object must be the archive's only member: {members:?}"
    );

    let out = run_built_exe(&app_dir, "app");
    assert_eq!(
        out.out(),
        "2",
        "`1` means a stale member won symbol resolution; archive members: {members:?}"
    );
}

/// `--message-format json` must put nothing but JSON on stdout.
///
/// Logs used to go to stdout, so INFO records landed interleaved with the
/// JSON-lines output, ANSI escapes included, and anything consuming it broke
/// on the second line. stdout is the data channel; diagnostics belong on
/// stderr.
#[test]
fn test_json_message_format_keeps_stdout_machine_readable() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");

    let out = harbour(&home)
        .args(["build", "--message-format", "json"])
        .current_dir(&app_dir)
        .output()
        .unwrap();
    assert!(out.status.success());

    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut lines = 0;
    for line in stdout.lines().filter(|l| !l.trim().is_empty()) {
        lines += 1;
        assert!(
            serde_json::from_str::<serde_json::Value>(line).is_ok(),
            "every stdout line must parse as JSON, got: {line:?}"
        );
    }
    assert!(lines > 0, "expected some JSON output, got nothing");

    // The diagnostics still have to go somewhere.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("Compiling") || stderr.contains("Finished"),
        "build progress must still be reported on stderr, got: {stderr:?}"
    );
}

/// A per-platform generated header, reached through a conditional
/// `include_dirs`, must resolve while the package is a *dependency*.
///
/// This is the configure-derived `config.h` case: a shim vendors one per
/// platform and points at the right directory from a
/// `[[targets.NAME.when]]` block. Expressing it through that block's
/// `cflags` instead (`-Iharbour-config/<platform>`) compiles here and
/// fails as a dependency, because a bare relative `-I` resolves against
/// the *root* package's working directory. The fixture therefore only ever
/// builds the library through a dependent.
#[test]
fn test_conditional_include_dirs_resolve_relative_to_their_own_package() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let lib = tmp.path().join("cfglib");
    fs::create_dir_all(lib.join("src")).unwrap();
    fs::create_dir_all(lib.join("include")).unwrap();
    // One vendored config per platform; only the matching one is on the
    // include path, so picking the wrong directory changes the answer.
    for (dir, value) in [("this-platform", 7), ("other-platform", 99)] {
        let d = lib.join("harbour-config").join(dir);
        fs::create_dir_all(&d).unwrap();
        fs::write(
            d.join("cfg_generated.h"),
            format!("#define CFG_VALUE {value}\n"),
        )
        .unwrap();
    }
    fs::write(lib.join("include/cfglib.h"), "int cfg_value(void);\n").unwrap();
    fs::write(
        lib.join("src/lib.c"),
        "#include \"cfg_generated.h\"\nint cfg_value(void) { return CFG_VALUE; }\n",
    )
    .unwrap();

    // Condition on the host's own os/arch so the block matches wherever
    // this runs, while the non-matching directory stays unreachable.
    let os = if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "linux"
    };
    fs::write(
        lib.join("Harbour.toml"),
        format!(
            r#"[package]
name = "cfglib"
version = "0.1.0"

[targets.cfglib]
kind = "staticlib"
sources = ["src/**/*.c"]

[targets.cfglib.surface.compile.public]
include_dirs = ["include"]

[[targets.cfglib.when]]
os = "{os}"
include_dirs = ["harbour-config/this-platform"]
"#
        ),
    )
    .unwrap();

    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");
    harbour(&home)
        .args(["add", "cfglib", "--path", "../cfglib"])
        .current_dir(&app_dir)
        .assert()
        .success();
    fs::write(
        app_dir.join("src/main.c"),
        "#include <stdio.h>\n#include \"cfglib.h\"\n\nint main(void) { printf(\"%d\\n\", cfg_value()); return 0; }\n",
    )
    .unwrap();

    harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .success()
        .stderr(predicate::str::contains("Finished"));

    let exe = built_exe_path(&app_dir, "app");
    let out = Command::new(&exe).output().unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "7",
        "the conditional include dir must resolve against cfglib's own root, \
         not the dependent's working directory"
    );
}

/// A source named individually that does not exist is an error; a glob that
/// matches nothing is not.
///
/// Generated manifests list sources one per line -- the harvest tool emits
/// 1082 for openssl -- and a vendored file that failed to ship would
/// otherwise vanish while the defines describing it remained. For openssl
/// that means asserting an assembly implementation exists for a primitive
/// whose object is absent. A glob has to stay permissive, because
/// `src/**/*.S` legitimately matches nothing on a platform without assembly.
#[test]
fn test_missing_named_source_is_an_error_but_empty_glob_is_not() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let dir = tmp.path().join("lib");
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join("src/present.c"),
        "int present(void) { return 1; }\n",
    )
    .unwrap();

    let manifest = |sources: &str| {
        format!(
            r#"[package]
name = "lib"
version = "0.1.0"

[targets.lib]
kind = "staticlib"
sources = {sources}
"#
        )
    };

    // A glob that matches nothing alongside one that matches: fine.
    fs::write(
        dir.join("Harbour.toml"),
        manifest(r#"["src/**/*.c", "src/**/*.S"]"#),
    )
    .unwrap();
    harbour(&home)
        .args(["build"])
        .current_dir(&dir)
        .assert()
        .success();

    // A named file that is absent: rejected, and the message names it.
    fs::write(
        dir.join("Harbour.toml"),
        manifest(r#"["src/present.c", "src/vendored_asm.S"]"#),
    )
    .unwrap();
    harbour(&home)
        .args(["build"])
        .current_dir(&dir)
        .assert()
        .failure()
        .stderr(predicate::str::contains("src/vendored_asm.S"));
}

/// `supports` warns rather than blocking, and names the package.
///
/// The list records what someone has built, not what can build: above the
/// freestanding/hosted line C guarantees nothing, so glibc, musl, MSVC and
/// newlib disagree on POSIX coverage. Rejecting an unlisted triple would mean
/// rejecting working builds as targets proliferate.
#[test]
fn test_unlisted_target_warns_but_still_builds() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let lib = tmp.path().join("declared");
    fs::create_dir_all(lib.join("src")).unwrap();
    fs::create_dir_all(lib.join("include")).unwrap();
    fs::write(lib.join("include/declared.h"), "int dv(void);\n").unwrap();
    fs::write(lib.join("src/lib.c"), "int dv(void) { return 5; }\n").unwrap();
    // A triple no CI runner uses, so the warning fires everywhere.
    fs::write(
        lib.join("Harbour.toml"),
        r#"[package]
name = "declared"
version = "0.1.0"
requires = "hosted"
supports = ["mips64-unknown-linux-gnuabi64"]

[targets.declared]
kind = "staticlib"
sources = ["src/**/*.c"]

[targets.declared.surface.compile.public]
include_dirs = ["include"]
"#,
    )
    .unwrap();

    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");
    harbour(&home)
        .args(["add", "declared", "--path", "../declared"])
        .current_dir(&app_dir)
        .assert()
        .success();
    fs::write(
        app_dir.join("src/main.c"),
        "#include <stdio.h>\n#include \"declared.h\"\n\nint main(void) { printf(\"%d\\n\", dv()); return 0; }\n",
    )
    .unwrap();

    // Warns, names the package, and succeeds anyway.
    harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .success()
        .stderr(predicate::str::contains("does not list"))
        .stderr(predicate::str::contains("declared"));

    let exe = built_exe_path(&app_dir, "app");
    let out = Command::new(&exe).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "5");
}

/// Write a code generator script into `dir` that emits `generated/table.h`
/// (declaring `generated_answer`) and `generated/table.c` (defining it as
/// `value`), and return the `[[targets.app.prebuild]]` block that invokes it.
///
/// The generated symbol is deliberately a variable rather than a function:
/// the script body then contains no parentheses or braces, which keeps the
/// `cmd.exe` and POSIX `sh` versions equivalent without quoting games.
fn write_answer_generator(dir: &std::path::Path, value: i32) -> String {
    if cfg!(windows) {
        fs::write(
            dir.join("gen.cmd"),
            format!(
                "@echo off\r\n\
                 if not exist generated mkdir generated\r\n\
                 >generated\\table.h echo extern int generated_answer;\r\n\
                 >generated\\table.c echo int generated_answer = {value};\r\n"
            ),
        )
        .unwrap();
        "[[targets.app.prebuild]]\n\
         program = \"cmd\"\n\
         args = [\"/C\", \"gen.cmd\"]\n\
         outputs = [\"generated/table.c\", \"generated/table.h\"]\n"
            .to_string()
    } else {
        fs::write(
            dir.join("gen.sh"),
            format!(
                "#!/bin/sh\n\
                 mkdir -p generated\n\
                 echo 'extern int generated_answer;' > generated/table.h\n\
                 echo 'int generated_answer = {value};' > generated/table.c\n"
            ),
        )
        .unwrap();
        "[[targets.app.prebuild]]\n\
         program = \"sh\"\n\
         args = [\"gen.sh\"]\n\
         outputs = [\"generated/table.c\", \"generated/table.h\"]\n"
            .to_string()
    }
}

/// Lay out an `app` package whose `[[targets.app.prebuild]]` generator emits
/// a source that the target compiles, with `sources` written as `sources_toml`.
fn write_codegen_app(app_dir: &std::path::Path, sources_toml: &str, value: i32) {
    let prebuild = write_answer_generator(app_dir, value);
    fs::write(
        app_dir.join("Harbour.toml"),
        format!(
            "[package]\n\
             name = \"app\"\n\
             version = \"0.1.0\"\n\
             \n\
             [targets.app]\n\
             kind = \"bin\"\n\
             sources = {sources_toml}\n\
             \n\
             [targets.app.private]\n\
             include_dirs = [\"generated\"]\n\
             \n\
             {prebuild}"
        ),
    )
    .unwrap();

    fs::write(
        app_dir.join("src/main.c"),
        "#include <stdio.h>\n\
         #include \"table.h\"\n\
         \n\
         int main(void) { printf(\"%d\\n\", generated_answer); return 0; }\n",
    )
    .unwrap();
}

#[test]
fn test_prebuild_generated_source_is_compiled_on_clean_build() {
    // Regression coverage for "run a code generator, then compile its
    // output" being broken on a *clean* build only.
    //
    // Source globs were expanded while the plan was built, but pre-build
    // steps ran later, during execution. So on a fresh checkout
    // `generated/*.c` matched nothing, the generated translation unit was
    // absent from the plan, and the link failed on the symbol it defines --
    // while the very next build succeeded, because by then the generator had
    // left the file on disk. That asymmetry is why this has to assert on
    // both builds, and on what the binary *prints*, not on exit status: for
    // a `staticlib` target the same bug produces a successful build and an
    // archive quietly missing a member.
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");

    write_codegen_app(&app_dir, r#"["src/**/*.c", "generated/*.c"]"#, 42);

    // Build 1: clean. Nothing under `generated/` exists yet; only the
    // generator knows what will be there.
    build_ok(&home, &app_dir);
    assert_eq!(
        run_built_exe(&app_dir, "app").out(),
        "42",
        "the generated translation unit must be compiled into the *clean* build"
    );

    // Build 2: incremental, nothing changed. The generator re-runs and
    // rewrites byte-identical output, which must not force a recompile.
    //
    // Asserted against the build tree rather than against the log. The
    // previous form -- `rebuild_log.contains("up to date")` -- could not
    // fail: the compiling line reads `Compiling 1 file(s) (0 up to date)`,
    // so the needle is present even when every file was recompiled, which
    // is exactly the thing this build is supposed to rule out.
    rebuild(&home, &app_dir).assert_reused_everything(
        "the generator re-ran and rewrote byte-identical output, so nothing \
         may be recompiled",
    );
    assert_eq!(run_built_exe(&app_dir, "app").out(), "42");

    // Build 3: the generator now emits a different value. The fingerprint of
    // the generated source is taken after regeneration, so this must
    // recompile and change what the program prints.
    write_answer_generator(&app_dir, 99);
    let (log, diff) = rebuild_and_diff(&home, &app_dir);
    diff.assert_touched(
        "table",
        "the generated source changed, so its object must be recompiled",
    );
    assert_eq!(
        run_built_exe(&app_dir, "app").out(),
        "99",
        "regenerated output must be recompiled, not served from the \
         fingerprint cache\n{log}\n{diff}"
    );
}

#[test]
fn test_prebuild_generated_source_named_explicitly_is_compiled() {
    // A generated source listed individually rather than matched by a glob.
    //
    // Naming a source that does not exist is normally a hard error, and this
    // used to need an exemption for targets with `prebuild`, because at plan
    // time the generator had not run and its output legitimately wasn't
    // there yet. Now that generators run before sources are resolved, the
    // file *is* present and no exemption is needed -- so this must build,
    // and the existence check can stay strict for everyone.
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");

    write_codegen_app(&app_dir, r#"["src/main.c", "generated/table.c"]"#, 7);

    // Built twice on purpose: the ordering bug this covers was visible only
    // on the clean build, and the second build must additionally reuse the
    // generated object rather than recompiling it because the generator
    // re-ran.
    build_twice(&home, &app_dir).assert_incremental_is_a_no_op();
    assert_eq!(
        run_built_exe(&app_dir, "app").out(),
        "7",
        "the explicitly named generated source must be compiled and linked"
    );
}

#[test]
fn test_prebuild_that_skips_a_declared_output_fails_loudly() {
    // A generator that exits 0 without writing what its `outputs` declares
    // is the failure this ordering fix exists to surface. Before, the named
    // source silently vanished from the compile set; now the build stops at
    // the generator, naming it and the file it owes.
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");

    write_codegen_app(&app_dir, r#"["src/**/*.c", "generated/*.c"]"#, 42);

    // Replace the generator with one that only writes the header.
    if cfg!(windows) {
        fs::write(
            app_dir.join("gen.cmd"),
            "@echo off\r\n\
             if not exist generated mkdir generated\r\n\
             >generated\\table.h echo extern int generated_answer;\r\n",
        )
        .unwrap();
    } else {
        fs::write(
            app_dir.join("gen.sh"),
            "#!/bin/sh\n\
             mkdir -p generated\n\
             echo 'extern int generated_answer;' > generated/table.h\n",
        )
        .unwrap();
    }

    harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .failure()
        .stderr(predicate::str::contains("did not produce"))
        .stderr(predicate::str::contains("table.c"));
}

/// Write a generator script named `stem` into `dir` that emits
/// `generated/<stem>.c` defining `int <symbol> = <value>;`, and return the
/// `program`/`args` TOML fragment that invokes it.
fn write_symbol_generator(dir: &std::path::Path, stem: &str, symbol: &str, value: i32) -> String {
    if cfg!(windows) {
        fs::write(
            dir.join(format!("{stem}.cmd")),
            format!(
                "@echo off\r\n\
                 if not exist generated mkdir generated\r\n\
                 >generated\\{stem}.c echo int {symbol} = {value};\r\n"
            ),
        )
        .unwrap();
        format!("program = \"cmd\"\nargs = [\"/C\", \"{stem}.cmd\"]\n")
    } else {
        fs::write(
            dir.join(format!("{stem}.sh")),
            format!(
                "#!/bin/sh\n\
                 mkdir -p generated\n\
                 echo 'int {symbol} = {value};' > generated/{stem}.c\n"
            ),
        )
        .unwrap();
        format!("program = \"sh\"\nargs = [\"{stem}.sh\"]\n")
    }
}

/// A generator that always fails. Used as a tripwire: if a non-matching
/// `when` block's generator runs, the build dies and the test says so.
fn write_failing_generator(dir: &std::path::Path, stem: &str) -> String {
    if cfg!(windows) {
        fs::write(
            dir.join(format!("{stem}.cmd")),
            "@echo off\r\necho this generator must not run 1>&2\r\nexit /b 1\r\n",
        )
        .unwrap();
        format!("program = \"cmd\"\nargs = [\"/C\", \"{stem}.cmd\"]\n")
    } else {
        fs::write(
            dir.join(format!("{stem}.sh")),
            "#!/bin/sh\necho 'this generator must not run' >&2\nexit 1\n",
        )
        .unwrap();
        format!("program = \"sh\"\nargs = [\"{stem}.sh\"]\n")
    }
}

/// The `os` value Harbour evaluates `[[targets.X.when]]` against on this host.
fn host_os_condition() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

#[test]
fn test_conditional_prebuild_runs_only_the_matching_generator() {
    // `prebuild` used to be a plain `Vec<CustomCommand>` on the target with
    // no `when` support, so a per-platform generator was inexpressible --
    // and a generator is often the *most* platform-specific thing a package
    // does. openssl runs perlasm scripts with `flavour elf` on Linux x86_64
    // and a different set with `flavour macosx` on Darwin; there is no
    // single script to run unconditionally.
    //
    // Three generators here: one unconditional, one behind a `when` that
    // matches this host, and one behind a `when` that cannot match. The
    // program's output proves the first two ran and were *compiled in*; the
    // third is rigged to exit non-zero, so the build succeeding at all
    // proves it was skipped rather than merely tolerated.
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");

    let base = write_symbol_generator(&app_dir, "base", "base_value", 1);
    let plat = write_symbol_generator(&app_dir, "plat", "plat_value", 10);
    let never = write_failing_generator(&app_dir, "never");

    fs::write(
        app_dir.join("Harbour.toml"),
        format!(
            "[package]\n\
             name = \"app\"\n\
             version = \"0.1.0\"\n\
             \n\
             [targets.app]\n\
             kind = \"bin\"\n\
             sources = [\"src/**/*.c\", \"generated/*.c\"]\n\
             \n\
             [[targets.app.prebuild]]\n\
             {base}\
             outputs = [\"generated/base.c\"]\n\
             \n\
             [[targets.app.when]]\n\
             os = \"{os}\"\n\
             \n\
             [[targets.app.when.prebuild]]\n\
             {plat}\
             outputs = [\"generated/plat.c\"]\n\
             \n\
             [[targets.app.when]]\n\
             arch = \"s390x\"\n\
             \n\
             [[targets.app.when.prebuild]]\n\
             {never}\
             outputs = [\"generated/never.c\"]\n",
            os = host_os_condition()
        ),
    )
    .unwrap();

    fs::write(
        app_dir.join("src/main.c"),
        "#include <stdio.h>\n\
         \n\
         extern int base_value;\n\
         extern int plat_value;\n\
         \n\
         int main(void) { printf(\"%d\\n\", base_value + plat_value); return 0; }\n",
    )
    .unwrap();

    // Clean build. If the matching `when` generator were ignored,
    // `plat_value` would be undefined and this would fail to link.
    harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .success();

    let exe = built_exe_path(&app_dir, "app");
    let out = Command::new(&exe).output().unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "11",
        "both the unconditional and the matching conditional generator must \
         run and have their output compiled in"
    );

    // The non-matching generator must never have been invoked.
    assert!(
        !app_dir.join("generated/never.c").exists(),
        "a `when` block whose condition does not match must not run its generator"
    );

    // Second build, for the same reason the unconditional case checks it:
    // this class of bug passes on one build and fails on the other.
    harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .success();
    let out = Command::new(&exe).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "11");
}

/// A `surface.when` block's private requirements must reach the compiler,
/// and an unrecognised key in one must be rejected.
///
/// `ConditionalSurface` had only `compile.public`/`link.public`, and its
/// condition fields are `#[serde(flatten)]`ed, so serde absorbed
/// `compile.private` as a condition it did not recognise: the table parsed
/// cleanly and did nothing. `harbour new` scaffolds `-Wall -Wextra` (and
/// `/W4` for MSVC) into exactly that table, so no generated project had
/// ever been compiled with warnings enabled.
#[test]
fn test_conditional_private_requirements_reach_the_compiler() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");

    // The scaffold's own warning flags, unmodified.
    harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .success();

    let cc = fs::read_to_string(app_dir.join(".harbour/compile_commands.json")).unwrap();
    let expected = if cfg!(target_env = "msvc") {
        "/W4"
    } else {
        "-Wall"
    };
    assert!(
        cc.contains(expected),
        "the scaffold declares {expected} in a `surface.when` block's \
         compile.private; it must reach the compiler. compile_commands.json:\n{cc}"
    );

    // A key that is neither a condition nor a requirement table is a
    // mistake, and must not be absorbed as an unknown condition.
    let manifest = app_dir.join("Harbour.toml");
    let text = fs::read_to_string(&manifest)
        .unwrap()
        .replace("compile.private", "compile.privat");
    fs::write(&manifest, text).unwrap();

    harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .failure()
        .stderr(predicate::str::contains("compile.privat"));
}

// ============================================================================
// Cross-compiling with the host clang and an explicit -target
// ============================================================================

/// A real bare-metal cross build with no cross toolchain installed: the host
/// clang, `-target aarch64-none-elf`, and an archiver that survived
/// validation.
///
/// The assertion is on the *artifact*, not on the exit status: the bug this
/// route invites is a build that reports success while producing an archive
/// with no members at all (macOS `ar` drops ELF members and still exits 0),
/// or objects quietly built for the host. So the archive is opened and its
/// ELF header read: `e_machine` must be `EM_AARCH64`.
///
/// Skipped rather than failed when the host cannot do this at all -- no
/// clang, a clang without the AArch64 backend, or no archiver that keeps ELF
/// members (a stock macOS install has no `llvm-ar`). Those are properties of
/// the machine, not regressions.
#[test]
fn cross_builds_a_static_lib_with_host_clang_and_target_flag() {
    use harbour::builder::toolchain::{probe_host_clang, HostClangProbe};
    use harbour::core::target::TargetTriple;

    let triple = "aarch64-none-elf";
    if !matches!(
        probe_host_clang(&TargetTriple::parse(triple)),
        HostClangProbe::Ready { .. }
    ) {
        eprintln!("skipping: this host's clang cannot build for {triple}");
        return;
    }

    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let root = tmp.path().join("bare");

    harbour(&home)
        .current_dir(tmp.path())
        .args(["new", "--lib", "bare"])
        .assert()
        .success();

    // No `#include`: host clang has no sysroot for a bare-metal target, and
    // supplying one (or `-ffreestanding`/`-nostdlib`) is a build-flag
    // concern handled separately from discovery.
    fs::write(
        root.join("src/lib.c"),
        "int bare_add(int a, int b) { return a + b; }\n",
    )
    .unwrap();
    fs::write(
        root.join("include/bare/bare.h"),
        "#ifndef BARE_H\n#define BARE_H\nint bare_add(int a, int b);\n#endif\n",
    )
    .unwrap();

    harbour(&home)
        .current_dir(&root)
        .args(["build", "--target-triple", triple])
        .assert()
        .success();

    // Cross builds get their own output tree, keyed on the canonical triple.
    let archive = root
        .join(".harbour/target")
        .join(TargetTriple::parse(triple).canonical())
        .join("debug/lib/libbare.a");
    assert!(
        archive.exists(),
        "expected a cross-built archive at {}",
        archive.display()
    );

    let bytes = fs::read(&archive).unwrap();
    let elf = bytes
        .windows(4)
        .position(|w| w == b"\x7fELF")
        .unwrap_or_else(|| {
            panic!(
                "archive contains no ELF member at all ({} bytes) -- the \
                 archiver dropped the object while reporting success",
                bytes.len()
            )
        });
    // ELF header: e_machine is a little-endian u16 at offset 18. 0xB7 is
    // EM_AARCH64; a host build on x86_64 would read 0x3E, and on an Apple
    // host there would be no ELF magic to find in the first place.
    let e_machine = u16::from_le_bytes([bytes[elf + 18], bytes[elf + 19]]);
    assert_eq!(
        e_machine, 0xB7,
        "archived object is not AArch64 (e_machine {e_machine:#x})"
    );
}

/// `-ffreestanding` has to reach the *compiler*, and the only honest proof
/// is a compiled artifact that behaves differently. `__STDC_HOSTED__` is
/// exactly that: the C standard has a freestanding implementation define it
/// as `0` and a hosted one as `1`, so the library's own return value
/// distinguishes "the flag was passed" from "the flag was declared,
/// deduplicated and reported but never handed to `cc`".
///
/// A static library is deliberately the subject: it has a compile step and
/// no link step, so this isolates the compile half from `-nostdlib`, and it
/// pins the decision that `freestanding` is accepted on `staticlib` (where
/// it changes how the library compiles) while `linker_script`/`entry` are
/// not (nothing would ever read them).
/// Gated off MSVC, which rejects all three keys up front (its equivalents
/// are unwired and it has no linker-script concept); the rejection itself
/// is covered by
/// `test_freestanding_keys_are_rejected_under_msvc`.
#[cfg(not(target_env = "msvc"))]
#[test]
fn test_freestanding_staticlib_is_compiled_without_a_hosted_libc() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let lib = tmp.path().join("barelib");
    fs::create_dir_all(lib.join("src")).unwrap();
    fs::create_dir_all(lib.join("include")).unwrap();
    fs::write(lib.join("include/barelib.h"), "int hosted_level(void);\n").unwrap();
    fs::write(
        lib.join("src/lib.c"),
        "#include \"barelib.h\"\nint hosted_level(void) { return __STDC_HOSTED__; }\n",
    )
    .unwrap();
    fs::write(
        lib.join("Harbour.toml"),
        r#"[package]
name = "barelib"
version = "0.1.0"
requires = "freestanding"

[targets.barelib]
kind = "staticlib"
sources = ["src/**/*.c"]
freestanding = true

[targets.barelib.surface.compile.public]
include_dirs = ["include"]
"#,
    )
    .unwrap();

    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");
    harbour(&home)
        .args(["add", "barelib", "--path", "../barelib"])
        .current_dir(&app_dir)
        .assert()
        .success();
    fs::write(
        app_dir.join("src/main.c"),
        "#include <stdio.h>\n#include \"barelib.h\"\n\n\
         int main(void) { printf(\"%d %d\\n\", hosted_level(), __STDC_HOSTED__); return 0; }\n",
    )
    .unwrap();

    harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .success();

    let exe = built_exe_path(&app_dir, "app");
    let out = Command::new(&exe).output().unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "0 1",
        "the library must be compiled -ffreestanding (__STDC_HOSTED__ == 0) \
         while the hosted app that consumes it is not: `freestanding` is a \
         per-target build mode, not a graph-wide one"
    );
}

/// The link line a freestanding target produces must be anchored to the
/// package root, and `harbour flags`/`harbour linkplan` must both report
/// exactly what the linker will get.
///
/// Run from a *subdirectory* on purpose. `linker_script` is resolved against
/// the package root, and the failure mode being guarded against is a
/// relative path silently resolved against the process working directory --
/// which happens to be right for the root package and wrong the moment the
/// package is a dependency. That has already been fixed twice here, for
/// `include_dirs` in a `when` block and for a recipe's `source_dir`.
/// Gated off MSVC, which rejects all three keys up front (its equivalents
/// are unwired and it has no linker-script concept); the rejection itself
/// is covered by
/// `test_freestanding_keys_are_rejected_under_msvc`.
#[cfg(not(target_env = "msvc"))]
#[test]
fn test_freestanding_link_line_is_package_rooted_and_reported_consistently() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "payload"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let dir = tmp.path().join("payload");
    fs::create_dir_all(dir.join("boot")).unwrap();
    fs::write(
        dir.join("boot/layout.ld"),
        "ENTRY(_start)\nSECTIONS { . = 0x40000000; .text : { *(.text) } }\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/main.c"),
        "void _start(void) { for (;;) { } }\n",
    )
    .unwrap();
    fs::write(
        dir.join("Harbour.toml"),
        r#"[package]
name = "payload"
version = "0.1.0"
requires = "freestanding"

[targets.payload]
kind = "exe"
sources = ["src/**/*.c"]
freestanding = true
linker_script = "boot/layout.ld"
entry = "_start"
"#,
    )
    .unwrap();

    // Not canonicalized on purpose: temp directories are symlinked on macOS
    // (`/var` -> `/private/var`), so the exact prefix is not the point --
    // *which package directory the tail hangs off* is.
    //
    // Separators are flattened because the reported path legitimately mixes
    // them on Windows: `Path::join` appends the platform separator and
    // leaves the one already inside `boot/layout.ld` alone, so the flag
    // reads `-Wl,-T,C:\...\payload\boot/layout.ld`. The property under test
    // is the anchoring, not the spelling.
    let flat = |s: &str| s.replace('\\', "/");
    let wanted = "payload/boot/layout.ld";
    let cwd_relative = "payload/src/boot";

    // `src/` is not the package root: a cwd-relative resolution would
    // produce `<pkg>/src/boot/layout.ld`, or nothing at all.
    for cmd in ["flags", "linkplan"] {
        let out = harbour(&home)
            .args([cmd, "payload"])
            .current_dir(dir.join("src"))
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let stdout = flat(&String::from_utf8_lossy(&out));

        for expected in ["-nostdlib", "-Wl,--entry=_start", "-Wl,-T,", wanted] {
            assert!(
                stdout.contains(expected),
                "`harbour {cmd}` must report `{expected}`; got:\n{stdout}"
            );
        }
        assert!(
            !stdout.contains(cwd_relative),
            "the linker script must not be resolved against the working \
             directory; got:\n{stdout}"
        );
    }
}

/// A dependency's relative `linker_script` must be looked for inside *that
/// dependency*, not wherever `harbour` was run.
///
/// The script here exists only in the dependent's root, so a cwd-relative
/// resolution would find it and the build would proceed with the wrong
/// file. Correct behaviour is to fail, naming the dependency's own root.
/// Checked through the error message rather than a completed link because
/// no host linker can finish a freestanding link (see [`bare_metal_cc`]).
/// Gated off MSVC, which rejects all three keys up front (its equivalents
/// are unwired and it has no linker-script concept); the rejection itself
/// is covered by
/// `test_freestanding_keys_are_rejected_under_msvc`.
#[cfg(not(target_env = "msvc"))]
#[test]
fn test_a_dependencys_linker_script_is_looked_for_in_that_dependency() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let dep = tmp.path().join("payload");
    fs::create_dir_all(dep.join("src")).unwrap();
    fs::write(
        dep.join("src/start.c"),
        "void _start(void) { for (;;) { } }\n",
    )
    .unwrap();
    fs::write(
        dep.join("Harbour.toml"),
        r#"[package]
name = "payload"
version = "0.1.0"
requires = "freestanding"

[targets.payload]
kind = "exe"
sources = ["src/**/*.c"]
freestanding = true
linker_script = "layout.ld"
"#,
    )
    .unwrap();

    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");
    // Present in the *dependent*, absent in the dependency that names it.
    fs::write(app_dir.join("layout.ld"), "ENTRY(_start)\n").unwrap();
    harbour(&home)
        .args(["add", "payload", "--path", "../payload"])
        .current_dir(&app_dir)
        .assert()
        .success();

    harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .failure()
        .stderr(predicate::str::contains("layout.ld").and(predicate::str::contains("payload")));
}

/// The one test that performs a real freestanding link, and the only place
/// the flags are proven to be *accepted* rather than merely emitted.
///
/// Skipped unless a bare-metal cross driver is installed, because there is
/// no way to fake it: the host linker on macOS rejects both `-nostdlib` and
/// `-T`, so a version of this test that "passed" everywhere would be
/// testing nothing.
#[test]
fn test_freestanding_image_links_with_a_bare_metal_toolchain() {
    let Some((triple, cc)) = bare_metal_cc() else {
        eprintln!(
            "skipping: no bare-metal cross toolchain installed \
             (x86_64-elf-gcc / aarch64-elf-gcc / arm-none-eabi-gcc / \
             riscv64-unknown-elf-gcc); the freestanding link is NOT verified"
        );
        return;
    };
    eprintln!("running the freestanding link against {cc} for {triple}");

    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "payload"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let dir = tmp.path().join("payload");
    fs::write(
        dir.join("layout.ld"),
        "ENTRY(_start)\nSECTIONS { . = 0x100000; .text : { *(.text*) } \
         .rodata : { *(.rodata*) } .data : { *(.data*) } .bss : { *(.bss*) } }\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/main.c"),
        "void _start(void) { volatile int spin = __STDC_HOSTED__; while (spin == 0) { } }\n",
    )
    .unwrap();
    fs::write(
        dir.join("Harbour.toml"),
        r#"[package]
name = "payload"
version = "0.1.0"
requires = "freestanding"

[targets.payload]
kind = "exe"
sources = ["src/**/*.c"]
freestanding = true
linker_script = "layout.ld"
entry = "_start"
"#,
    )
    .unwrap();

    harbour(&home)
        .args(["build", "--target-triple", triple])
        .current_dir(&dir)
        .assert()
        .success();
}

/// The counterpart to the three tests above: on MSVC these keys must be
/// *refused*, not quietly dropped.
///
/// Worth a test of its own rather than leaving MSVC as a hole in the
/// coverage. `-ffreestanding`/`-nostdlib`/`-Wl,-T,` are GCC-driver
/// spellings; `cl`/`link.exe` have neither those nor any linker-script
/// concept, and their nearest equivalents (`/NODEFAULTLIB`, `/ENTRY:`) are
/// not wired. Passing the flags through would fail deep inside the compiler,
/// and dropping them would produce a hosted binary from a manifest asking
/// for a freestanding one.
#[cfg(target_env = "msvc")]
#[test]
fn test_freestanding_keys_are_rejected_under_msvc() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "payload"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let dir = tmp.path().join("payload");
    fs::write(dir.join("layout.ld"), "ENTRY(_start)\n").unwrap();
    fs::write(
        dir.join("src/main.c"),
        "void _start(void) { for (;;) { } }\n",
    )
    .unwrap();
    fs::write(
        dir.join("Harbour.toml"),
        r#"[package]
name = "payload"
version = "0.1.0"
requires = "freestanding"

[targets.payload]
kind = "exe"
sources = ["src/**/*.c"]
freestanding = true
linker_script = "layout.ld"
entry = "_start"
"#,
    )
    .unwrap();

    harbour(&home)
        .args(["build"])
        .current_dir(&dir)
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("freestanding")
                .and(predicate::str::contains("GCC/Clang"))
                .and(predicate::str::contains("linker_script")),
        );
}

/// A `prebuild` generator may emit the linker script itself, and the
/// plan-time existence check must see it.
///
/// This is a direct interaction with the prebuild-ordering change: since
/// generators now run *during planning*, before a target's sources are
/// resolved, they also run before the linker-script check further down the
/// same loop. Under the previous ordering -- generators ran at the start of
/// `NativeBuilder::execute`, i.e. after the whole plan was built -- this
/// check would have rejected a generated script that had not been written
/// yet. Templating a script with memory sizes is ordinary bare-metal
/// practice, so the combination is worth pinning rather than rediscovering.
#[cfg(not(target_env = "msvc"))]
#[test]
fn test_a_generated_linker_script_is_visible_to_the_plan_time_check() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "payload"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let dir = tmp.path().join("payload");
    fs::write(
        dir.join("src/main.c"),
        "void _start(void) { for (;;) { } }\n",
    )
    .unwrap();

    // The script is *not* in the checkout: only the generator can produce it.
    let prebuild = if cfg!(windows) {
        fs::write(
            dir.join("gen.cmd"),
            "@echo off\r\n>layout.ld echo ENTRY^(_start^)\r\n",
        )
        .unwrap();
        "[[targets.payload.prebuild]]\n\
         program = \"cmd\"\n\
         args = [\"/C\", \"gen.cmd\"]\n\
         outputs = [\"layout.ld\"]\n"
    } else {
        fs::write(
            dir.join("gen.sh"),
            "#!/bin/sh\necho 'ENTRY(_start)' > layout.ld\n",
        )
        .unwrap();
        "[[targets.payload.prebuild]]\n\
         program = \"sh\"\n\
         args = [\"gen.sh\"]\n\
         outputs = [\"layout.ld\"]\n"
    };

    fs::write(
        dir.join("Harbour.toml"),
        format!(
            r#"[package]
name = "payload"
version = "0.1.0"
requires = "freestanding"

[targets.payload]
kind = "exe"
sources = ["src/**/*.c"]
freestanding = true
linker_script = "layout.ld"

{prebuild}"#
        ),
    )
    .unwrap();

    assert!(
        !dir.join("layout.ld").exists(),
        "the script must not exist before the generator runs, or this test \
         proves nothing"
    );

    // `linkplan` builds the plan without linking, which is exactly the phase
    // under test -- and the only phase reachable without a bare-metal linker.
    harbour(&home)
        .args(["linkplan", "payload"])
        .current_dir(&dir)
        .assert()
        .success()
        .stdout(predicate::str::contains("-Wl,-T,"));

    assert!(
        dir.join("layout.ld").exists(),
        "the generator should have produced the script during planning"
    );
}

/// A target-level freestanding flag and a `surface.when` block's private
/// ldflags must both reach the link line, and be attributed separately.
///
/// These are two different mechanisms landing in the same sorted, deduped
/// `ldflags` list, so it is worth pinning that neither displaces the other.
/// The combination is also the documented escape hatch for Apple and other
/// hosts whose default linker cannot do a freestanding link: `-fuse-ld=lld`
/// belongs in a conditional private link surface, while `-nostdlib` comes
/// from the target key. Note this only became testable once private
/// requirements in a `when` block were actually applied -- they used to
/// parse and do nothing.
#[cfg(not(target_env = "msvc"))]
#[test]
fn test_conditional_private_ldflags_compose_with_target_level_freestanding() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "payload"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let dir = tmp.path().join("payload");
    fs::write(dir.join("layout.ld"), "ENTRY(_start)\n").unwrap();
    fs::write(
        dir.join("src/main.c"),
        "void _start(void) { for (;;) { } }\n",
    )
    .unwrap();

    // Condition on the host's own os so the block matches wherever this runs.
    let os = if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "linux"
    };
    fs::write(
        dir.join("Harbour.toml"),
        format!(
            r#"[package]
name = "payload"
version = "0.1.0"
requires = "freestanding"

[targets.payload]
kind = "exe"
sources = ["src/**/*.c"]
freestanding = true
linker_script = "layout.ld"

[[targets.payload.surface.when]]
os = "{os}"
[targets.payload.surface.when."link.private"]
ldflags = ["-fuse-ld=lld"]
"#
        ),
    )
    .unwrap();

    let out = harbour(&home)
        .args(["flags", "payload", "--link"])
        .current_dir(&dir)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8_lossy(&out).to_string();

    for expected in ["-nostdlib", "-Wl,-T,", "-fuse-ld=lld"] {
        assert!(
            stdout.contains(expected),
            "`{expected}` must reach the link line; got:\n{stdout}"
        );
    }
    // Attribution must distinguish the two mechanisms, so that `harbour
    // flags` stays a usable answer to "where did this flag come from".
    assert!(
        stdout.contains("target config"),
        "the target keys must be attributed to the target, got:\n{stdout}"
    );
    assert!(
        stdout.contains("surface.link.private"),
        "the `when` block's ldflags must be attributed to the surface, got:\n{stdout}"
    );

    // And the real link line agrees with what `flags` just reported.
    harbour(&home)
        .args(["linkplan", "payload"])
        .current_dir(&dir)
        .assert()
        .success()
        .stdout(
            predicate::str::contains("-nostdlib")
                .and(predicate::str::contains("-fuse-ld=lld"))
                .and(predicate::str::contains("-Wl,-T,")),
        );
}

/// A cross toolchain that can actually produce a freestanding ELF image, if
/// one happens to be installed.
///
/// The host toolchain is not a substitute. Apple's `ld` has no `-T` and
/// refuses `-nostdlib` links outright ("dynamic executables or dylibs must
/// link with libSystem.dylib"), so on macOS there is no way to complete a
/// freestanding link at all; on Linux the host GCC can, which is why this
/// looks for a bare-metal driver rather than assuming.
fn bare_metal_cc() -> Option<(&'static str, &'static str)> {
    for (triple, cc) in [
        ("x86_64-unknown-none", "x86_64-elf-gcc"),
        ("aarch64-unknown-none", "aarch64-elf-gcc"),
        ("arm-none-eabi", "arm-none-eabi-gcc"),
        ("riscv64-unknown-none-elf", "riscv64-unknown-elf-gcc"),
    ] {
        // Probed by running it rather than by looking it up on PATH: the
        // question is whether a working driver exists, and `which` is not
        // in this test crate's dependency graph.
        let usable = Command::new(cc)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        if usable {
            return Some((triple, cc));
        }
    }
    None
}

// ============================================================================
// Declared-but-unpassed link inputs
// ============================================================================

/// A framework declared on a dependency's public link surface must appear on
/// the link line the linker actually receives.
///
/// `surface.link.public.frameworks` was parsed, propagated across the
/// dependency graph, deduplicated, and printed by `harbour flags` and by the
/// top half of `harbour linkplan` -- while `LinkStep`/`LinkInput` had no
/// field for it, so the linker never saw it. Every existing test passed:
/// nothing linked a framework, and the surface-level output agreed with
/// itself.
///
/// The two halves of `linkplan` are what make this checkable on every
/// platform. The first walks the surface; the "Link line" section is
/// rendered from the `LinkStep` the builder executes. Asserting on the
/// second is asserting on what gets passed. The framework name is
/// deliberately fictional -- linking it would fail, and this is about
/// whether the input is *carried*, not whether macOS has it.
#[test]
fn test_declared_framework_reaches_the_link_line() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let lib = tmp.path().join("flib");
    fs::create_dir_all(lib.join("src")).unwrap();
    fs::create_dir_all(lib.join("include")).unwrap();
    fs::write(lib.join("include/flib.h"), "int flib_value(void);\n").unwrap();
    fs::write(
        lib.join("src/flib.c"),
        "int flib_value(void) { return 1; }\n",
    )
    .unwrap();
    fs::write(
        lib.join("Harbour.toml"),
        r#"[package]
name = "flib"
version = "0.1.0"

[targets.flib]
kind = "staticlib"
sources = ["src/**/*.c"]

[targets.flib.surface.compile.public]
include_dirs = ["include"]

[targets.flib.surface.link.public]
frameworks = ["HarbourTestFramework"]
"#,
    )
    .unwrap();

    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");
    harbour(&home)
        .args(["add", "flib", "--path", "../flib"])
        .current_dir(&app_dir)
        .assert()
        .success();

    let plan = harbour_run(&home, &app_dir, &["linkplan", "app"]).success();
    let stdout = plan.stdout.clone();
    let (surface_section, link_line) = stdout
        .split_once("Link line (what the linker receives")
        .unwrap_or_else(|| panic!("linkplan printed no link line section\n{plan}"));

    assert!(
        surface_section.contains("-framework HarbourTestFramework"),
        "sanity: the resolved surface must carry the framework, otherwise \
         the assertion below is testing nothing\n{plan}"
    );
    assert_eq!(
        link_line.matches("-framework HarbourTestFramework").count(),
        1,
        "a framework declared on a dependency's public link surface must be \
         passed to the linker exactly once. Appearing in the surface listing \
         but not on the link line is the exact shape of the bug this covers: \
         parsed, propagated, printed -- and dropped before the link.\n{plan}"
    );

    // `harbour flags` reads the surface, so it must agree with the link line
    // rather than being the only place the framework shows up.
    let flags = harbour_run(&home, &app_dir, &["flags", "app"]).success();
    assert!(
        flags.stdout.contains("HarbourTestFramework"),
        "`harbour flags` and the link line must not disagree\n{flags}"
    );
}

// ============================================================================
// Incremental rebuilds across a dependency graph
// ============================================================================

/// A staticlib whose single function returns `value`, plus a public header
/// exposing that value as a macro so tests can invalidate either the
/// dependency's *implementation* or its *interface*.
fn write_answer_lib(dir: &std::path::Path, value: i32, macro_value: i32) {
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::create_dir_all(dir.join("include")).unwrap();
    fs::write(
        dir.join("include/answerlib.h"),
        format!("#define ANSWER_BONUS {macro_value}\nint answer(void);\n"),
    )
    .unwrap();
    fs::write(
        dir.join("src/answer.c"),
        format!("int answer(void) {{ return {value}; }}\n"),
    )
    .unwrap();
    fs::write(
        dir.join("Harbour.toml"),
        r#"[package]
name = "answerlib"
version = "0.1.0"

[targets.answerlib]
kind = "staticlib"
sources = ["src/**/*.c"]

[targets.answerlib.surface.compile.public]
include_dirs = ["include"]
"#,
    )
    .unwrap();
}

/// An app that prints `answer() + ANSWER_BONUS`, so its output is sensitive
/// to both halves of the dependency.
fn write_answer_app(home: &std::path::Path, tmp: &TempDir) -> PathBuf {
    harbour(home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");
    harbour(home)
        .args(["add", "answerlib", "--path", "../answerlib"])
        .current_dir(&app_dir)
        .assert()
        .success();
    fs::write(
        app_dir.join("src/main.c"),
        r#"#include <stdio.h>
#include "answerlib.h"

int main(void) {
    printf("%d\n", answer() + ANSWER_BONUS);
    return 0;
}
"#,
    )
    .unwrap();
    app_dir
}

/// Building a dependency graph twice with nothing changed must reuse every
/// artifact.
///
/// Nothing in this suite asserted anything about incremental freshness
/// before -- `grep -c "up to date"` over it returned 0 -- so a fingerprint
/// that always reported "dirty" and rebuilt the world on every invocation
/// would have been completely invisible, as would a `clean` that failed to
/// remove its cache. It also pins the "build twice" habit itself: a whole
/// class of bug (sources a `prebuild` step generates, which globs expanded
/// at plan time could not see on a clean build until #63) only exists on one
/// of the two builds.
#[test]
fn test_second_build_of_a_dependency_graph_reuses_every_artifact() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    write_answer_lib(&tmp.path().join("answerlib"), 40, 2);
    let app_dir = write_answer_app(&home, &tmp);

    let builds = build_twice(&home, &app_dir);
    assert_eq!(
        run_built_exe(&app_dir, "app").out(),
        "42",
        "sanity: the graph must actually link and run before freshness \
         means anything\n{}",
        builds.clean
    );
    builds.assert_incremental_is_a_no_op();
}

/// Rewrite `answerlib`'s manifest with an `[...surface.abi] toggles` list.
///
/// Everything else is byte-identical to [`write_answer_lib`]'s manifest, so
/// a rebuild after calling this sees exactly one changed input: the ABI
/// declaration.
fn set_answer_lib_abi_toggles(dir: &std::path::Path, toggles: &[&str]) {
    let rendered = toggles
        .iter()
        .map(|t| format!("\"{t}\""))
        .collect::<Vec<_>>()
        .join(", ");
    fs::write(
        dir.join("Harbour.toml"),
        format!(
            r#"[package]
name = "answerlib"
version = "0.1.0"

[targets.answerlib]
kind = "staticlib"
sources = ["src/**/*.c"]

[targets.answerlib.surface.compile.public]
include_dirs = ["include"]

[targets.answerlib.surface.abi]
toggles = [{rendered}]
"#
        ),
    )
    .unwrap();
}

/// Changing a library's declared ABI toggles must re-produce the library.
///
/// `surface.abi.toggles` names the axes an author says their ABI depends on
/// (`pic`, `visibility`, `crt`, `stdlib`). It deliberately emits no flags, so
/// the *only* thing that can honour the declaration is the cache key -- and
/// before this was wired, it reached `ResolvedSurface.abi` and stopped dead:
/// `AbiIdentity::with_surface` had no non-test caller, so the ABI fingerprint
/// varied with nothing but the target triple, the compiler and the target
/// kind. Editing `toggles` produced `All N file(s) up to date` and left every
/// artifact's mtime untouched, which was verified against the unfixed binary
/// before this test was written.
///
/// The second half of the assertion matters as much as the first. A cache key
/// that invalidates too much is its own bug, so the objects must be *reused*:
/// toggles change no compile flag, so nothing may recompile.
#[test]
fn test_changing_abi_toggles_relinks_the_library_but_recompiles_nothing() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let lib_dir = tmp.path().join("answerlib");
    write_answer_lib(&lib_dir, 40, 2);
    set_answer_lib_abi_toggles(&lib_dir, &["pic"]);
    let app_dir = write_answer_app(&home, &tmp);

    let clean = build_ok(&home, &app_dir);
    assert_eq!(
        run_built_exe(&app_dir, "app").out(),
        "42",
        "sanity: the graph must link and run before freshness means \
         anything\n{clean}"
    );

    // The only edit: one more toggle.
    set_answer_lib_abi_toggles(&lib_dir, &["pic", "visibility"]);
    let (log, diff) = rebuild_and_diff(&home, &app_dir);

    // `answerlib.` rather than `answerlib`: the object files live under
    // `.../answerlib/src/` and in an `answerlib-0.1.0` directory, so the bare
    // name matches them too. The archive is the only artifact whose *file
    // name* is `answerlib` followed by an extension -- `libanswerlib.a` on
    // Unix, `answerlib.lib` under MSVC -- so the trailing dot separates the
    // two without hardcoding either spelling or a path separator.
    diff.assert_touched(
        "answerlib.",
        "the library's declared ABI changed, so its archive must be \
         re-produced rather than served from the link cache",
    );

    // `answer.o` is a prefix of `answer.obj`, so this reads the same under
    // MSVC.
    diff.assert_untouched(
        "answer.o",
        "ABI toggles emit no compiler flag, so no source may be recompiled \
         -- a cache key that invalidates more than it must is also a bug",
    );
    diff.assert_untouched(
        "main.o",
        "the app's own sources are untouched by a dependency's ABI \
         declaration",
    );

    assert_eq!(
        run_built_exe(&app_dir, "app").out(),
        "42",
        "the program must still work after the ABI-driven relink\n{log}\n{diff}"
    );
}

/// Changing the compiler between two builds of the same tree must recompile
/// everything -- and the toolchain change must be visible in the fingerprint
/// database.
///
/// Two things are covered here. The first is the invariant itself: a
/// compiler change must invalidate every object, since an object built by
/// another compiler cannot be reused. The second is the mechanism
/// [`Rebuild::assert_reused_everything`] leans on to tell a fingerprinting
/// bug apart from a toolchain change -- if Harbour ever stopped recording
/// the toolchain in its fingerprints, that diagnosis would silently become
/// wrong, so it is asserted rather than assumed.
///
/// Unix-only, and skipped if the second compiler is absent: it needs two
/// compilers that really exist. `CC` is how Harbour's own toolchain
/// detection is overridden (`detect.rs`).
#[test]
#[cfg(unix)]
fn test_changing_the_compiler_between_builds_recompiles_everything() {
    let probe = Command::new("gcc").arg("--version").output();
    if !probe.map(|o| o.status.success()).unwrap_or(false) {
        eprintln!("skipping: no `gcc` on PATH to switch to");
        return;
    }

    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    write_answer_lib(&tmp.path().join("answerlib"), 40, 2);
    let app_dir = write_answer_app(&home, &tmp);

    build_ok(&home, &app_dir);
    let before = recorded_toolchain_hashes(&app_dir);
    assert!(
        !before.is_empty(),
        "the fingerprint database must record a toolchain hash; the \
         conditional freshness assertion is built on it"
    );

    let (rebuild, diff) = rebuild_and_diff_env(&home, &app_dir, &[("CC", "gcc")]);
    let after = recorded_toolchain_hashes(&app_dir);

    assert_ne!(
        before, after,
        "compiling with a different `CC` must change the recorded toolchain \
         fingerprint\n{rebuild}"
    );
    diff.assert_touched(
        "answer",
        "the dependency's object was produced by a different compiler, so it \
         must be recompiled",
    );
    diff.assert_touched(
        "main",
        "the app's object was produced by a different compiler, so it must \
         be recompiled",
    );
    assert_eq!(
        run_built_exe(&app_dir, "app").out(),
        "42",
        "the program must behave the same after being rebuilt with another \
         compiler\n{rebuild}\n{diff}"
    );
}

/// Changing a dependency must recompile exactly what depends on the part
/// that changed, and the consumer must end up running the new code.
///
/// Two failure modes are asserted separately, because they need different
/// fixes and an assertion on the program's output alone cannot tell them
/// apart -- the ambiguity that made the first two attempts at diagnosing
/// the Windows archive bug useless:
///
/// - the dependency's object was never recompiled (a fingerprint that
///   failed to invalidate), versus
/// - it was recompiled and the consumer was never relinked against the new
///   archive.
///
/// The precision cuts the other way too: changing only the dependency's
/// implementation must *not* recompile the consumer's objects, so a
/// fingerprint that invalidates the world on any change fails here.
#[test]
fn test_changing_a_dependency_recompiles_the_right_things() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let lib = tmp.path().join("answerlib");
    write_answer_lib(&lib, 40, 2);
    let app_dir = write_answer_app(&home, &tmp);

    build_ok(&home, &app_dir);
    assert_eq!(run_built_exe(&app_dir, "app").out(), "42", "sanity");

    // Phase 1: the dependency's *implementation* changes. Its object and
    // archive must be redone and the app relinked, but the app's own
    // translation unit does not include anything that changed.
    fs::write(
        lib.join("src/answer.c"),
        "int answer(void) { return 100; }\n",
    )
    .unwrap();

    let (rebuild, diff) = rebuild_and_diff(&home, &app_dir);
    diff.assert_touched(
        "answer",
        "the dependency's source changed, so its object must be recompiled",
    );
    diff.assert_touched(
        "answerlib",
        "the recompiled object must be re-archived; an archive left \
         untouched still holds the old member",
    );
    diff.assert_touched(
        "bin/",
        "the archive changed, so the executable must be relinked -- \
         `recompiled but never relinked` produces a stale program from a \
         successful build",
    );
    diff.assert_untouched(
        "obj/app/",
        "nothing the app's own translation unit includes changed, so \
         recompiling it means the fingerprint is invalidating too much",
    );
    assert_eq!(
        run_built_exe(&app_dir, "app").out(),
        "102",
        "the relinked program must run the new code\n{rebuild}\n{diff}"
    );

    // Phase 2: the dependency's *header* changes. Now the consumer's own
    // object must be recompiled, which only happens if header dependencies
    // are tracked across package boundaries.
    fs::write(
        lib.join("include/answerlib.h"),
        "#define ANSWER_BONUS 900\nint answer(void);\n",
    )
    .unwrap();

    let (rebuild, diff) = rebuild_and_diff(&home, &app_dir);
    diff.assert_touched(
        "obj/app/",
        "the app includes the dependency's public header, so a change to it \
         must recompile the app's object; a build system that only tracks \
         source mtimes silently keeps the old macro value",
    );
    assert_eq!(
        run_built_exe(&app_dir, "app").out(),
        "1000",
        "a header-only change must reach the binary\n{rebuild}\n{diff}"
    );
}

/// A relative `kind = "path"` library resolves against the package that
/// *declared* it, not against the process working directory.
///
/// `add_compile_requirements` takes a `root` and anchors `include_dirs` to
/// it; `add_link_requirements` took no root at all, so a relative archive
/// path reached the linker verbatim and resolved against the root package's
/// directory. `MANIFEST.md` spends a paragraph warning about exactly this
/// hazard for `-I`.
///
/// The test is built so that the unanchored behaviour *succeeds* and links
/// the wrong file: both packages carry a `vendor/` archive of the same name,
/// returning different values, so the binary's own output says which one the
/// linker picked. A test that only asserted "the build fails without the fix"
/// would miss the failure mode that matters -- a root package that happens to
/// have a same-named archive gets a silently wrong link.
#[test]
fn a_relative_path_library_anchors_to_the_package_that_declared_it() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    // Two throwaway staticlib packages, built with Harbour, whose archives
    // are then vendored by hand. Using Harbour to produce them keeps the
    // test off `cc`/`ar` invocation details.
    let archive = |name: &str, answer: i32| -> PathBuf {
        let dir = tmp.path().join(format!("src-{name}"));
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(
            dir.join("Harbour.toml"),
            format!(
                "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n\n\
                 [targets.{name}]\nkind = \"staticlib\"\nsources = [\"src/v.c\"]\n"
            ),
        )
        .unwrap();
        fs::write(
            dir.join("src/v.c"),
            format!("int vendored_answer(void) {{ return {answer}; }}\n"),
        )
        .unwrap();
        harbour(&home)
            .args(["build"])
            .current_dir(&dir)
            .assert()
            .success();
        built_archive_path(&dir, name)
    };
    let real = archive("real", 42);
    let decoy = archive("decoy", 99);

    // The dependency: declares the vendored archive by a path relative to
    // its own root, the same way it declares `include_dirs`.
    let lib_dir = tmp.path().join("lib");
    fs::create_dir_all(lib_dir.join("src")).unwrap();
    fs::create_dir_all(lib_dir.join("include")).unwrap();
    fs::create_dir_all(lib_dir.join("vendor")).unwrap();
    fs::copy(&real, lib_dir.join("vendor/libvend.a")).unwrap();
    fs::write(
        lib_dir.join("Harbour.toml"),
        r#"[package]
name = "mylib"
version = "0.1.0"

[targets.mylib]
kind = "staticlib"
sources = ["src/l.c"]
public_headers = ["include/**/*.h"]

[targets.mylib.surface.compile.public]
include_dirs = ["include"]

[targets.mylib.surface.link.public]
libs = [{ kind = "path", path = "vendor/libvend.a" }]
"#,
    )
    .unwrap();
    fs::write(lib_dir.join("include/l.h"), "int l(void);\n").unwrap();
    fs::write(
        lib_dir.join("src/l.c"),
        "int vendored_answer(void);\nint l(void) { return vendored_answer(); }\n",
    )
    .unwrap();

    // The root package, with a same-named archive of its own at the same
    // relative path. This is what makes the unanchored bug silent.
    let app_dir = tmp.path().join("app");
    fs::create_dir_all(app_dir.join("src")).unwrap();
    fs::create_dir_all(app_dir.join("vendor")).unwrap();
    fs::copy(&decoy, app_dir.join("vendor/libvend.a")).unwrap();
    fs::write(
        app_dir.join("Harbour.toml"),
        r#"[package]
name = "app"
version = "0.1.0"

[dependencies]
mylib = { path = "../lib" }

[targets.app]
kind = "exe"
sources = ["src/m.c"]
"#,
    )
    .unwrap();
    fs::write(
        app_dir.join("src/m.c"),
        "#include <stdio.h>\n#include <l.h>\nint main(void) { printf(\"%d\\n\", l()); return 0; }\n",
    )
    .unwrap();

    harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .success();

    assert_eq!(
        run_built_exe(&app_dir, "app").out(),
        "42",
        "`vendor/libvend.a` is declared in `mylib`'s manifest, so it must \
         resolve inside `mylib`'s tree. `99` means the link resolved it \
         against the root package's directory instead and quietly used the \
         root's same-named archive"
    );

    // `harbour flags` must report the same path the link used, or the
    // inspection command sends you looking in the wrong tree.
    let flags = harbour(&home)
        .args(["flags", "app"])
        .current_dir(&app_dir)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let flags = String::from_utf8_lossy(&flags).to_string();

    // Assert the *property* -- absolute, and ending in the declared relative
    // path -- rather than matching the path string. Two things defeat string
    // comparison on Windows: the anchored path keeps the manifest's own
    // separator inside it (`...\\lib\\vendor/libvend.a`, which Windows accepts),
    // and the CI runner hands the test an 8.3 short temp dir (`RUNNER~1`)
    // while Harbour normalizes the package root to the long form
    // (`runneradmin`). Both spellings name the same file; neither is a bug.
    let line = flags
        .lines()
        .find(|l| l.contains("libvend.a"))
        .unwrap_or_else(|| panic!("`harbour flags` never mentions the library:\n{flags}"));
    let token = line.split("# from:").next().unwrap_or(line).trim();
    let token_path = Path::new(token);
    assert!(
        token_path.is_absolute(),
        "`harbour flags` must name the anchored path, not the relative one: \
         got `{token}`\n{flags}"
    );
    assert!(
        token_path.ends_with(Path::new("vendor").join("libvend.a")),
        "the anchored path must still end in the declared relative path: \
         got `{token}`\n{flags}"
    );
}

/// Compiler flags reach the compiler in the order the manifest wrote them.
///
/// This is the sharpest available proof, because the two halves differ only
/// in the order of two flags and the expected outcomes are opposite:
///
/// - `["-Wall", "-Wno-error", "-Werror"]` must **fail** the build. The
///   author put `-Werror` last, so warnings are errors, and the source has
///   an unused variable.
/// - `["-Wall", "-Werror", "-Wno-error"]` must **succeed** and produce a
///   working binary. `-Wno-error` last downgrades it again.
///
/// Harbour used to sort the effective `cflags` before handing them over,
/// which collates to `-Wall -Werror -Wno-error` either way: both manifests
/// built, and the first one -- the one that says "treat warnings as errors"
/// -- was the one silently inverted. A test that only checked the second
/// half would still pass under the sort, which is why both directions are
/// asserted here.
#[cfg(not(target_env = "msvc"))]
#[test]
fn test_cflag_order_is_the_manifest_order_and_last_wins() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "flagorder"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let dir = tmp.path().join("flagorder");

    // An unused variable: diagnosed by -Wall, fatal under -Werror.
    fs::write(
        dir.join("src/main.c"),
        "#include <stdio.h>\n\
         int main(void) { int unused_here = 1; printf(\"ran\\n\"); return 0; }\n",
    )
    .unwrap();

    let manifest_with = |cflags: &str| {
        format!(
            "[package]\n\
             name = \"flagorder\"\n\
             version = \"0.1.0\"\n\
             \n\
             [targets.flagorder]\n\
             kind = \"exe\"\n\
             sources = [\"src/main.c\"]\n\
             \n\
             [targets.flagorder.private]\n\
             cflags = {cflags}\n"
        )
    };

    // -Werror last: the author asked for a hard failure and must get one.
    fs::write(
        dir.join("Harbour.toml"),
        manifest_with("[\"-Wall\", \"-Wno-error\", \"-Werror\"]"),
    )
    .unwrap();

    let strict = harbour_run(&home, &dir, &["build"]);
    assert!(
        !strict.status.success(),
        "`cflags = [\"-Wall\", \"-Wno-error\", \"-Werror\"]` means warnings are \
         errors. A successful build here means the flags reached the compiler in \
         some other order and `-Wno-error` won.\n{strict}"
    );
    assert!(
        strict.combined().contains("unused"),
        "and it must fail on the unused variable specifically, not on something \
         unrelated\n{strict}"
    );

    // The same two flags the other way round: now it must build and run.
    fs::write(
        dir.join("Harbour.toml"),
        manifest_with("[\"-Wall\", \"-Werror\", \"-Wno-error\"]"),
    )
    .unwrap();

    build_ok(&home, &dir);
    assert_eq!(
        run_built_exe(&dir, "flagorder").out(),
        "ran",
        "with -Wno-error last the warning is not fatal, and the binary must work"
    );
}

/// `harbour flags` reports `cflags` in the same order the compiler gets
/// them.
///
/// Reading the fold is not enough to know this: the command and the build
/// used to be two different folds, and this ordering was one of the four
/// ways they disagreed.
#[cfg(not(target_env = "msvc"))]
#[test]
fn test_flags_command_reports_cflags_in_manifest_order() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "flagecho"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let dir = tmp.path().join("flagecho");

    fs::write(
        dir.join("Harbour.toml"),
        "[package]\n\
         name = \"flagecho\"\n\
         version = \"0.1.0\"\n\
         \n\
         [targets.flagecho]\n\
         kind = \"exe\"\n\
         sources = [\"src/main.c\"]\n\
         \n\
         [targets.flagecho.private]\n\
         cflags = [\"-Wall\", \"-Wno-error\", \"-Werror\"]\n",
    )
    .unwrap();

    let reported = harbour_run(&home, &dir, &["flags", "flagecho"]).success();
    let order: Vec<&str> = reported
        .combined()
        .lines()
        .filter_map(|l| {
            let flag = l.split_whitespace().next()?;
            ["-Wall", "-Wno-error", "-Werror"]
                .into_iter()
                .find(|f| *f == flag)
        })
        .collect();

    assert_eq!(
        order,
        vec!["-Wall", "-Wno-error", "-Werror"],
        "`harbour flags` must print the manifest's order, which is also the \
         compiler's order\n{reported}"
    );
}

// ============================================================================
// `harbour flags` parity with the real compile command
//
// `harbour flags` used to read a second, hand-maintained copy of the surface
// fold, and disagreed with the build in four ways at once -- a
// `compile = "private"` dependency it reported anyway, a `target = "..."` it
// ignored (so its answer flapped between runs), a `-L` whose absence is a
// deliberate safety property, and a different flag order.
//
// The fix is structural (one fold), but the reason this class of bug recurs
// is that nothing could observe the disagreement. So the deliverable is this
// test: it captures the **actual argv the compiler is handed** and asserts
// that it is exactly what `harbour flags` printed, plus the operands. Reading
// the code cannot establish that; only running it can.
// ============================================================================

/// Install a `cc` wrapper that records every argv it is given, and return
/// (path to the wrapper, directory the records land in).
///
/// One file per invocation: Harbour compiles in parallel, and concurrent
/// appends to a single log interleave mid-line -- which, the first time this
/// was tried by hand, produced a "difference" that was purely the log
/// corrupting itself.
#[cfg(not(windows))]
fn install_cc_recorder(tmp: &std::path::Path) -> (PathBuf, PathBuf) {
    let records = tmp.join("argv-records");
    fs::create_dir_all(&records).unwrap();
    let shim = tmp.join("cc-recorder");
    fs::write(
        &shim,
        format!(
            "#!/bin/sh\n\
             out=\"{}/$$.$(od -An -N2 -tu2 /dev/urandom | tr -d ' ')\"\n\
             printf '%s\\n' \"$@\" > \"$out\"\n\
             exec cc \"$@\"\n",
            records.display()
        ),
    )
    .unwrap();
    let mut perms = fs::metadata(&shim).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    fs::set_permissions(&shim, perms).unwrap();
    (shim, records)
}

/// Every recorded argv, one `Vec<String>` per compiler invocation.
#[cfg(not(windows))]
fn recorded_argvs(records: &std::path::Path) -> Vec<Vec<String>> {
    let mut all = Vec::new();
    for entry in fs::read_dir(records).unwrap() {
        let text = fs::read_to_string(entry.unwrap().path()).unwrap();
        all.push(text.lines().map(|l| l.to_string()).collect());
    }
    all
}

/// The flags `harbour flags` printed, in order, with the `# from:`
/// attribution stripped.
fn reported_flags(output: &str) -> Vec<String> {
    output
        .lines()
        .filter(|l| l.starts_with("  ") && !l.trim().is_empty())
        .filter_map(|l| {
            let flag = l.split("# from:").next()?.trim();
            (!flag.is_empty()).then(|| flag.to_string())
        })
        .flat_map(|flag| {
            // A two-token option is printed on one line with its operand
            // (`-framework Security`); the argv has them separately.
            flag.split_whitespace()
                .map(|t| t.to_string())
                .collect::<Vec<_>>()
        })
        .collect()
}

/// `harbour flags --compile` is exactly the compile command, minus the
/// source and output operands.
///
/// The fixture is deliberately not minimal. It has a dependency whose
/// public surface must propagate, a second dependency marked
/// `compile = "private"` whose surface must *not*, a `target = "..."`
/// naming one of two library targets in the same package, a flag both
/// packages ask for (so deduplication is exercised), and a conditional
/// block for the host OS. Every one of those was a way the two folds used
/// to disagree.
#[cfg(all(not(windows), not(target_env = "msvc")))]
#[test]
fn test_flags_matches_the_real_compile_command() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let (shim, records) = install_cc_recorder(tmp.path());

    let host_os = if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };

    // A dependency with two library targets, so `target = "..."` is load
    // bearing: "the first library target" is a coin flip across runs.
    let lib = tmp.path().join("paritylib");
    fs::create_dir_all(lib.join("src")).unwrap();
    fs::create_dir_all(lib.join("include")).unwrap();
    fs::write(lib.join("src/wanted.c"), "int wanted(void) { return 7; }\n").unwrap();
    fs::write(lib.join("src/other.c"), "int other(void) { return 9; }\n").unwrap();
    fs::write(lib.join("include/wanted.h"), "int wanted(void);\n").unwrap();
    fs::write(
        lib.join("Harbour.toml"),
        "[package]\n\
         name = \"paritylib\"\n\
         version = \"1.0.0\"\n\
         \n\
         [targets.wanted]\n\
         kind = \"staticlib\"\n\
         sources = [\"src/wanted.c\"]\n\
         \n\
         [targets.wanted.public]\n\
         include_dirs = [\"include\"]\n\
         defines = [\"FROM_WANTED=1\"]\n\
         cflags = [\"-fno-common\"]\n\
         \n\
         [targets.other]\n\
         kind = \"staticlib\"\n\
         sources = [\"src/other.c\"]\n\
         \n\
         [targets.other.public]\n\
         defines = [\"FROM_OTHER=1\"]\n",
    )
    .unwrap();

    // A dependency whose compile surface must not reach the consumer.
    let hidden = tmp.path().join("parityhidden");
    fs::create_dir_all(hidden.join("src")).unwrap();
    fs::write(
        hidden.join("src/h.c"),
        "int hidden_thing(void) { return 1; }\n",
    )
    .unwrap();
    fs::write(
        hidden.join("Harbour.toml"),
        "[package]\n\
         name = \"parityhidden\"\n\
         version = \"1.0.0\"\n\
         \n\
         [targets.parityhidden]\n\
         kind = \"staticlib\"\n\
         sources = [\"src/h.c\"]\n\
         \n\
         [targets.parityhidden.public]\n\
         defines = [\"MUST_NOT_REACH_CONSUMER=1\"]\n",
    )
    .unwrap();

    let app = tmp.path().join("parityapp");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(
        app.join("src/main.c"),
        "#include <stdio.h>\n\
         #include \"wanted.h\"\n\
         #ifdef MUST_NOT_REACH_CONSUMER\n\
         #error \"a compile = private dependency reached the consumer\"\n\
         #endif\n\
         int main(void) { printf(\"%d\\n\", wanted()); return 0; }\n",
    )
    .unwrap();
    fs::write(
        app.join("Harbour.toml"),
        format!(
            "[package]\n\
             name = \"parityapp\"\n\
             version = \"0.1.0\"\n\
             \n\
             [dependencies]\n\
             paritylib = {{ path = \"../paritylib\" }}\n\
             parityhidden = {{ path = \"../parityhidden\" }}\n\
             \n\
             [targets.parityapp]\n\
             kind = \"exe\"\n\
             sources = [\"src/main.c\"]\n\
             \n\
             [targets.parityapp.deps]\n\
             paritylib = {{ target = \"wanted\" }}\n\
             parityhidden = {{ compile = \"private\", link = \"public\" }}\n\
             \n\
             [targets.parityapp.private]\n\
             defines = [\"APP_PRIVATE=1\"]\n\
             cflags = [\"-Wall\", \"-fno-common\"]\n\
             \n\
             [[targets.parityapp.when]]\n\
             os = \"{host_os}\"\n\
             defines = [\"HOST_MATCHED=1\"]\n"
        ),
    )
    .unwrap();

    let build =
        harbour_run_env(&home, &app, &["build"], &[("CC", shim.to_str().unwrap())]).success();

    // The consumer's own translation unit -- the one whose flags the two
    // folds disagreed about. Canonicalized: Harbour resolves the manifest
    // path, and on macOS the temp dir lives under `/var`, a symlink to
    // `/private/var`, so the uncanonicalized path matches nothing.
    let main_c = app
        .join("src/main.c")
        .canonicalize()
        .unwrap()
        .display()
        .to_string();
    let argvs = recorded_argvs(&records);
    let compile: Vec<String> = argvs
        .into_iter()
        .find(|a| a.contains(&main_c))
        .unwrap_or_else(|| {
            panic!("no recorded compile of {main_c}; the CC wrapper never ran\n{build}")
        });

    // Strip the parts that are not flags: `-c` leads, and the source and
    // output operands trail (see `GccToolchain::compile_command`).
    assert_eq!(
        compile.first().map(String::as_str),
        Some("-c"),
        "{compile:?}"
    );
    let tail = compile.len() - 3;
    assert_eq!(compile[tail], main_c, "{compile:?}");
    assert_eq!(compile[tail + 1], "-o", "{compile:?}");
    let argv_flags: Vec<String> = compile[1..tail].to_vec();

    let reported = harbour_run(&home, &app, &["flags", "parityapp", "--compile"]).success();
    let printed = reported_flags(&reported.stdout);

    assert_eq!(
        printed, argv_flags,
        "`harbour flags` must print exactly the flags the compiler was \
         handed, in the same order.\n\
         reported: {printed:#?}\n\
         actual argv: {argv_flags:#?}\n{reported}"
    );

    // And the things that made the old copy wrong, asserted directly so a
    // regression names itself rather than showing up as a diff.
    assert!(
        printed.iter().any(|f| f == "-DFROM_WANTED=1"),
        "the named dependency target's public surface must be there: {printed:#?}"
    );
    assert!(
        !printed.iter().any(|f| f.contains("FROM_OTHER")),
        "the dependency's *other* library target must not contribute: {printed:#?}"
    );
    assert!(
        !printed
            .iter()
            .any(|f| f.contains("MUST_NOT_REACH_CONSUMER")),
        "a compile = \"private\" dependency must not appear: {printed:#?}"
    );
    assert_eq!(
        printed.iter().filter(|f| *f == "-fno-common").count(),
        1,
        "a flag both packages asked for reaches the compiler once, so it must \
         be printed once: {printed:#?}"
    );

    assert_eq!(
        run_built_exe(&app, "parityapp").out(),
        "7",
        "and the whole thing still has to build and run"
    );
}

/// `harbour flags --link` is exactly the link command's flags.
///
/// Separate from the compile assertion because the failure modes are
/// different: this is the one where a `-L` for a dependency's artifact
/// directory used to be reported, and its *absence* is deliberate -- every
/// `-L` also applies to the driver's implicit libraries, so a dependency
/// named `c` on the search path lets its `libc.a` shadow the system one.
#[cfg(all(not(windows), not(target_env = "msvc")))]
#[test]
fn test_flags_matches_the_real_link_command() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let (shim, records) = install_cc_recorder(tmp.path());

    let lib = tmp.path().join("linklib");
    fs::create_dir_all(lib.join("src")).unwrap();
    fs::write(lib.join("src/l.c"), "int lib_answer(void) { return 5; }\n").unwrap();
    fs::write(
        lib.join("Harbour.toml"),
        "[package]\n\
         name = \"linklib\"\n\
         version = \"1.0.0\"\n\
         \n\
         [targets.linklib]\n\
         kind = \"staticlib\"\n\
         sources = [\"src/l.c\"]\n\
         \n\
         [targets.linklib.public]\n\
         libs = [\"m\"]\n",
    )
    .unwrap();

    let app = tmp.path().join("linkapp");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(
        app.join("src/main.c"),
        "#include <stdio.h>\n\
         int lib_answer(void);\n\
         int main(void) { printf(\"%d\\n\", lib_answer()); return 0; }\n",
    )
    .unwrap();
    fs::write(
        app.join("Harbour.toml"),
        "[package]\n\
         name = \"linkapp\"\n\
         version = \"0.1.0\"\n\
         \n\
         [dependencies]\n\
         linklib = { path = \"../linklib\" }\n\
         \n\
         [targets.linkapp]\n\
         kind = \"exe\"\n\
         sources = [\"src/main.c\"]\n\
         \n\
         [targets.linkapp.deps]\n\
         linklib = \"linklib\"\n",
    )
    .unwrap();

    let build =
        harbour_run_env(&home, &app, &["build"], &[("CC", shim.to_str().unwrap())]).success();

    // Canonicalized for the same reason as the compile test.
    let output = built_exe_path(&app, "linkapp")
        .canonicalize()
        .unwrap()
        .display()
        .to_string();
    let argvs = recorded_argvs(&records);
    let link: Vec<String> = argvs
        .into_iter()
        .find(|a| a.contains(&output) && !a.iter().any(|t| t == "-c"))
        .unwrap_or_else(|| panic!("no recorded link of {output}\n{build}"));

    // Link argv is `-o <output> <objects...> <flags...>` (see
    // `GccToolchain::link_exe_command`). Everything after the last object
    // file is a flag.
    let after_output = link
        .iter()
        .position(|t| *t == output)
        .expect("output operand")
        + 1;
    let argv_flags: Vec<String> = link[after_output..]
        .iter()
        .filter(|t| !t.ends_with(".o"))
        .cloned()
        .collect();

    let reported = harbour_run(&home, &app, &["flags", "linkapp", "--link"]).success();
    let printed = reported_flags(&reported.stdout);

    assert_eq!(
        printed, argv_flags,
        "`harbour flags --link` must print exactly the flags the linker was \
         handed, in the same order.\n\
         reported: {printed:#?}\n\
         actual argv: {argv_flags:#?}\n{reported}"
    );
    assert!(
        printed.iter().any(|f| f.ends_with("liblinklib.a")),
        "the dependency archive is passed by absolute path: {printed:#?}"
    );
    assert!(
        !printed
            .iter()
            .any(|f| f.starts_with("-L") && f.contains("deps")),
        "and deliberately without a matching -L, so a dependency named `c` \
         cannot shadow the system libc: {printed:#?}"
    );

    assert_eq!(run_built_exe(&app, "linkapp").out(), "5");
}

/// A compiler warning on a *successful* compile must reach the user.
///
/// `NativeBuilder::compile` read `output.stderr` only inside
/// `if !output.status.success()`, so every diagnostic from a compile that
/// exited 0 was discarded. `harbour new` scaffolds `-Wall -Wextra` into every
/// generated project and those flags do reach the compiler -- verified
/// separately against the real `cc` argv -- so the effect was that the
/// scaffold's warning flags were decorative: enabled, fired, and thrown away.
///
/// The source below is written so the two flags catch one diagnostic each:
/// `-Wall` gives `-Wunused-variable`, `-Wextra` gives `-Wunused-parameter`.
/// Neither is reported without those flags, which is what makes this a test
/// of the plumbing rather than of the compiler's defaults.
#[test]
fn compiler_warnings_on_a_successful_build_reach_the_user() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");

    fs::write(
        app_dir.join("src/main.c"),
        r#"#include <stdio.h>
static int helper(int used, int ignored) { return used; }
int main(void) {
    int never_read = 1;
    printf("%d\n", helper(2, 3));
    return 0;
}
"#,
    )
    .unwrap();

    let out = harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .success()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();

    // The content assertion is deliberately not made under MSVC.
    //
    // The mechanism is confirmed there -- CI showed `cl`'s own
    // `D9002` command-line warnings arriving on stderr, which they did not
    // before this change. What is *not* established is which stream `cl`
    // puts file-level diagnostics (`C4189`, `C4100`) on; they did not appear
    // on stderr in that run, and I have no MSVC host to determine whether
    // they go to stdout instead. Asserting a guess here would either fail
    // spuriously or pass vacuously, and a vacuous assertion is what this
    // very test exists to prevent. Tracked separately; the regression this
    // test guards was found and is reproducible on the Unix toolchains.
    if !cfg!(target_env = "msvc") {
        for needle in ["warning", "unused variable", "unused parameter"] {
            assert!(
                stderr.contains(needle),
                "the build succeeded and the compiler emitted diagnostics, so \
                 `{needle}` must appear on stderr -- otherwise the scaffold's \
                 warning flags are decorative.\nstderr:\n{stderr}"
            );
        }
    }

    // And the build still succeeded: warnings are surfaced, not promoted.
    assert!(
        built_exe_path(&app_dir, "app").exists(),
        "surfacing warnings must not fail the build"
    );
}

/// `compile_commands.json` must be generated from the same options as the
/// build, C++ language flags included.
///
/// `BuildPlan::emit_compile_commands` used to build its own `CompileInput`
/// and pass `cxx_opts: None`, while `NativeBuilder::compile` passed the real
/// options. Every C++ flag -- `-std=`, `-fno-exceptions`, `-fno-rtti`,
/// `-stdlib=` -- is emitted inside a `if let Some(opts) = cxx_opts` in the
/// toolchain backends, so none of them appeared in the database. clangd read
/// the file as exceptions-enabled C++ at the default standard while the
/// compiler was given `-std=c++17 -fno-exceptions -fno-rtti`.
///
/// The witness for "the compiler really got these" is the produced *binary*,
/// not the flag list: it prints what the preprocessor saw, so the claim is
/// established independently of what the database says.
#[test]
fn compile_commands_carries_the_same_cxx_flags_as_the_build() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let app_dir = tmp.path().join("cpptest");
    fs::create_dir_all(app_dir.join("src")).unwrap();

    fs::write(
        app_dir.join("Harbour.toml"),
        r#"[package]
name = "cpptest"
version = "0.1.0"

[build]
cpp_std = "17"
exceptions = false
rtti = false

[targets.cpptest]
kind = "exe"
lang = "c++"
sources = ["src/main.cpp"]
"#,
    )
    .unwrap();
    // The probe has to mean the same thing under both toolchains, and the
    // obvious spellings do not:
    //
    // - `__cplusplus` reports `199711` under MSVC no matter which `/std:` is
    //   in force, unless `/Zc:__cplusplus` is passed (which Harbour does not
    //   pass). `_MSVC_LANG` carries the real value and is the documented way
    //   to read it.
    // - `__EXCEPTIONS` is a GCC/clang macro. MSVC never defines it, so the
    //   original probe reported `exceptions=off` under MSVC whether or not
    //   `/EHsc` was passed -- it would have "passed" for the wrong reason.
    //   MSVC's spelling is `_CPPUNWIND`.
    // - RTTI is `__GXX_RTTI` on GCC/clang and `_CPPRTTI` on MSVC.
    fs::write(
        app_dir.join("src/main.cpp"),
        r#"#include <cstdio>

#ifdef _MSVC_LANG
#  define HB_STD _MSVC_LANG
#else
#  define HB_STD __cplusplus
#endif

#if defined(__EXCEPTIONS) || defined(_CPPUNWIND)
#  define HB_EXCEPTIONS "on"
#else
#  define HB_EXCEPTIONS "off"
#endif

#if defined(__GXX_RTTI) || defined(_CPPRTTI)
#  define HB_RTTI "on"
#else
#  define HB_RTTI "off"
#endif

int main() {
    std::printf("std=%ld\n", (long)HB_STD);
    std::printf("exceptions=%s\n", HB_EXCEPTIONS);
    std::printf("rtti=%s\n", HB_RTTI);
    return 0;
}
"#,
    )
    .unwrap();

    harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .success();

    // What the compiler actually did, read off the artifact it produced.
    let exe = built_exe_path(&app_dir, "cpptest");
    let out = Command::new(&exe).output().unwrap();
    assert!(out.status.success(), "the built binary must run");
    let reported = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        reported.contains("std=201703"),
        "the compile must have selected C++17: {reported}"
    );
    assert!(
        reported.contains("exceptions=off"),
        "the manifest sets `exceptions = false`, so the compile must have \
         disabled them: {reported}"
    );
    assert!(
        reported.contains("rtti=off"),
        "the manifest sets `rtti = false`, so the compile must have disabled \
         it: {reported}"
    );

    // What the database claims it did. MSVC spells all three differently;
    // these spellings were read off `MsvcToolchain::compile_command`'s actual
    // output rather than guessed (`/EHs-c-`, not `/EHsc-`).
    let cc = fs::read_to_string(app_dir.join(".harbour/compile_commands.json")).unwrap();
    let expected: [&str; 3] = if cfg!(target_env = "msvc") {
        ["/std:c++17", "/EHs-c-", "/GR-"]
    } else {
        ["-std=c++17", "-fno-exceptions", "-fno-rtti"]
    };
    for flag in expected {
        assert!(
            cc.contains(flag),
            "the compiler received `{flag}` (the binary above proves it), so \
             compile_commands.json must list it too, or clangd parses this \
             file as a different dialect than the build compiles it as. \
             compile_commands.json:\n{cc}"
        );
    }
}

// ============================================================================
// `[profile]` flags in the active toolchain's own syntax
//
// `ProfileContext::profile_cflags` emitted `-O{level}`, `-g`, `-g3` and
// `-fsanitize=` unconditionally, in GCC syntax, with no toolchain branch.
// `cl.exe` answered every one of them with `D9002: ignoring unknown option`
// and compiled anyway -- so on Windows the release profile was unoptimised,
// no build had ever carried debug information, and `sanitizers` did nothing.
// The build was green throughout.
//
// The unit tests in `src/builder/toolchain/msvc.rs` pin the spellings. These
// two exist because a spelling that is right in a `Vec<String>` and never
// reaches `cl` is worth nothing: they read what the real build recorded, and
// on the `windows-latest` job the compiler that received it is a real
// `cl.exe`.
// ============================================================================

/// A project whose source can tell whether it was optimised, and by how much
/// debug information it was compiled with.
fn profile_fixture(tmp: &std::path::Path) -> PathBuf {
    let dir = tmp.join("profileflags");
    fs::create_dir_all(dir.join("src")).unwrap();
    // `__OPTIMIZE__` is defined by GCC and clang only when the optimiser is
    // actually enabled -- it is evidence about codegen, not about the command
    // line. MSVC defines no equivalent, so there the binary just runs.
    fs::write(
        dir.join("src/main.c"),
        "#include <stdio.h>\n\
         int main(void) {\n\
         #if defined(__OPTIMIZE__)\n\
             puts(\"optimised\");\n\
         #else\n\
             puts(\"unoptimised\");\n\
         #endif\n\
             return 0;\n\
         }\n",
    )
    .unwrap();
    fs::write(
        dir.join("Harbour.toml"),
        "[package]\n\
         name = \"profileflags\"\n\
         version = \"0.1.0\"\n\
         \n\
         [targets.profileflags]\n\
         kind = \"exe\"\n\
         sources = [\"src/main.c\"]\n",
    )
    .unwrap();
    dir
}

/// Every file under `root` whose extension is `ext`, searched recursively.
///
/// Object and program-database layout differs per toolchain, so the tests
/// that need "the objects this build produced" find them rather than
/// hard-coding a path -- which would also hard-code a separator.
fn files_with_extension(root: &std::path::Path, ext: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(next) = stack.pop() {
        let Ok(entries) = fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some(ext) {
                found.push(path);
            }
        }
    }
    found
}

/// Whether `haystack` contains `needle`, for looking at binaries.
fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Every argv `compile_commands.json` recorded, flattened.
///
/// The database is generated from `BuildContext::compile_spec` -- the same
/// call `NativeBuilder::compile` makes -- so this is the argv the compiler
/// was handed, not a reconstruction of it.
fn recorded_compile_args(app_dir: &std::path::Path) -> String {
    fs::read_to_string(app_dir.join(".harbour").join("compile_commands.json")).unwrap()
}

/// The default profiles must reach the real compiler in the syntax that
/// compiler understands, and must produce a binary that behaves accordingly.
#[test]
fn profile_flags_reach_the_real_compiler_in_the_toolchains_own_syntax() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let dir = profile_fixture(tmp.path());

    // `[profile.debug]`: opt_level = "0", debug = "2".
    let debug = harbour_run(&home, &dir, &["build"]).success();
    let debug_args = recorded_compile_args(&dir);

    // `[profile.release]`: opt_level = "3", debug = "0".
    let release = harbour_run(&home, &dir, &["build", "--release"]).success();
    let release_args = recorded_compile_args(&dir);

    let msvc = cfg!(target_env = "msvc");

    let (no_opt, max_opt, debug_info) = if msvc {
        ("/Od", "/O2", "/Z7")
    } else {
        ("-O0", "-O3", "-g3")
    };

    assert!(
        debug_args.contains(no_opt),
        "the debug profile asks for opt_level = \"0\"; the compiler must be \
         told so in its own syntax.\n{debug_args}"
    );
    assert!(
        debug_args.contains(debug_info),
        "the debug profile asks for debug = \"2\"; without this flag the \
         build carries no debug information at all -- which is what every \
         Windows build did.\n{debug_args}"
    );
    assert!(
        release_args.contains(max_opt),
        "the release profile asks for opt_level = \"3\"; without this the \
         release build is unoptimised.\n{release_args}"
    );
    assert!(
        !release_args.contains(debug_info),
        "the release profile asks for debug = \"0\"; nothing should request \
         debug information.\n{release_args}"
    );

    // The other half of the same claim: nothing in the *other* toolchain's
    // syntax may appear. On `main` a `cl` command line carried `-O3`.
    let wrong: &[&str] = if msvc {
        &["-O0", "-O3", "-g3", "-g "]
    } else {
        &["/Od", "/O2", "/Z7"]
    };
    for flag in wrong {
        for (which, args) in [("debug", &debug_args), ("release", &release_args)] {
            assert!(
                !args.contains(flag),
                "`{flag}` is not this compiler's syntax and would be ignored \
                 with a warning at best ({which} build):\n{args}"
            );
        }
    }

    // The diagnostic that was being thrown away. Warnings from successful
    // compiles are surfaced now, so if `cl` is ignoring an option we would
    // see it here.
    for build in [&debug, &release] {
        let out = build.combined();
        assert!(
            !out.contains("D9002") && !out.contains("ignoring unknown option"),
            "the compiler reported that it ignored an option we passed:\n{out}"
        );
    }

    // And the effect, where the compiler will tell us: `__OPTIMIZE__` is
    // defined from codegen settings, not from the command line text.
    if !msvc {
        // Debug information is in the object or it is not there at all. The
        // section is named `.debug_info` in ELF and `__debug_info` in
        // Mach-O, so the common substring covers Linux and macOS both.
        let debug_objects = files_with_extension(&target_dir(&dir).join("debug"), "o");
        assert!(
            !debug_objects.is_empty(),
            "no object files under the debug tree to inspect"
        );
        for obj in &debug_objects {
            let bytes = fs::read(obj).unwrap();
            assert!(
                contains_bytes(&bytes, b"debug_info"),
                "`debug = \"2\"` must put real debug information in {}, not \
                 just a flag on the command line",
                obj.display()
            );
        }
        for obj in files_with_extension(&target_dir(&dir).join("release"), "o") {
            let bytes = fs::read(&obj).unwrap();
            assert!(
                !contains_bytes(&bytes, b"debug_info"),
                "`debug = \"0\"` asked for none, yet {} carries debug \
                 information",
                obj.display()
            );
        }

        assert_eq!(
            run_built_exe(&dir, "profileflags").out(),
            "unoptimised",
            "the debug profile sets opt_level = \"0\", so the optimiser must \
             be off in the binary, not merely off on paper"
        );
        assert_eq!(
            run_built_exe_in(&dir, "release", "profileflags").out(),
            "optimised",
            "the release profile sets opt_level = \"3\", so the optimiser \
             must actually have run"
        );
    } else {
        // MSVC has no `__OPTIMIZE__`; all we can assert here is that both
        // binaries work. The optimisation itself is pinned by the argv
        // assertions above plus `cl` accepting the flag without a D9002.
        run_built_exe(&dir, "profileflags").success();
        run_built_exe_in(&dir, "release", "profileflags").success();
    }
}

/// The Windows artifact the old code could never produce: a PDB.
///
/// `/Z7` puts CodeView records in each `.obj` and `/DEBUG` makes the linker
/// turn them into a program database beside the image. With neither flag
/// emitted -- `-g` being ignored -- a Windows debug build produced a binary
/// with no symbols and no PDB, which is to say one you cannot set a
/// breakpoint in. The release profile asks for `debug = "0"`, so it must
/// *not* produce one; that direction is what makes this test non-vacuous.
#[cfg(target_env = "msvc")]
#[test]
fn a_windows_debug_build_produces_a_pdb_and_a_release_build_does_not() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let dir = profile_fixture(tmp.path());

    harbour_run(&home, &dir, &["build"]).success();
    let debug_pdbs = files_with_extension(&target_dir(&dir).join("debug"), "pdb");
    assert!(
        !debug_pdbs.is_empty(),
        "`debug = \"2\"` must produce debug information that survives into \
         the image; no .pdb exists under {}",
        target_dir(&dir).join("debug").display()
    );

    harbour_run(&home, &dir, &["build", "--release"]).success();
    let release_pdbs = files_with_extension(&target_dir(&dir).join("release"), "pdb");
    assert!(
        release_pdbs.is_empty(),
        "`debug = \"0\"` asked for no debug information, but a .pdb was \
         written anyway: {release_pdbs:?}"
    );
}

/// The control for the `D9002` assertion above.
///
/// "No `D9002` in the output" is only worth something if a `D9002` would
/// have shown up. This hands `cl` the exact flag `profile_cflags` used to
/// emit for `opt_level = "3"` -- `-O3`, via `cflags`, where a verbatim flag
/// is the user's business -- and asserts that the compiler says it ignored
/// it *and* that the build still succeeds. That pairing is the whole defect
/// in one test: an option that does nothing, a warning that says so, and a
/// green build.
#[cfg(target_env = "msvc")]
#[test]
fn cl_reports_a_gcc_style_flag_as_ignored_and_builds_anyway() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let dir = profile_fixture(tmp.path());

    let manifest = fs::read_to_string(dir.join("Harbour.toml")).unwrap();
    fs::write(
        dir.join("Harbour.toml"),
        format!("{manifest}\n[profile.debug]\ncflags = [\"-O3\"]\n"),
    )
    .unwrap();

    let build = harbour_run(&home, &dir, &["build"]).success();
    let out = build.combined();
    assert!(
        out.contains("D9002"),
        "`cl` ignores `-O3` and says so; if this does not appear, either the \
         compiler's warnings are being discarded again or the assertion in \
         `profile_flags_reach_the_real_compiler_in_the_toolchains_own_syntax` \
         is vacuous.\n{out}"
    );
    assert!(
        out.contains("-O3"),
        "the warning must name the option that was ignored\n{out}"
    );
}

/// A profile setting a toolchain cannot express must stop the build, not
/// vanish from the command line.
///
/// `sanitizers = ["thread"]` used to become `-fsanitize=thread`, which `cl`
/// ignored with a `D9002` nobody was reading -- a build that reported
/// success and was not sanitized. On GCC and clang the same manifest is
/// legitimate, so this is deliberately two different expectations.
#[test]
fn a_sanitizer_the_toolchain_lacks_fails_the_build_rather_than_disappearing() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let dir = profile_fixture(tmp.path());

    let manifest = fs::read_to_string(dir.join("Harbour.toml")).unwrap();
    fs::write(
        dir.join("Harbour.toml"),
        format!("{manifest}\n[profile.debug]\nsanitizers = [\"thread\"]\n"),
    )
    .unwrap();

    let build = harbour_run(&home, &dir, &["build"]);

    if cfg!(target_env = "msvc") {
        assert!(
            !build.status.success(),
            "MSVC implements no thread sanitizer; the build must say so \
             instead of quietly producing an unsanitized binary\n{build}"
        );
        assert!(
            build.combined().contains("MSVC does not implement"),
            "the error must name the reason\n{build}"
        );
    } else {
        // ThreadSanitizer exists here. Not every platform can *run* the
        // result (and macOS x86-only support makes running it unportable),
        // so this asserts only that the flag was accepted and recorded.
        build.success();
        let args = recorded_compile_args(&dir);
        assert!(
            args.contains("-fsanitize=thread"),
            "a sanitizer this toolchain supports must reach the compiler\n{args}"
        );
    }
}

/// A misspelled profile value must be rejected, not pasted into a flag.
///
/// `opt_level = "fastest"` used to be formatted straight into `-Ofastest`
/// and handed to the compiler; on MSVC it became a `D9002` and was ignored
/// outright.
#[test]
fn an_unknown_opt_level_is_an_error_with_the_valid_values_listed() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let dir = profile_fixture(tmp.path());

    let manifest = fs::read_to_string(dir.join("Harbour.toml")).unwrap();
    fs::write(
        dir.join("Harbour.toml"),
        format!("{manifest}\n[profile.debug]\nopt_level = \"fastest\"\n"),
    )
    .unwrap();

    let build = harbour_run(&home, &dir, &["build"]);
    assert!(
        !build.status.success(),
        "`opt_level = \"fastest\"` is not a thing; the build must not \
         proceed as if it were\n{build}"
    );
    assert!(
        build.combined().contains("fastest") && build.combined().contains("0, 1, 2, 3, s, z"),
        "the error must name the bad value and the good ones\n{build}"
    );
}

/// The manifest source for the probe fixture used by the tests below.
///
/// The header list is chosen so the answers are *known independently*: one
/// header exists on every hosted platform, and one exists nowhere. A probe
/// subsystem whose fixture answers are all `yes` would be indistinguishable
/// from a constant, which is why the negative case is in the fixture rather
/// than in a separate test.
#[cfg(not(windows))]
const PROBE_FIXTURE_MANIFEST: &str = r#"[package]
name = "probed"
version = "0.1.0"

[targets.probed]
kind = "exe"
sources = ["src/main.c"]

[targets.probed.probes]
check_headers = ["stdio.h", "definitely/not/a/real/header.h"]
check_sizeof = ["int", "long", "short", "size_t", "void *"]
"#;

/// A program that is *only* compilable if every probed size agrees with what
/// the compiler itself says, and which prints what the preprocessor saw.
///
/// The compile-time assertions are the point. "Harbour built something and
/// exited 0" has repeatedly meant "wrong output, exit 0" in this repo, so the
/// fixture is written so that a wrong probe answer is a **build failure**
/// rather than a wrong number on stdout.
#[cfg(not(windows))]
const PROBE_FIXTURE_SOURCE: &str = r#"#include <stdio.h>
#include <stddef.h>
#define SAME(a, b) { char c[((a) == (b)) ? 1 : -1]; (void) c; }
int main(void) {
    SAME(sizeof(int), SIZEOF_INT)
    SAME(sizeof(long), SIZEOF_LONG)
    SAME(sizeof(short), SIZEOF_SHORT)
    SAME(sizeof(size_t), SIZEOF_SIZE_T)
    SAME(sizeof(void *), SIZEOF_VOID_P)
#ifdef HAVE_STDIO_H
    printf("stdio=yes\n");
#else
    printf("stdio=no\n");
#endif
#ifdef HAVE_DEFINITELY_NOT_A_REAL_HEADER_H
    printf("bogus=yes\n");
#else
    printf("bogus=no\n");
#endif
    printf("long=%d\n", SIZEOF_LONG);
    return 0;
}
"#;

#[cfg(not(windows))]
fn write_probe_fixture(dir: &std::path::Path) {
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(dir.join("Harbour.toml"), PROBE_FIXTURE_MANIFEST).unwrap();
    fs::write(dir.join("src/main.c"), PROBE_FIXTURE_SOURCE).unwrap();
}

/// Every recorded argv containing a probe snippet.
#[cfg(not(windows))]
fn probe_compile_count(records: &std::path::Path) -> usize {
    recorded_argvs(records)
        .iter()
        .filter(|a| a.iter().any(|x| x.contains("probe.c")))
        .count()
}

#[cfg(not(windows))]
fn clear_records(records: &std::path::Path) {
    for entry in fs::read_dir(records).unwrap() {
        fs::remove_file(entry.unwrap().path()).unwrap();
    }
}

/// The probe defines on the recorded compile of the package's own source, in
/// the order the compiler received them.
#[cfg(not(windows))]
fn recorded_probe_defines(records: &std::path::Path) -> Vec<String> {
    let argvs = recorded_argvs(records);
    let real = argvs
        .iter()
        .find(|a| a.iter().any(|x| x.ends_with("main.c")))
        .unwrap_or_else(|| panic!("no compile of main.c was recorded; argvs: {argvs:#?}"));
    real.iter()
        .filter(|a| a.starts_with("-DHAVE_") || a.starts_with("-DSIZEOF_"))
        .cloned()
        .collect()
}

/// Probe answers must reach the *real* compile command, not just a data
/// structure.
///
/// This is the test that distinguishes a working probe subsystem from the
/// failure mode this repo keeps producing: nine-plus features have existed as
/// well-typed, fully-parsed code that never reached the compiler, and the
/// crate's root `pub mod`s suppress `dead_code` so nothing warns. The witness
/// here is the argv the compiler was actually handed, captured by a recording
/// `CC` shim, plus the behaviour of the produced binary.
#[test]
#[cfg(not(windows))]
fn probe_answers_reach_the_real_compile_command() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let (shim, records) = install_cc_recorder(tmp.path());
    let app = tmp.path().join("probed");
    write_probe_fixture(&app);

    harbour_run_env(&home, &app, &["build"], &[("CC", shim.to_str().unwrap())]).success();

    let probe_defines = recorded_probe_defines(&records);

    assert!(
        probe_defines.contains(&"-DHAVE_STDIO_H=1".to_string()),
        "a header that exists must reach the compiler as a define. \
         probe defines on the real compile line: {probe_defines:?}"
    );

    // The inverse, and the more important half: a false answer must emit
    // *nothing*. `-DHAVE_X=0` would satisfy the `#ifdef HAVE_X` that every
    // real config header is tested with, inverting the answer.
    assert!(
        !probe_defines
            .iter()
            .any(|a| a.contains("DEFINITELY_NOT_A_REAL_HEADER")),
        "a header that does not exist must emit no define at all, not \
         `=0`: {probe_defines:?}"
    );

    // Order is declaration order, not sorted. `ac859c1` removed a sort of
    // the flag list because C flags are last-wins; re-introducing one here
    // for probe defines would be the same defect in a new place.
    let names: Vec<String> = probe_defines
        .iter()
        .map(|d| {
            d.trim_start_matches("-D")
                .split('=')
                .next()
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(
        names,
        vec![
            "HAVE_STDIO_H",
            "SIZEOF_INT",
            "SIZEOF_LONG",
            "SIZEOF_SHORT",
            "SIZEOF_SIZE_T",
            "SIZEOF_VOID_P",
        ],
        "probe defines must reach the compiler in declaration order -- \
         `check_headers` before `check_sizeof`, each in list order"
    );

    // The binary is the independent witness: it reports what the
    // preprocessor saw, so the claim does not rest on the argv capture.
    let out = run_built_exe(&app, "probed");
    assert!(
        out.out().contains("stdio=yes"),
        "the built program must see HAVE_STDIO_H: {}",
        out.out()
    );
    assert!(
        out.out().contains("bogus=no"),
        "the built program must not see a define for a header that does not \
         exist: {}",
        out.out()
    );
}

/// A `sizeof` probe must produce the size the compiler itself reports, for
/// the target being built for, without running anything.
///
/// The fixture's `SAME(...)` macros are compile-time assertions, so a wrong
/// answer fails the build. That is deliberate: the alternative -- printing
/// the number and comparing strings -- cannot distinguish "the probe is
/// wrong" from "the program printed something else".
#[test]
#[cfg(not(windows))]
fn sizeof_probes_agree_with_the_compiler_without_running_a_program() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let app = tmp.path().join("probed");
    write_probe_fixture(&app);

    // If any probed size disagreed with `sizeof` as the compiler computes
    // it, this build would fail on a negative array bound.
    build_ok(&home, &app);

    let out = run_built_exe(&app, "probed");
    let long_size: usize = out
        .out()
        .lines()
        .find_map(|l| l.strip_prefix("long="))
        .expect("the fixture prints the probed sizeof(long)")
        .parse()
        .expect("a number");
    // Compared against Rust's own view of C `long` rather than a literal
    // `8`, so this stays correct on a 32-bit target -- where the answer must
    // be 4, and where the vendored-config placeholder this replaces would
    // have asserted 8 because it was keyed on the OS alone.
    assert_eq!(
        long_size,
        std::mem::size_of::<std::os::raw::c_long>(),
        "the probed sizeof(long) must match what this target's C `long` \
         actually is"
    );
}

/// Probe answers must be cached against the toolchain, and the cache must be
/// discarded wholesale when the toolchain changes.
///
/// A stale probe answer is worse than a stale object file: it is a *wrong
/// `#define`*, so the package compiles as though it were running on a
/// different machine. This repo has already shipped one cache-keying defect
/// (`e860627`), and the probe cache reuses `ToolchainFingerprint::hash()`
/// precisely so that there is only one definition of "the toolchain
/// changed".
#[test]
#[cfg(not(windows))]
fn the_probe_cache_survives_a_rebuild_and_dies_with_the_toolchain() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let app = tmp.path().join("probed");
    write_probe_fixture(&app);

    // Two shims that behave identically but live at different paths, so the
    // only thing that changes between builds is the toolchain identity.
    let (shim_a, records) = install_cc_recorder(tmp.path());
    let shim_b = tmp.path().join("cc-recorder-b");
    fs::copy(&shim_a, &shim_b).unwrap();
    let mut perms = fs::metadata(&shim_b).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    fs::set_permissions(&shim_b, perms).unwrap();

    harbour_run_env(&home, &app, &["build"], &[("CC", shim_a.to_str().unwrap())]).success();
    let cold = probe_compile_count(&records);
    assert!(
        cold > 0,
        "a cold build must actually run probe compiles; recorded none"
    );

    clear_records(&records);
    harbour_run_env(&home, &app, &["build"], &[("CC", shim_a.to_str().unwrap())]).success();
    assert_eq!(
        probe_compile_count(&records),
        0,
        "a warm rebuild with an unchanged toolchain must answer every probe \
         from the cache and spawn no compiler at all"
    );

    clear_records(&records);
    harbour_run_env(&home, &app, &["build"], &[("CC", shim_b.to_str().unwrap())]).success();
    assert_eq!(
        probe_compile_count(&records),
        cold,
        "changing the compiler must discard the probe cache wholesale and \
         re-measure every probe -- a probe answer that survives a toolchain \
         change is a wrong `#define`, not a stale object"
    );
}

/// A probe answer that changes must recompile the objects that depend on it.
///
/// The chain under test is long and every link has been a bug in some build
/// system: the compile surface changes, so the probe cache is discarded, so
/// the probe is re-measured, so the answer flips, so the define set changes,
/// so the compile fingerprint changes, so the object is rebuilt, so the
/// program behaves differently. Asserting on the program's *behaviour* is
/// what makes this a real test rather than a check that a hash changed.
#[test]
#[cfg(not(windows))]
fn a_changed_probe_answer_rebuilds_and_changes_what_the_program_does() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let app = tmp.path().join("probed");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::create_dir_all(app.join("vendored/made/up")).unwrap();

    let manifest = |extra_include: &str| {
        format!(
            "[package]\n\
             name = \"probed\"\n\
             version = \"0.1.0\"\n\
             \n\
             [targets.probed]\n\
             kind = \"exe\"\n\
             sources = [\"src/main.c\"]\n\
             {extra_include}\n\
             [targets.probed.probes]\n\
             check_headers = [\"made/up/header.h\"]\n"
        )
    };

    fs::write(
        app.join("src/main.c"),
        "#include <stdio.h>\n\
         int main(void) {\n\
         #ifdef HAVE_MADE_UP_HEADER_H\n\
         \x20   printf(\"found\\n\");\n\
         #else\n\
         \x20   printf(\"missing\\n\");\n\
         #endif\n\
         \x20   return 0;\n\
         }\n",
    )
    .unwrap();
    fs::write(
        app.join("vendored/made/up/header.h"),
        "/* a header that exists only when the include path reaches it */\n",
    )
    .unwrap();

    // Build 1: the header is on disk but not on the include path, so the
    // probe must answer no.
    fs::write(app.join("Harbour.toml"), manifest("")).unwrap();
    build_ok(&home, &app);
    assert_eq!(
        run_built_exe(&app, "probed").out(),
        "missing",
        "a header outside the include path must probe as absent -- if this \
         says `found`, the probe is reading the source tree rather than \
         asking the compiler"
    );

    // Build 2: the same header, now reachable. Nothing about the probe
    // declaration changed; only the surface it is asked against.
    fs::write(
        app.join("Harbour.toml"),
        manifest("\n[targets.probed.private]\ninclude_dirs = [\"vendored\"]\n"),
    )
    .unwrap();
    build_ok(&home, &app);
    assert_eq!(
        run_built_exe(&app, "probed").out(),
        "found",
        "the include path changed, so the probe must be re-measured, the \
         define must change, and the object must be recompiled. `missing` \
         here means a stale probe answer survived into a rebuilt binary"
    );
}

/// The same manifest and toolchain must produce the same probe defines, in
/// the same order, every time.
///
/// Not a hypothetical concern: a measured 18 distinct link orders across 40
/// clean runs of one manifest was fixed days ago, caused by `HashMap`
/// iteration reaching build output. Probe answers reach build output as
/// defines, so this is the same exposure in a new subsystem.
#[test]
#[cfg(not(windows))]
fn probe_defines_are_byte_identical_across_clean_builds() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let (shim, records) = install_cc_recorder(tmp.path());
    let app = tmp.path().join("probed");
    write_probe_fixture(&app);

    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..6 {
        fs::remove_dir_all(app.join(".harbour")).ok();
        clear_records(&records);
        harbour_run_env(&home, &app, &["build"], &[("CC", shim.to_str().unwrap())]).success();
        seen.insert(recorded_probe_defines(&records).join(" "));
    }
    assert_eq!(
        seen.len(),
        1,
        "6 clean builds of one manifest produced {} different probe define \
         lists:\n{:#?}",
        seen.len(),
        seen
    );
}

/// `harbour flags` must list the probe defines the build actually uses, in
/// the same order, and must attribute them to a *probe* rather than to a
/// manifest table nobody wrote.
///
/// This command is documented as authoritative about what the compiler
/// receives, and §2.4 of the 2026-09-07 audit is four separate instances of
/// it not being -- each one a second implementation of "what flags does this
/// file get" that had drifted from the first. A probe define reaching the
/// compiler but not this listing would be the fifth, and it was: the first
/// version of the probe subsystem measured probes inside `BuildPlan`, so
/// `harbour flags` printed three profile flags and nothing else.
#[test]
#[cfg(not(windows))]
fn harbour_flags_lists_the_probe_defines_the_build_uses() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let (shim, records) = install_cc_recorder(tmp.path());
    let app = tmp.path().join("probed");
    write_probe_fixture(&app);

    harbour_run_env(&home, &app, &["build"], &[("CC", shim.to_str().unwrap())]).success();
    let from_compiler = recorded_probe_defines(&records);
    assert!(
        !from_compiler.is_empty(),
        "the compiler must have received probe defines for this test to mean \
         anything"
    );

    let reported = harbour_run(&home, &app, &["flags", "probed", "--compile"]).success();
    let from_flags: Vec<String> = reported_flags(reported.out())
        .into_iter()
        .filter(|f| f.starts_with("-DHAVE_") || f.starts_with("-DSIZEOF_"))
        .collect();

    assert_eq!(
        from_flags, from_compiler,
        "`harbour flags` must print exactly the probe defines the compiler \
         received, in the same order.\nflags said: {from_flags:?}\ncc got:    \
         {from_compiler:?}"
    );

    // And the attribution must say these were measured, not declared. A
    // reader who goes looking for `-DSIZEOF_LONG=8` in a `surface` table
    // will not find it, and needs to know that changing the toolchain can
    // change the value.
    assert!(
        reported.out().contains("(probe)"),
        "a probe-derived define must be attributed to `probe`, not to a \
         manifest table that does not contain it:\n{}",
        reported.out()
    );
}

/// Probe answers are private to the target that declared them, and do **not**
/// reach a dependent. This pins the behaviour Harbour actually has.
///
/// A characterization test, not an aspiration. `visibility = "public"` was
/// implemented in the first draft of this subsystem: it parsed, it was
/// branched on in `BuildPlan`, and its defines were folded into the
/// `AbiSurfaceKey` so a consumer would relink. It did not work. A dependent's
/// compile surface is folded from each dependency's *declared*
/// `surface.compile.public` (`surface_resolver.rs`, the `dep_resolved
/// .compile_public` arm), and a probe answer exists in no manifest -- so the
/// consumer here failed to compile on an undefined `SIZEOF_LONG` while the
/// field looked, from the library's side, like it worked.
///
/// That is the 2026-09-07 audit's §2.7 shape exactly, so the field was
/// removed rather than shipped. This test exists so that the day someone
/// implements propagation, it fails and says so, instead of the manifest
/// reference quietly becoming wrong.
#[test]
#[cfg(not(windows))]
fn probe_answers_do_not_reach_a_dependent() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let lib = tmp.path().join("problib");
    fs::create_dir_all(lib.join("src")).unwrap();
    fs::create_dir_all(lib.join("include")).unwrap();
    fs::write(
        lib.join("Harbour.toml"),
        "[package]\n\
         name = \"problib\"\n\
         version = \"0.1.0\"\n\
         \n\
         [targets.problib]\n\
         kind = \"staticlib\"\n\
         sources = [\"src/lib.c\"]\n\
         public_headers = [\"include/problib.h\"]\n\
         \n\
         [targets.problib.public]\n\
         include_dirs = [\"include\"]\n\
         \n\
         [targets.problib.probes]\n\
         check_headers = [\"stdio.h\"]\n\
         check_sizeof = [\"long\"]\n",
    )
    .unwrap();
    fs::write(
        lib.join("include/problib.h"),
        "int problib_size_of_long(void);\n",
    )
    .unwrap();
    // The library itself does see its own answers -- that is the half that
    // works, and this file would not compile without it.
    fs::write(
        lib.join("src/lib.c"),
        "#ifndef HAVE_STDIO_H\n\
         #error \"the declaring target must see its own probe answers\"\n\
         #endif\n\
         int problib_size_of_long(void) { return SIZEOF_LONG; }\n",
    )
    .unwrap();

    let app = tmp.path().join("probapp");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(
        app.join("Harbour.toml"),
        "[package]\n\
         name = \"probapp\"\n\
         version = \"0.1.0\"\n\
         \n\
         [dependencies]\n\
         problib = { path = \"../problib\" }\n\
         \n\
         [targets.probapp]\n\
         kind = \"exe\"\n\
         sources = [\"src/main.c\"]\n\
         \n\
         [targets.probapp.deps]\n\
         problib = \"problib\"\n",
    )
    .unwrap();
    fs::write(
        app.join("src/main.c"),
        "#include <stdio.h>\n\
         #include \"problib.h\"\n\
         int main(void) {\n\
         #ifdef HAVE_STDIO_H\n\
         \x20   printf(\"consumer_sees_probe=1\\n\");\n\
         #else\n\
         \x20   printf(\"consumer_sees_probe=0\\n\");\n\
         #endif\n\
         \x20   printf(\"lib_long=%d\\n\", problib_size_of_long());\n\
         \x20   return 0;\n\
         }\n",
    )
    .unwrap();

    build_ok(&home, &app);
    let out = run_built_exe(&app, "probapp");

    assert!(
        out.out().contains("consumer_sees_probe=0"),
        "probe answers are private to the declaring target. If the consumer \
         now sees them, propagation has been implemented -- update \
         MANIFEST.md's probe section and this test together:\n{}",
        out.out()
    );
    // The library's own answer is real and travels in the archive, which is
    // what makes "private" the right word rather than "broken".
    let long_size: usize = out
        .out()
        .lines()
        .find_map(|l| l.strip_prefix("lib_long="))
        .expect("the library reports its probed sizeof(long)")
        .parse()
        .expect("a number");
    assert_eq!(
        long_size,
        std::mem::size_of::<std::os::raw::c_long>(),
        "the library was compiled with its own probe answer, and the value \
         must be the real one:\n{}",
        out.out()
    );
}

/// `c_std` reaches the compiler, the compile database, and the cache key.
///
/// This is the test the field never had. `c_std` parsed, validated, was
/// documented in `MANIFEST.md` as working, and was read by nothing: the only
/// `-std=` Harbour emitted came from `CxxOptions`, so a C package pinning
/// `c_std = "99"` for portability got the compiler's default in silence.
///
/// Every assertion here is on observed behaviour rather than on a flag
/// string alone:
///
/// * the built program prints `__STDC_VERSION__`, so the *compiler* has to
///   have agreed, not merely been handed an argument;
/// * it also prints whether `__STRICT_ANSI__` is defined, which is the only
///   thing that distinguishes `gnu99` from `c99` -- and the difference real
///   packages depend on (`typeof`, statement expressions, `asm`);
/// * "it entered the fingerprint" is proved by the compiler *not being
///   invoked* on an unchanged rebuild and being invoked again when only
///   `c_std` changed. A cache key that reads right but does not invalidate
///   would pass a hash-shaped assertion and fail this one.
#[cfg(all(not(windows), not(target_env = "msvc")))]
#[test]
fn test_c_std_reaches_the_compiler_the_compile_database_and_the_cache_key() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let (shim, records) = install_cc_recorder(tmp.path());

    let app = tmp.path().join("cstdapp");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(
        app.join("src/main.c"),
        "#include <stdio.h>\n\
         int main(void) {\n\
         #if defined(__STRICT_ANSI__)\n\
             const char *dialect = \"strict\";\n\
         #else\n\
             const char *dialect = \"gnu\";\n\
         #endif\n\
             printf(\"%ld %s\\n\", (long)__STDC_VERSION__, dialect);\n\
             return 0;\n\
         }\n",
    )
    .unwrap();

    let manifest = |c_std: &str| {
        format!(
            "[package]\n\
             name = \"cstdapp\"\n\
             version = \"0.1.0\"\n\
             \n\
             [targets.cstdapp]\n\
             kind = \"exe\"\n\
             sources = [\"src/main.c\"]\n\
             {c_std}"
        )
    };

    let main_c = app
        .join("src/main.c")
        .canonicalize()
        .unwrap()
        .display()
        .to_string();

    // Every recorded compile of `main.c` since the records were last
    // cleared. Empty means the object was reused -- which is the only
    // evidence that distinguishes "the fingerprint changed" from "the
    // fingerprint is ignored".
    let compiles_of_main = |records: &std::path::Path| -> Vec<Vec<String>> {
        recorded_argvs(records)
            .into_iter()
            .filter(|argv| argv.contains(&main_c))
            .collect()
    };
    let clear = |records: &std::path::Path| {
        fs::remove_dir_all(records).unwrap();
        fs::create_dir_all(records).unwrap();
    };

    let build = |records: &std::path::Path| -> RunLog {
        clear(records);
        harbour_run_env(&home, &app, &["build"], &[("CC", shim.to_str().unwrap())]).success()
    };

    // ---- `c_std = "99"`: strict C99, all the way to the running binary.
    fs::write(app.join("Harbour.toml"), manifest("c_std = \"99\"\n")).unwrap();
    let log = build(&records);
    let argvs = compiles_of_main(&records);
    assert_eq!(argvs.len(), 1, "expected one compile of main.c\n{log}");
    assert!(
        argvs[0].contains(&"-std=c99".to_string()),
        "`c_std = \"99\"` must put `-std=c99` on the real compiler argv, \
         not merely parse: {:?}\n{log}",
        argvs[0]
    );
    assert_eq!(
        run_built_exe(&app, "cstdapp").out(),
        "199901 strict",
        "the compiler has to have *honoured* the standard: the program \
         prints its own __STDC_VERSION__ and whether __STRICT_ANSI__ is \
         defined"
    );

    // clangd must be told the same dialect the build used, or it reports
    // diagnostics for a language the build never compiled.
    let cc_db = fs::read_to_string(app.join(".harbour/compile_commands.json")).unwrap();
    assert!(
        cc_db.contains("-std=c99"),
        "compile_commands.json must carry the same `-std=` the build used:\n{cc_db}"
    );

    // `harbour flags` exists to be authoritative, so it has to show it too.
    let reported = harbour_run(&home, &app, &["flags", "cstdapp", "--compile"]).success();
    let printed = reported_flags(&reported.stdout);
    let tail = argvs[0].len() - 3;
    assert_eq!(
        printed,
        argvs[0][1..tail].to_vec(),
        "`harbour flags` must print exactly the flags the compiler was \
         handed, in order.\n{reported}"
    );

    // ---- An unchanged rebuild must not recompile. Without this, the next
    // assertion would pass even if the fingerprint ignored `c_std` and
    // simply recompiled everything every time.
    let log = build(&records);
    assert!(
        compiles_of_main(&records).is_empty(),
        "nothing changed, so main.c must not be recompiled; otherwise the \
         `c_std` invalidation below proves nothing\n{log}"
    );

    // ---- Only `c_std` changes: c99 -> gnu99. Same ISO version, different
    // dialect, and the compiler must be run again to pick it up.
    fs::write(app.join("Harbour.toml"), manifest("c_std = \"gnu99\"\n")).unwrap();
    let log = build(&records);
    let argvs = compiles_of_main(&records);
    assert_eq!(
        argvs.len(),
        1,
        "changing `c_std` must invalidate the compile fingerprint, or the \
         stale object silently survives\n{log}"
    );
    assert!(
        argvs[0].contains(&"-std=gnu99".to_string()),
        "the GNU dialect must be requested as such: {:?}\n{log}",
        argvs[0]
    );
    assert_eq!(
        run_built_exe(&app, "cstdapp").out(),
        "199901 gnu",
        "gnu99 is C99 *without* __STRICT_ANSI__ -- that difference is the \
         whole reason the GNU forms are spellable"
    );

    // ---- Dropping the field again is also a change, and must also rebuild.
    fs::write(app.join("Harbour.toml"), manifest("")).unwrap();
    let log = build(&records);
    let argvs = compiles_of_main(&records);
    assert_eq!(
        argvs.len(),
        1,
        "removing `c_std` must invalidate the fingerprint too\n{log}"
    );
    assert!(
        !argvs[0].iter().any(|a| a.starts_with("-std=")),
        "with no `c_std` and no C++ in the graph, Harbour must not invent a \
         standard: {:?}\n{log}",
        argvs[0]
    );
}

/// Assembly in the same target as C must not be given `-std=`.
///
/// zstd, libuv and every crypto library lay their sources out this way: one
/// target, `.c` and `.S` side by side. `-std=` describes a C dialect; the
/// `.S` files go through the same driver but the flag is meaningless there,
/// and `GccToolchain::compile_command` skips it for `Language::Asm`. This is
/// the end-to-end check of that -- the unit test pins the argv, this pins
/// that a mixed target still builds and runs.
#[cfg(all(not(windows), not(target_env = "msvc")))]
#[test]
fn test_c_std_is_not_applied_to_assembly_in_a_mixed_target() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let (shim, records) = install_cc_recorder(tmp.path());

    let app = tmp.path().join("mixed");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(
        app.join("src/main.c"),
        "#include <stdio.h>\n\
         long asm_answer(void);\n\
         int main(void) { printf(\"%ld\\n\", asm_answer()); return 0; }\n",
    )
    .unwrap();

    // A leaf function returning 41 + 1, in the host's assembly dialect.
    let asm = if cfg!(target_arch = "aarch64") {
        ".text\n\
         .globl _asm_answer\n\
         .globl asm_answer\n\
         _asm_answer:\n\
         asm_answer:\n\
         \tmov x0, #42\n\
         \tret\n"
    } else {
        ".text\n\
         .globl _asm_answer\n\
         .globl asm_answer\n\
         _asm_answer:\n\
         asm_answer:\n\
         \tmovq $42, %rax\n\
         \tret\n"
    };
    fs::write(app.join("src/answer.S"), asm).unwrap();

    fs::write(
        app.join("Harbour.toml"),
        "[package]\n\
         name = \"mixed\"\n\
         version = \"0.1.0\"\n\
         \n\
         [targets.mixed]\n\
         kind = \"exe\"\n\
         sources = [\"src/main.c\", \"src/answer.S\"]\n\
         c_std = \"gnu11\"\n",
    )
    .unwrap();

    let log = harbour_run_env(&home, &app, &["build"], &[("CC", shim.to_str().unwrap())]).success();

    let asm_path = app
        .join("src/answer.S")
        .canonicalize()
        .unwrap()
        .display()
        .to_string();
    let main_path = app
        .join("src/main.c")
        .canonicalize()
        .unwrap()
        .display()
        .to_string();
    let argvs = recorded_argvs(&records);

    let asm_argv = argvs
        .iter()
        .find(|a| a.contains(&asm_path))
        .unwrap_or_else(|| panic!("no recorded compile of {asm_path}\n{log}"));
    assert!(
        !asm_argv.iter().any(|a| a.starts_with("-std=")),
        "assembly must not be handed a C standard: {asm_argv:?}"
    );

    let c_argv = argvs
        .iter()
        .find(|a| a.contains(&main_path))
        .unwrap_or_else(|| panic!("no recorded compile of {main_path}\n{log}"));
    assert!(
        c_argv.contains(&"-std=gnu11".to_string()),
        "the C source in the same target still gets it: {c_argv:?}"
    );

    assert_eq!(
        run_built_exe(&app, "mixed").out(),
        "42",
        "and the mixed target has to link and run"
    );
}

/// On MSVC, `c_std` uses the `/std:` switch `cl` has -- and says so when it
/// hasn't got one.
///
/// `cl` has `/std:c11` and `/std:c17` and nothing for C89, C99 or C23. The
/// unit tests in `msvc.rs` pin the argv from any host; this one only runs
/// where `cl` actually does, because the thing worth proving is that `cl`
/// *accepts* the switch and that a standard it cannot express does not turn
/// into a silently ignored setting. Two wrong MSVC expectations have been
/// committed in this repo from reasoning by analogy with gcc, so this is
/// deliberately verified on the `windows-latest` job rather than asserted.
#[cfg(target_env = "msvc")]
#[test]
fn test_c_std_on_msvc_uses_the_switch_cl_has_and_warns_about_the_rest() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let app = tmp.path().join("msvccstd");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(
        app.join("src/main.c"),
        "#include <stdio.h>\n\
         int main(void) { printf(\"ok\\n\"); return 0; }\n",
    )
    .unwrap();

    let manifest = |c_std: &str| {
        format!(
            "[package]\n\
             name = \"msvccstd\"\n\
             version = \"0.1.0\"\n\
             \n\
             [targets.msvccstd]\n\
             kind = \"exe\"\n\
             sources = [\"src/main.c\"]\n\
             c_std = \"{c_std}\"\n"
        )
    };

    // A standard `cl` has: the switch must be on the command line, `cl`
    // must accept it, and the program must run.
    fs::write(app.join("Harbour.toml"), manifest("11")).unwrap();
    let log = harbour_run(&home, &app, &["build"]).success();
    let cc_db = fs::read_to_string(app.join(".harbour/compile_commands.json")).unwrap();
    assert!(
        cc_db.contains("/std:c11"),
        "`c_std = \"11\"` must become `/std:c11` on MSVC:\n{cc_db}\n{log}"
    );
    assert_eq!(run_built_exe(&app, "msvccstd").out(), "ok");

    // A standard `cl` has not: no flag, no GCC spelling smuggled in, and a
    // warning that names the gap.
    fs::remove_dir_all(app.join(".harbour")).unwrap();
    fs::write(app.join("Harbour.toml"), manifest("99")).unwrap();
    let log = harbour_run(&home, &app, &["build"]).success();
    let cc_db = fs::read_to_string(app.join(".harbour/compile_commands.json")).unwrap();
    assert!(
        !cc_db.contains("/std:c") && !cc_db.contains("-std="),
        "`cl` has no C99 switch, so none may be emitted -- and certainly \
         not the gcc spelling:\n{cc_db}"
    );
    let narration = log.combined();
    assert!(
        narration.contains("c_std") && narration.contains("c99"),
        "a standard that cannot be honoured must be reported, not dropped \
         in silence\n{log}"
    );
    assert_eq!(run_built_exe(&app, "msvccstd").out(), "ok");
}

/// Probes are answered in the dialect the package is compiled in.
///
/// `answer_for_target` builds its `CompileInput` from the same
/// `Toolchain::compile_command` the real build uses, so when `c_std` grew a
/// field there, that site had to say something. `c_std: None` would have
/// compiled, and would have meant probes measured in the compiler's default
/// dialect while the package is compiled in its own — a `config.h`
/// describing a translation unit the package is not going to have.
///
/// That is not hypothetical. Measured, on two libcs:
///
/// * glibc (gcc 13): `sizeof(u_int)` answers under the default dialect and
///   under `-std=gnu99`, and the type **does not exist** under `-std=c99`
///   or `-std=c11`. `__STRICT_ANSI__` stops `features.h` defining
///   `_DEFAULT_SOURCE`, and `__USE_MISC` goes with it. `gnu99` versus `c99`
///   — the exact pair — is a different set of declarations.
/// * Apple clang: BSD types survive `-std=c99` there, but `max_align_t` is
///   visible only from `-std=c11` up, so the dialect still decides the
///   answer, by a different mechanism.
///
/// This test uses a header of its own rather than either of those, so it
/// asserts the wiring on every toolchain including MSVC, and so it cannot
/// start failing because a libc reorganised its feature macros. `89` and
/// `11` are the two values every backend can express: `cl` has `/std:c11`
/// and defines `__STDC_VERSION__` under it, and emits nothing for `89`.
///
/// The second half is the cache: both builds run in the same tree with no
/// `.harbour` removed in between, so an answer that survived the `c_std`
/// change would be a stale cache hit. `surface_key` covers the dialect for
/// exactly this reason.
#[test]
fn test_probes_are_measured_in_the_packages_own_c_dialect() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let app = tmp.path().join("probedialect");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::create_dir_all(app.join("include")).unwrap();
    fs::write(app.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();

    // A header that exists on disk but only *compiles* from C99 onward.
    // The probe question "does `#include <needs_c99.h>` compile" therefore
    // has a different answer per dialect, on every toolchain, without
    // depending on any libc's feature macros.
    fs::write(
        app.join("include/needs_c99.h"),
        "#if !defined(__STDC_VERSION__) || __STDC_VERSION__ < 199901L\n\
         #error \"this header requires C99\"\n\
         #endif\n\
         int needs_c99(void);\n",
    )
    .unwrap();

    let manifest = |c_std: &str| {
        format!(
            "[package]\n\
             name = \"probedialect\"\n\
             version = \"0.1.0\"\n\
             \n\
             [targets.probedialect]\n\
             kind = \"exe\"\n\
             sources = [\"src/main.c\"]\n\
             c_std = \"{c_std}\"\n\
             \n\
             [targets.probedialect.private]\n\
             include_dirs = [\"include\"]\n\
             \n\
             [targets.probedialect.probes]\n\
             check_headers = [\"needs_c99.h\"]\n"
        )
    };

    let define_present = |app: &std::path::Path| -> bool {
        fs::read_to_string(app.join(".harbour/compile_commands.json"))
            .unwrap()
            .contains("HAVE_NEEDS_C99_H")
    };

    // C89: the probe must be compiled as C89 and answer "no", so no define.
    fs::write(app.join("Harbour.toml"), manifest("89")).unwrap();
    let run = harbour_run(&home, &app, &["build"]).success();
    assert!(
        !define_present(&app),
        "the probe must be answered under the target's own `c_std = \"89\"`; \
         a `no` here is the header refusing to compile as C89\n{run}"
    );

    // C11, same tree, cache warm from the run above: the answer must be
    // re-measured, not served.
    fs::write(app.join("Harbour.toml"), manifest("11")).unwrap();
    let run = harbour_run(&home, &app, &["build"]).success();
    assert!(
        define_present(&app),
        "under `c_std = \"11\"` the same probe must answer `yes` -- and the \
         previous dialect's answer must not have been served from the probe \
         cache\n{run}"
    );

    // And back again, to pin that the invalidation is not one-directional
    // (a cache keyed on \"anything changed\" would pass the step above).
    fs::write(app.join("Harbour.toml"), manifest("89")).unwrap();
    let run = harbour_run(&home, &app, &["build"]).success();
    assert!(
        !define_present(&app),
        "going back to C89 must re-measure too\n{run}"
    );

    assert_eq!(
        run_built_exe(&app, "probedialect").status.code(),
        Some(0),
        "and the package still builds and runs"
    );
}

/// Declaring something Harbour cannot honour fails the build, by name, with
/// somewhere to go.
///
/// This started as three fields from the #102 sweep, each of which parsed
/// cleanly and then reached nothing. Two have since been implemented --
/// `[profile.NAME]` (#106) and `optional = true` (#108), both covered by
/// their own tests at the end of this file -- and the third,
/// `[targets.NAME.backend]`, has been **removed from the schema** rather
/// than implemented: it was a second, weaker spelling of
/// `[targets.NAME.recipe]`, which already dispatches per target and checks
/// its per-backend option keys (#107).
///
/// Checked end to end rather than only in `Manifest::parse`, because what
/// matters is that the *user running `harbour build`* is told: before this,
/// all three produced an ordinary green build.
#[test]
fn test_declared_but_unimplemented_manifest_settings_fail_the_build() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let lib = tmp.path().join("unimplib");
    fs::create_dir_all(lib.join("src")).unwrap();
    fs::write(lib.join("src/l.c"), "int l(void) { return 1; }\n").unwrap();
    fs::write(
        lib.join("Harbour.toml"),
        "[package]\n\
         name = \"unimplib\"\n\
         version = \"1.0.0\"\n\
         \n\
         [targets.unimplib]\n\
         kind = \"staticlib\"\n\
         sources = [\"src/l.c\"]\n",
    )
    .unwrap();

    let app = tmp.path().join("unimpapp");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(app.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();

    let base = "[package]\n\
                name = \"unimpapp\"\n\
                version = \"0.1.0\"\n\
                \n\
                [targets.unimpapp]\n\
                kind = \"exe\"\n\
                sources = [\"src/main.c\"]\n";

    // The table is gone from the schema, so the diagnostic has to name the
    // table, point at the spelling that does dispatch, and say where the
    // decision is recorded.
    fs::write(
        app.join("Harbour.toml"),
        format!("{base}\n[targets.unimpapp.backend]\nbackend = \"cmake\"\n"),
    )
    .unwrap();
    let run = harbour_run(&home, &app, &["build"]);
    assert!(
        !run.status.success(),
        "`[targets.X.backend]` is not in the schema, so the build must fail \
         rather than build natively and report `[native]`\n{run}"
    );
    let message = run.combined();
    assert!(
        message.contains("`[targets.unimpapp.backend]`"),
        "the diagnostic must name the table\n{run}"
    );
    assert!(
        message.contains("recipe"),
        "and point at the spelling that dispatches\n{run}"
    );
    assert!(
        message.contains("issues/107"),
        "and say where the decision is tracked\n{run}"
    );
    assert!(
        !built_exe_path_in(&app, "debug", "unimpapp").exists(),
        "nothing may have been built"
    );

    // And the same manifest without the offending fragment still builds and
    // runs, so the rejection is the only thing being tested here.
    fs::write(app.join("Harbour.toml"), base).unwrap();
    harbour_run(&home, &app, &["build"]).success();
    assert!(built_exe_path_in(&app, "debug", "unimpapp").exists());
}

/// `harbour ffi generate` for a language with no generator exits non-zero
/// and writes nothing -- and the one language that does work still does.
///
/// It used to print "not yet implemented. Contributions welcome!" and exit
/// **0**, having created no files and not even the output directory. In CI
/// the exit code is the only thing read, so that is a green binding-
/// generation step that generated no bindings. Confirmed by running before
/// the change: exit 0, `<output>` absent.
#[test]
fn test_ffi_generate_for_an_unimplemented_language_exits_non_zero() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let app = tmp.path().join("ffiapp");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::create_dir_all(app.join("include")).unwrap();
    fs::write(
        app.join("Harbour.toml"),
        r#"[package]
name = "ffiapp"
version = "0.1.0"

[targets.ffiapp]
kind = "sharedlib"
sources = ["src/lib.c"]
public_headers = ["include/**/*.h"]

[targets.ffiapp.ffi]
header_files = ["include/**/*.h"]
"#,
    )
    .unwrap();
    fs::write(
        app.join("include/ffiapp.h"),
        "#ifndef FFIAPP_H\n#define FFIAPP_H\nint ffiapp_answer(void);\n#endif\n",
    )
    .unwrap();
    fs::write(
        app.join("src/lib.c"),
        "#include \"ffiapp.h\"\nint ffiapp_answer(void) { return 42; }\n",
    )
    .unwrap();

    for lang in ["python", "csharp", "rust"] {
        let out = app.join(format!("bindings-{lang}"));
        let run = harbour_run(
            &home,
            &app,
            &[
                "ffi",
                "generate",
                "--lang",
                lang,
                "--output",
                out.to_str().unwrap(),
            ],
        );
        assert!(
            !run.status.success(),
            "`--lang {lang}` writes no files, so it must not exit 0\n{run}"
        );
        assert!(
            run.combined().contains("issues/109"),
            "and say where the work is tracked\n{run}"
        );
        assert!(
            !out.exists(),
            "nothing was generated, so `{}` must not exist either",
            out.display()
        );
    }

    // TypeScript is the one that works, and must keep working.
    let out = app.join("bindings-ts");
    harbour_run(
        &home,
        &app,
        &[
            "ffi",
            "generate",
            "--lang",
            "typescript",
            "--output",
            out.to_str().unwrap(),
        ],
    )
    .success();
    let generated: Vec<_> = fs::read_dir(&out)
        .unwrap_or_else(|e| panic!("typescript must generate into {}: {e}", out.display()))
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    assert!(
        generated.iter().any(|f| f.ends_with(".ts")),
        "a `.ts` file must be produced: {generated:?}"
    );
}

/// `harbour add --optional` writes `optional = true`, and the manifest it
/// produces loads.
///
/// The flag used to refuse, because the key it wrote changed nothing. Now
/// that it does something, the round trip is the thing worth asserting: the
/// spelling `add` writes has to be the spelling the parser reads, and the
/// added dependency must be *absent* from the build until a feature asks for
/// it (which no feature here does).
#[test]
fn test_harbour_add_optional_writes_a_key_the_parser_reads() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let app = tmp.path().join("addopt");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(app.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();
    let manifest = "[package]\n\
                    name = \"addopt\"\n\
                    version = \"0.1.0\"\n\
                    \n\
                    [targets.addopt]\n\
                    kind = \"exe\"\n\
                    sources = [\"src/main.c\"]\n";
    fs::write(app.join("Harbour.toml"), manifest).unwrap();

    let run = harbour_run(
        &home,
        &app,
        &[
            "add",
            "zlib",
            "--version",
            "1.3.1",
            "--optional",
            "--offline",
        ],
    );
    run.success();

    let written = fs::read_to_string(app.join("Harbour.toml")).unwrap();
    assert!(
        written.contains("optional = true"),
        "`--optional` must write the key\n{written}"
    );

    // The manifest `add` just wrote has to load. `--offline` with no
    // registry cache means the dependency cannot be fetched, so a build
    // that *succeeds* is itself the assertion that the optional dependency
    // was never fetched: nothing activated `zlib`, so nothing asked its
    // source a question.
    harbour_run(&home, &app, &["build", "--offline"]).success();
    assert!(built_exe_path_in(&app, "debug", "addopt").exists());
}

/// `default-features = false` turns default features off, and a misspelled
/// key in the same table fails the build.
///
/// Two halves of one defect. `DetailedDependencySpec.default_features` had
/// no serde rename, so the hyphenated spelling -- the only one anyone
/// writes, the one Cargo uses, the one this crate's own tests use, and the
/// one `manifest.rs` lists in its "you meant the package-level table" hint
/// -- was absorbed as an unknown key and thrown away. The dependency was
/// built with its default features on, silently. The same table absorbed
/// `brnach` and `verison`, where the value being thrown away decides which
/// source is fetched.
///
/// Asserted on the compile database rather than on a parsed field, because
/// what matters is whether the *dependency's compile* changed.
#[test]
fn test_default_features_false_reaches_the_build_and_a_typo_does_not_pass() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let lib = tmp.path().join("featlib");
    fs::create_dir_all(lib.join("src")).unwrap();
    fs::write(
        lib.join("src/l.c"),
        "#ifdef WITH_EXTRA\n\
         int extra(void) { return 1; }\n\
         #endif\n\
         int l(void) { return 2; }\n",
    )
    .unwrap();
    fs::write(
        lib.join("Harbour.toml"),
        "[package]\n\
         name = \"featlib\"\n\
         version = \"1.0.0\"\n\
         \n\
         [features]\n\
         default = [\"extra\"]\n\
         extra = []\n\
         \n\
         [targets.featlib]\n\
         kind = \"staticlib\"\n\
         sources = [\"src/l.c\"]\n\
         \n\
         [[targets.featlib.when]]\n\
         feature = \"extra\"\n\
         defines = [\"WITH_EXTRA=1\"]\n",
    )
    .unwrap();

    let app = tmp.path().join("featapp");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(app.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();

    let manifest = |dep_keys: &str| {
        format!(
            "[package]\n\
             name = \"featapp\"\n\
             version = \"0.1.0\"\n\
             \n\
             [dependencies]\n\
             featlib = {{ path = \"../featlib\"{dep_keys} }}\n\
             \n\
             [targets.featapp]\n\
             kind = \"exe\"\n\
             sources = [\"src/main.c\"]\n\
             \n\
             [targets.featapp.deps]\n\
             featlib = \"featlib\"\n"
        )
    };

    let extra_is_on = |app: &std::path::Path| -> bool {
        fs::read_to_string(app.join(".harbour/compile_commands.json"))
            .unwrap()
            .contains("WITH_EXTRA")
    };

    // Say nothing: the default feature is on.
    fs::write(app.join("Harbour.toml"), manifest("")).unwrap();
    harbour_run(&home, &app, &["build"]).success();
    assert!(
        extra_is_on(&app),
        "with no opt-out the dependency's default feature must be enabled"
    );

    // Opt out with the hyphenated spelling: it must actually take effect.
    fs::remove_dir_all(app.join(".harbour")).unwrap();
    fs::write(
        app.join("Harbour.toml"),
        manifest(", default-features = false"),
    )
    .unwrap();
    let run = harbour_run(&home, &app, &["build"]).success();
    assert!(
        !extra_is_on(&app),
        "`default-features = false` must reach the build -- it used to be \
         absorbed as an unknown key and the dependency kept its defaults\n{run}"
    );

    // The underscore spelling stays accepted, so manifests written against
    // the form that worked keep working.
    fs::remove_dir_all(app.join(".harbour")).unwrap();
    fs::write(
        app.join("Harbour.toml"),
        manifest(", default_features = false"),
    )
    .unwrap();
    harbour_run(&home, &app, &["build"]).success();
    assert!(!extra_is_on(&app), "the underscore alias must still work");

    // And a key that is neither fails the build, by name.
    fs::write(
        app.join("Harbour.toml"),
        manifest(", deafult-features = false"),
    )
    .unwrap();
    let run = harbour_run(&home, &app, &["build"]);
    assert!(
        !run.status.success(),
        "a misspelled dependency key must not be absorbed\n{run}"
    );
    assert!(
        run.combined().contains("deafult-features") && run.combined().contains("featlib"),
        "the diagnostic must name the key and the dependency\n{run}"
    );
}

/// Two settings that are *correct* to ignore, but were ignored silently.
///
/// Neither is rejected. A dependency's `[build]` is overridden by design --
/// one graph, one C++ ABI -- and `public_headers` genuinely does something
/// (it drives the private-define ABI lint and FFI header discovery), it just
/// does not add an include directory. What was missing in both cases was
/// anyone saying so, which is the whole complaint in #102 items 7 and 8.
///
/// Warnings rather than errors, so the assertion is "the build succeeds AND
/// says this" -- a rejection here would make a package unusable as a
/// dependency for describing its own build honestly.
#[test]
fn test_settings_that_are_ignored_by_design_say_so() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    // A dependency that sets the whole ABI-relevant `[build]` table, and
    // declares public headers without a public include dir.
    let lib = tmp.path().join("quietlib");
    fs::create_dir_all(lib.join("src")).unwrap();
    fs::create_dir_all(lib.join("include")).unwrap();
    fs::write(lib.join("src/l.c"), "int l(void) { return 1; }\n").unwrap();
    fs::write(lib.join("include/l.h"), "int l(void);\n").unwrap();
    let lib_manifest = |public: &str| {
        format!(
            "[package]\n\
             name = \"quietlib\"\n\
             version = \"1.0.0\"\n\
             \n\
             [build]\n\
             cpp_std = \"20\"\n\
             exceptions = false\n\
             rtti = false\n\
             \n\
             [targets.quietlib]\n\
             kind = \"staticlib\"\n\
             sources = [\"src/l.c\"]\n\
             public_headers = [\"include/*.h\"]\n\
             {public}"
        )
    };
    fs::write(lib.join("Harbour.toml"), lib_manifest("")).unwrap();

    let app = tmp.path().join("quietapp");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(app.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();
    fs::write(
        app.join("Harbour.toml"),
        "[package]\n\
         name = \"quietapp\"\n\
         version = \"0.1.0\"\n\
         \n\
         [dependencies]\n\
         quietlib = { path = \"../quietlib\" }\n\
         \n\
         [targets.quietapp]\n\
         kind = \"exe\"\n\
         sources = [\"src/main.c\"]\n\
         \n\
         [targets.quietapp.deps]\n\
         quietlib = \"quietlib\"\n",
    )
    .unwrap();

    let run = harbour_run(&home, &app, &["build"]).success();
    let said = run.combined();

    assert!(
        said.contains("quietlib") && said.contains("`[build]`") && said.contains("ignored"),
        "a dependency's `[build]` is not read, and the dependency's author \
         cannot see this build, so it has to be reported\n{run}"
    );
    assert!(
        said.contains("exceptions = false") && said.contains("rtti = false"),
        "and it has to name which settings were dropped\n{run}"
    );
    assert!(
        said.contains("requires_cpp"),
        "for `cpp_std` specifically there *is* a spelling that travels to a \
         consumer, and the warning should name it\n{run}"
    );
    assert!(
        said.contains("public_headers") && said.contains("include_dirs"),
        "declared public headers that no consumer can find must be reported\n{run}"
    );
    assert_eq!(
        run_built_exe(&app, "quietapp").status.code(),
        Some(0),
        "and all of this is a warning, not a failure"
    );

    // Adding the include dir silences that one and only that one.
    fs::write(
        lib.join("Harbour.toml"),
        lib_manifest("\n[targets.quietlib.public]\ninclude_dirs = [\"include\"]\n"),
    )
    .unwrap();
    let run = harbour_run(&home, &app, &["build"]).success();
    assert!(
        !run.combined().contains("consumers cannot include"),
        "with a public include dir there is nothing to warn about\n{run}"
    );
    assert!(
        run.combined().contains("`[build]`"),
        "the dependency's `[build]` is still being ignored, so that one stays\n{run}"
    );
}

/// A `symbol` probe must **link**, not merely compile.
///
/// That is the entire reason the kind exists separately from `header`: a
/// header declaring something the libc does not provide is the classic
/// `configure` trap, and a compile-only check answers `yes` to every one of
/// them. The witness here is a symbol that cannot possibly exist answering
/// `no` while real libc functions answer `yes`, both reported by the built
/// program rather than by a flag list.
#[test]
#[cfg(not(windows))]
fn symbol_probes_link_and_distinguish_real_functions_from_invented_ones() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let app = tmp.path().join("symbols");
    fs::create_dir_all(app.join("src")).unwrap();

    fs::write(
        app.join("Harbour.toml"),
        "[package]\n\
         name = \"symbols\"\n\
         version = \"0.1.0\"\n\
         \n\
         [targets.symbols]\n\
         kind = \"exe\"\n\
         sources = [\"src/main.c\"]\n\
         \n\
         [targets.symbols.probes]\n\
         check_symbols = [\n\
         \x20 \"strerror_r\",\n\
         \x20 \"gettimeofday\",\n\
         \x20 \"definitely_not_a_real_function_anywhere\",\n\
         ]\n",
    )
    .unwrap();
    fs::write(
        app.join("src/main.c"),
        "#include <stdio.h>\n\
         int main(void) {\n\
         #ifdef HAVE_STRERROR_R\n\
         \x20   printf(\"strerror_r=yes\\n\");\n\
         #else\n\
         \x20   printf(\"strerror_r=no\\n\");\n\
         #endif\n\
         #ifdef HAVE_GETTIMEOFDAY\n\
         \x20   printf(\"gettimeofday=yes\\n\");\n\
         #else\n\
         \x20   printf(\"gettimeofday=no\\n\");\n\
         #endif\n\
         #ifdef HAVE_DEFINITELY_NOT_A_REAL_FUNCTION_ANYWHERE\n\
         \x20   printf(\"bogus=yes\\n\");\n\
         #else\n\
         \x20   printf(\"bogus=no\\n\");\n\
         #endif\n\
         \x20   return 0;\n\
         }\n",
    )
    .unwrap();

    build_ok(&home, &app);
    let out = run_built_exe(&app, "symbols");

    assert!(
        out.out().contains("strerror_r=yes"),
        "a real libc function must probe as present:\n{}",
        out.out()
    );
    assert!(
        out.out().contains("gettimeofday=yes"),
        "a real libc function must probe as present:\n{}",
        out.out()
    );
    // The load-bearing half. A probe that answered `yes` to everything --
    // which is what a broken link step, or a compile-only check against a
    // fallback declaration that is never resolved, would produce -- fails
    // here and only here.
    assert!(
        out.out().contains("bogus=no"),
        "a symbol that exists nowhere must probe as absent. `yes` here means \
         the probe is not actually linking, so every `HAVE_<function>` is a \
         lie:\n{}",
        out.out()
    );
}

/// A symbol that is a *macro* rather than a function must still probe as
/// present.
///
/// Not hypothetical: on macOS `htonl` is a macro and `<arpa/inet.h>`
/// declares no function of that name at all, so `&htonl` does not compile.
/// A probe that only took the address answers `no` on every Mac for
/// something the package can call perfectly well. Verified by deleting the
/// `#if defined` branch from `symbol_snippet` and watching this flip.
///
/// Asserted on both platforms rather than guarded by `cfg(target_os)`,
/// because the answer must be `yes` either way -- macro on one, ordinary
/// function on the other, usable on both. A test that only ran where the
/// macro case occurs would pass on Linux for the wrong reason.
#[test]
#[cfg(not(windows))]
fn a_symbol_that_is_a_macro_probes_as_present() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let app = tmp.path().join("macrosym");
    fs::create_dir_all(app.join("src")).unwrap();

    fs::write(
        app.join("Harbour.toml"),
        "[package]\n\
         name = \"macrosym\"\n\
         version = \"0.1.0\"\n\
         \n\
         [targets.macrosym]\n\
         kind = \"exe\"\n\
         sources = [\"src/main.c\"]\n\
         \n\
         [targets.macrosym.probes.named.HAVE_HTONL]\n\
         symbol = \"htonl\"\n\
         prelude = [\"arpa/inet.h\"]\n",
    )
    .unwrap();
    fs::write(
        app.join("src/main.c"),
        "#include <stdio.h>\n\
         #include <arpa/inet.h>\n\
         int main(void) {\n\
         #ifdef HAVE_HTONL\n\
         \x20   printf(\"htonl=yes %u\\n\", (unsigned) htonl(1u));\n\
         #else\n\
         \x20   printf(\"htonl=no\\n\");\n\
         #endif\n\
         \x20   return 0;\n\
         }\n",
    )
    .unwrap();

    build_ok(&home, &app);
    let out = run_built_exe(&app, "macrosym");
    assert!(
        out.out().contains("htonl=yes"),
        "`htonl` is callable on every platform Harbour supports -- as a macro \
         on some and a function on others -- so it must probe as present. \
         `no` means the macro case is unhandled:\n{}",
        out.out()
    );
}

/// `libs` puts the library on the probe's link line, which is what subsumes
/// `AC_CHECK_LIB` rather than needing a separate probe kind.
///
/// The assertion is deliberately weak in one direction and strong in the
/// other. `cbrt` must be found *with* `-lm` on every platform; whether it is
/// also found without depends on the libc (glibc 2.34+ merged libm into
/// libc, and Apple never separated them), so asserting `no` there would be
/// asserting a property of the machine rather than of Harbour.
#[test]
#[cfg(not(windows))]
fn libs_reaches_the_probe_link_line() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let app = tmp.path().join("libsym");
    fs::create_dir_all(app.join("src")).unwrap();

    fs::write(
        app.join("Harbour.toml"),
        "[package]\n\
         name = \"libsym\"\n\
         version = \"0.1.0\"\n\
         \n\
         [targets.libsym]\n\
         kind = \"exe\"\n\
         sources = [\"src/main.c\"]\n\
         \n\
         [targets.libsym.public]\n\
         system_libs = [\"m\"]\n\
         \n\
         [targets.libsym.probes.named.HAVE_CBRT_IN_LIBM]\n\
         symbol = \"cbrt\"\n\
         prelude = [\"math.h\"]\n\
         libs = [\"m\"]\n\
         \n\
         [targets.libsym.probes.named.HAVE_NOTHING_IN_LIBM]\n\
         symbol = \"definitely_not_in_libm_either\"\n\
         libs = [\"m\"]\n",
    )
    .unwrap();
    fs::write(
        app.join("src/main.c"),
        "#include <stdio.h>\n\
         #include <math.h>\n\
         int main(void) {\n\
         #ifdef HAVE_CBRT_IN_LIBM\n\
         \x20   printf(\"cbrt=yes %.0f\\n\", cbrt(27.0));\n\
         #else\n\
         \x20   printf(\"cbrt=no\\n\");\n\
         #endif\n\
         #ifdef HAVE_NOTHING_IN_LIBM\n\
         \x20   printf(\"nothing=yes\\n\");\n\
         #else\n\
         \x20   printf(\"nothing=no\\n\");\n\
         #endif\n\
         \x20   return 0;\n\
         }\n",
    )
    .unwrap();

    build_ok(&home, &app);
    let out = run_built_exe(&app, "libsym");
    assert!(
        out.out().contains("cbrt=yes 3"),
        "`cbrt` must be found with `-lm`, and the built program must be able \
         to call it:\n{}",
        out.out()
    );
    // Without this, "libs made the link succeed" is indistinguishable from
    // "the link always succeeds".
    assert!(
        out.out().contains("nothing=no"),
        "adding `libs` must not make every symbol resolve:\n{}",
        out.out()
    );
}

/// A `symbol` probe's answer depends on its `prelude`, and both answers are
/// correct.
///
/// `fdatasync` on macOS links but is not declared in `<unistd.h>`. With no
/// prelude the fallback declaration finds it; with `unistd.h` the compile
/// fails. Those are different questions -- "can I call this if I declare it
/// myself" versus "can I call this the way the header offers it" -- which is
/// why `prelude` is part of the probe's cache key rather than an
/// implementation detail.
///
/// The test asserts the *mechanism* (the two are asked independently and
/// cached separately) rather than a specific platform's answers, since which
/// way they differ is a property of the libc.
#[test]
#[cfg(not(windows))]
fn the_prelude_is_part_of_the_question_a_symbol_probe_asks() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let (shim, records) = install_cc_recorder(tmp.path());
    let app = tmp.path().join("preludesym");
    fs::create_dir_all(app.join("src")).unwrap();

    fs::write(
        app.join("Harbour.toml"),
        "[package]\n\
         name = \"preludesym\"\n\
         version = \"0.1.0\"\n\
         \n\
         [targets.preludesym]\n\
         kind = \"exe\"\n\
         sources = [\"src/main.c\"]\n\
         \n\
         [targets.preludesym.probes.named.HAVE_BARE]\n\
         symbol = \"strerror_r\"\n\
         \n\
         [targets.preludesym.probes.named.HAVE_VIA_HEADER]\n\
         symbol = \"strerror_r\"\n\
         prelude = [\"string.h\"]\n",
    )
    .unwrap();
    fs::write(app.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();

    harbour_run_env(&home, &app, &["build"], &[("CC", shim.to_str().unwrap())]).success();

    // Two probes for one symbol must have been measured separately -- a
    // shared cache entry keyed on the symbol alone would answer the second
    // from the first, and the two are not the same question.
    let argvs = recorded_argvs(&records);
    let snippets: Vec<&Vec<String>> = argvs
        .iter()
        .filter(|a| a.iter().any(|x| x.contains("probe.c")))
        .collect();
    assert!(
        snippets.len() >= 2,
        "both `strerror_r` probes must be measured; only {} probe compile(s) \
         were recorded, so one was served from the other's cache entry",
        snippets.len()
    );

    // And the snippets must actually differ: one includes <string.h>, the
    // other declares the symbol itself.
    let dirs: std::collections::BTreeSet<String> = snippets
        .iter()
        .filter_map(|a| a.iter().find(|x| x.contains("probe.c")))
        .cloned()
        .collect();
    assert!(
        dirs.len() >= 2,
        "each probe must get its own directory, or parallel probes race over \
         one path: {dirs:?}"
    );
}

/// `libs` on a kind that does not link is refused, not ignored.
#[test]
fn libs_on_a_non_linking_probe_kind_is_an_error() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let app = tmp.path().join("badlibs");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(app.join("src/main.c"), "int main(void){return 0;}\n").unwrap();
    fs::write(
        app.join("Harbour.toml"),
        "[package]\n\
         name = \"badlibs\"\n\
         version = \"0.1.0\"\n\
         \n\
         [targets.badlibs]\n\
         kind = \"exe\"\n\
         sources = [\"src/main.c\"]\n\
         \n\
         [targets.badlibs.probes.named.HAVE_POLL_H]\n\
         header = \"poll.h\"\n\
         libs = [\"m\"]\n",
    )
    .unwrap();

    let log = harbour_run(&home, &app, &["build"]);
    assert!(
        !log.status.success(),
        "a `libs` key on a non-linking probe kind must fail the build:\n{log}"
    );
    assert!(
        log.combined().contains("only a `symbol` probe uses"),
        "a `header` probe has no link line, so `libs` must be refused rather \
         than parsed and dropped:\n{}",
        log.combined()
    );
}

/// Write a target whose probe answers go into a generated header rather than
/// onto the command line.
#[cfg(not(windows))]
fn write_header_fixture(dir: &std::path::Path, disable_value: u32) {
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join("Harbour.toml"),
        format!(
            "[package]\n\
             name = \"hdr\"\n\
             version = \"0.1.0\"\n\
             \n\
             [targets.hdr]\n\
             kind = \"exe\"\n\
             sources = [\"src/main.c\"]\n\
             \n\
             [targets.hdr.probes]\n\
             emit = {{ header = \"gen_config.h\" }}\n\
             defines = [\"GEN_DISABLE_FTP={disable_value}\", \"GEN_OS=\\\"harbour\\\"\", \"GEN_FLAG\"]\n\
             check_headers = [\"stdio.h\", \"definitely/not/real.h\"]\n\
             check_symbols = [\"poll\", \"definitely_not_a_real_function\"]\n\
             check_sizeof = [\"long\", \"void *\"]\n"
        ),
    )
    .unwrap();
    // The package includes the header *by name*, which is the case a flag
    // list cannot serve and the whole reason this mode exists.
    fs::write(
        dir.join("src/main.c"),
        "#include <stdio.h>\n\
         #include \"gen_config.h\"\n\
         int main(void) {\n\
         \x20   printf(\"ftp=%d\\n\", GEN_DISABLE_FTP);\n\
         \x20   printf(\"os=%s\\n\", GEN_OS);\n\
         #ifdef GEN_FLAG\n\
         \x20   printf(\"flag=yes\\n\");\n\
         #endif\n\
         #ifdef HAVE_STDIO_H\n\
         \x20   printf(\"stdio=yes\\n\");\n\
         #else\n\
         \x20   printf(\"stdio=no\\n\");\n\
         #endif\n\
         #ifdef HAVE_DEFINITELY_NOT_REAL_H\n\
         \x20   printf(\"bogus_header=yes\\n\");\n\
         #else\n\
         \x20   printf(\"bogus_header=no\\n\");\n\
         #endif\n\
         #ifdef HAVE_POLL\n\
         \x20   printf(\"poll=yes\\n\");\n\
         #else\n\
         \x20   printf(\"poll=no\\n\");\n\
         #endif\n\
         #ifdef HAVE_DEFINITELY_NOT_A_REAL_FUNCTION\n\
         \x20   printf(\"bogus_symbol=yes\\n\");\n\
         #else\n\
         \x20   printf(\"bogus_symbol=no\\n\");\n\
         #endif\n\
         \x20   printf(\"long=%d ptr=%d\\n\", SIZEOF_LONG, SIZEOF_VOID_P);\n\
         \x20   return 0;\n\
         }\n",
    )
    .unwrap();
}

/// The generated header's path, which is fixed by `probe_include_dir`.
#[cfg(not(windows))]
fn generated_header_path(dir: &std::path::Path) -> PathBuf {
    dir.join(".harbour/target/debug/probe/hdr-0.1.0/hdr/include/gen_config.h")
}

/// `emit = { header = "..." }` must write a header the package can
/// `#include` by name, carrying both measured answers and declared literals.
///
/// This is the mode curl and openssl need and the reason it exists: 253
/// config lines are not expressible as `-D` flags, and those packages
/// `#include` a config header *by name*, so no arrangement of flags serves
/// them.
///
/// The witness is the built program, which reports what its preprocessor
/// saw. Nothing here arrives as a `-D` -- asserted separately below.
#[test]
#[cfg(not(windows))]
fn a_generated_config_header_carries_answers_and_literals() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let app = tmp.path().join("hdr");
    write_header_fixture(&app, 1);

    build_ok(&home, &app);
    let header = fs::read_to_string(generated_header_path(&app))
        .expect("the generated header must exist where `probe_include_dir` says");

    // Literals, in declaration order, before the measured answers.
    assert!(header.contains("#define GEN_DISABLE_FTP 1"), "{header}");
    assert!(header.contains("#define GEN_OS \"harbour\""), "{header}");
    // A value-less literal becomes `1`, matching how `-DFOO` behaves.
    assert!(header.contains("#define GEN_FLAG 1"), "{header}");

    // Measured answers.
    assert!(header.contains("#define HAVE_STDIO_H 1"), "{header}");
    assert!(header.contains("#define HAVE_POLL 1"), "{header}");
    assert!(header.contains("#define SIZEOF_LONG "), "{header}");

    // A false answer is a commented-out `#undef`, which is what autoconf and
    // CMake produce. It is not decoration: it records that the question was
    // *asked and answered no*, which is what distinguishes this file from a
    // header that forgot something. A reader diffing it against a vendored
    // `curl_config.h` sees the same shape.
    assert!(
        header.contains("/* #undef HAVE_DEFINITELY_NOT_REAL_H */"),
        "a false answer must be recorded as a commented `#undef`, not \
         omitted:\n{header}"
    );
    assert!(
        header.contains("/* #undef HAVE_DEFINITELY_NOT_A_REAL_FUNCTION */"),
        "{header}"
    );
    // And never as `=0`, which `#ifdef` would accept.
    assert!(
        !header.contains("#define HAVE_DEFINITELY_NOT_REAL_H 0"),
        "{header}"
    );

    // The include guard is prefixed. `GEN_CONFIG_H` is a guard a package's
    // own vendored copy may already define, and a header whose guard is
    // already defined expands to nothing -- a build that fails on missing
    // macros with no mention of this file.
    assert!(
        header.contains("#ifndef HARBOUR_PROBE_GEN_CONFIG_H"),
        "{header}"
    );

    let out = run_built_exe(&app, "hdr");
    for expected in [
        "ftp=1",
        "os=harbour",
        "flag=yes",
        "stdio=yes",
        "bogus_header=no",
        "poll=yes",
        "bogus_symbol=no",
    ] {
        assert!(
            out.out().contains(expected),
            "the built program must see `{expected}` through the generated \
             header:\n{}",
            out.out()
        );
    }
}

/// When a header is emitted, the answers must **not** also arrive as `-D`
/// flags.
///
/// Two sources for one fact is the defect shape this whole subsystem has
/// been audited against. The `contribution` function is the single place
/// `emit` is interpreted precisely so this cannot happen; this is the test
/// that says so.
#[test]
#[cfg(not(windows))]
fn emitting_a_header_puts_nothing_on_the_command_line() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let (shim, records) = install_cc_recorder(tmp.path());
    let app = tmp.path().join("hdr");
    write_header_fixture(&app, 1);

    harbour_run_env(&home, &app, &["build"], &[("CC", shim.to_str().unwrap())]).success();

    let argvs = recorded_argvs(&records);
    let real = argvs
        .iter()
        .find(|a| a.iter().any(|x| x.ends_with("main.c")))
        .expect("a compile of main.c");

    let probe_defines: Vec<&String> = real
        .iter()
        .filter(|a| {
            a.starts_with("-DHAVE_") || a.starts_with("-DSIZEOF_") || a.starts_with("-DGEN_")
        })
        .collect();
    assert!(
        probe_defines.is_empty(),
        "with `emit = {{ header = ... }}` the answers live in the file; \
         putting them on the command line as well would be two sources for \
         one fact: {probe_defines:?}"
    );

    // What must be there instead is the `-I`, and it must be **first**: `-I`
    // is first-match-wins, so a package that still vendors a `config.h` of
    // the same name has to get the generated one. That is the migration path
    // off the vendored file.
    let includes: Vec<&String> = real.iter().filter(|a| a.starts_with("-I")).collect();
    let first = includes.first().unwrap_or_else(|| {
        panic!("the generated header's dir must be on the include path: {real:?}")
    });
    assert!(
        first.contains("probe") && first.contains("include"),
        "the generated header's directory must come first on the include \
         path, so it wins over a vendored copy of the same name. Got: \
         {includes:?}"
    );
}

/// The generated header's *content* must be a compile fingerprint input.
///
/// `probes.defines` is the isolating lever: those literals go only into the
/// header, never onto the command line, and the include directory's path
/// does not change. So changing one changes the file's bytes and nothing
/// else. If the object did not recompile, a changed probe answer could leave
/// a stale object -- and the design document listed this as *inferred*, on
/// the grounds that `collect_header_deps` is a textual scanner rather than a
/// preprocessor.
///
/// Worth recording how this was first mis-measured: hand-editing the
/// generated header and rebuilding proves nothing, because the build
/// regenerates it during planning, before any fingerprint is taken. The
/// edit is gone before anything hashes it.
#[test]
#[cfg(not(windows))]
fn changing_the_generated_header_recompiles_what_includes_it() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let app = tmp.path().join("hdr");

    write_header_fixture(&app, 1);
    build_ok(&home, &app);
    assert!(run_built_exe(&app, "hdr").out().contains("ftp=1"));

    // Only the header's bytes change.
    write_header_fixture(&app, 7);
    build_ok(&home, &app);
    assert!(
        run_built_exe(&app, "hdr").out().contains("ftp=7"),
        "the generated header changed, so the object must be recompiled. \
         `ftp=1` here means the header's content is not a fingerprint input \
         and a changed answer can survive into a rebuilt binary:\n{}",
        run_built_exe(&app, "hdr").out()
    );
}

/// The generated header must be byte-identical across clean builds.
///
/// It is a compile fingerprint input, so a line that moved between runs
/// would recompile every translation unit that includes it on every build,
/// forever. For curl that is 196 sources. This is also where `HashMap`
/// iteration would show up, and a measured 18 distinct link orders across 40
/// clean runs of one manifest was fixed only days ago.
#[test]
#[cfg(not(windows))]
fn the_generated_header_is_byte_identical_across_clean_builds() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let app = tmp.path().join("hdr");
    write_header_fixture(&app, 1);

    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..6 {
        fs::remove_dir_all(app.join(".harbour")).ok();
        build_ok(&home, &app);
        seen.insert(fs::read_to_string(generated_header_path(&app)).expect("header"));
    }
    assert_eq!(
        seen.len(),
        1,
        "6 clean builds produced {} different generated headers:\n{:#?}",
        seen.len(),
        seen
    );
}

/// `probes.defines` without a generated header is refused.
#[test]
fn literal_probe_defines_require_a_generated_header() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let app = tmp.path().join("baddefs");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(app.join("src/main.c"), "int main(void){return 0;}\n").unwrap();
    fs::write(
        app.join("Harbour.toml"),
        "[package]\n\
         name = \"baddefs\"\n\
         version = \"0.1.0\"\n\
         \n\
         [targets.baddefs]\n\
         kind = \"exe\"\n\
         sources = [\"src/main.c\"]\n\
         \n\
         [targets.baddefs.probes]\n\
         defines = [\"FOO=1\"]\n\
         check_headers = [\"stdio.h\"]\n",
    )
    .unwrap();

    let log = harbour_run(&home, &app, &["build"]);
    assert!(!log.status.success(), "must fail:\n{log}");
    assert!(
        log.combined().contains("requires"),
        "without a header these are just compile defines, which \
         `[targets.X.private]` already spells -- offering a second spelling \
         invites the reader to look for a difference:\n{}",
        log.combined()
    );
}

/// Probe answers are cached against declarations, **not** against the
/// contents of the filesystem. This pins the behaviour Harbour actually has.
///
/// A characterization test, in the same spirit as
/// `probe_answers_do_not_reach_a_dependent`: it asserts the real behaviour
/// *and* makes the boundary legible, so the limitation is discovered by
/// reading a test rather than by debugging a wrong `#define`.
///
/// The cache key is the toolchain fingerprint, a hash of the pre-probe
/// compile surface, and each probe's own spec. None of those mentions
/// filesystem content, and **none of them can**: the input to a *negative*
/// answer is the absence of a file, so there is no finite set of paths to
/// watch for invalidation. The only alternative to declaration-keyed caching
/// is re-running every probe on every build -- 199 compiler spawns for curl.
/// CMake's `CMakeCache.txt` and autoconf's `config.cache` make the same
/// trade.
///
/// The consequence: installing a system header, or changing SDKs without
/// changing the compiler version, leaves the previous answer in place.
/// `harbour clean --probes` is the way out, and it exists so the way out is
/// not `clean --all`.
#[test]
#[cfg(not(windows))]
fn probe_answers_are_cached_against_declarations_not_the_filesystem() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let app = tmp.path().join("fscache");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::create_dir_all(app.join("vendored")).unwrap();

    // `vendored` is on the include path from the start, so the manifest never
    // changes and neither does the surface key. The only thing that changes
    // is whether a file exists inside a directory already being searched.
    fs::write(
        app.join("Harbour.toml"),
        "[package]\n\
         name = \"fscache\"\n\
         version = \"0.1.0\"\n\
         \n\
         [targets.fscache]\n\
         kind = \"exe\"\n\
         sources = [\"src/main.c\"]\n\
         \n\
         [targets.fscache.private]\n\
         include_dirs = [\"vendored\"]\n\
         \n\
         [targets.fscache.probes]\n\
         check_headers = [\"appears_later.h\"]\n",
    )
    .unwrap();
    fs::write(
        app.join("src/main.c"),
        "#include <stdio.h>\n\
         int main(void) {\n\
         #ifdef HAVE_APPEARS_LATER_H\n\
         \x20   printf(\"answer=yes\\n\");\n\
         #else\n\
         \x20   printf(\"answer=no\\n\");\n\
         #endif\n\
         \x20   return 0;\n\
         }\n",
    )
    .unwrap();

    let answer = |home: &std::path::Path| -> String {
        build_ok(home, &app);
        run_built_exe(&app, "fscache").out().to_string()
    };

    assert_eq!(answer(&home), "answer=no", "the header does not exist yet");

    // It appears, on a path already being searched.
    fs::write(app.join("vendored/appears_later.h"), "/* now here */\n").unwrap();
    assert_eq!(
        answer(&home),
        "answer=no",
        "documented limitation: the probe cache is keyed on declarations, so \
         a file appearing on an unchanged include path does not invalidate \
         it. If this now says `yes`, filesystem-sensitive invalidation has \
         been implemented -- update MANIFEST.md, the design document and this \
         test together"
    );

    // The way out, which must not cost the compiled objects.
    harbour_run(&home, &app, &["clean", "--probes"]).success();
    assert_eq!(
        answer(&home),
        "answer=yes",
        "`clean --probes` must re-measure"
    );

    // And the same in the other direction: a stale `yes` is the more
    // dangerous one, because the package compiles code for a feature that is
    // no longer there.
    fs::remove_file(app.join("vendored/appears_later.h")).unwrap();
    assert_eq!(
        answer(&home),
        "answer=yes",
        "the same limitation in reverse: a header disappearing leaves a stale \
         `yes`"
    );
    harbour_run(&home, &app, &["clean", "--probes"]).success();
    assert_eq!(answer(&home), "answer=no");
}

/// `clean --probes` must re-measure probes and keep compiled objects.
///
/// That is the entire reason it exists rather than telling people to run
/// `clean`. "My probe answer is wrong and the only fix is a full rebuild" is
/// a bad failure mode for a package author iterating on a shim.
#[test]
#[cfg(not(windows))]
fn clean_probes_keeps_compiled_objects() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let app = tmp.path().join("keepobj");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(
        app.join("Harbour.toml"),
        "[package]\n\
         name = \"keepobj\"\n\
         version = \"0.1.0\"\n\
         \n\
         [targets.keepobj]\n\
         kind = \"exe\"\n\
         sources = [\"src/main.c\"]\n\
         \n\
         [targets.keepobj.probes]\n\
         check_headers = [\"stdio.h\"]\n",
    )
    .unwrap();
    fs::write(app.join("src/main.c"), "int main(void){return 0;}\n").unwrap();

    build_ok(&home, &app);
    let obj = find_one(&app, "main.o");
    let before = fs::metadata(&obj).unwrap().modified().unwrap();

    let log = harbour_run(&home, &app, &["clean", "--probes"]).success();
    assert!(
        log.combined().contains("probe cache"),
        "the command must say what it removed:\n{}",
        log.combined()
    );
    assert!(
        obj.exists(),
        "`clean --probes` must keep compiled objects; it is not `clean` with \
         extra steps"
    );

    // The next build re-measures the probe and reuses the object.
    let rebuild = harbour_run(&home, &app, &["build"]).success();
    assert!(
        rebuild.combined().contains("up to date"),
        "the object must be reused after `clean --probes`:\n{}",
        rebuild.combined()
    );
    assert_eq!(
        fs::metadata(&obj).unwrap().modified().unwrap(),
        before,
        "the object must not have been rewritten"
    );
}

/// The package's own object file, found by name under the build tree.
#[cfg(not(windows))]
fn find_one(root: &std::path::Path, name: &str) -> PathBuf {
    fn walk(dir: &std::path::Path, name: &str, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                // Skip the probe scratch tree: it contains its own
                // `probe.o`, and matching that instead would make this
                // assert the opposite of what it means to.
                if p.file_name().is_some_and(|n| n == "probe") {
                    continue;
                }
                walk(&p, name, out);
            } else if p.file_name().is_some_and(|n| n == name) {
                out.push(p);
            }
        }
    }
    let mut found = Vec::new();
    walk(root, name, &mut found);
    found.sort();
    found
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("no `{name}` under {}", root.display()))
}

// ============================================================================
// MSVC probe argv construction, measured from a Unix host.
//
// Spike document: docs/superpowers/specs/2026-09-12-msvc-probes-spike.md
//
// `ProbeEnv` takes its toolchain as a `&dyn Toolchain`, so the entire
// argv-construction chain for a probe can be exercised with an
// `MsvcToolchain` whose `cl.exe`/`link.exe` are a recording shell script.
// That proves what Harbour *would* hand `cl` and `link`, on any host, with
// no Windows involved. It does **not** prove `cl` accepts it -- that is what
// the `windows-latest` job is for.
//
// Shell shim, hence `cfg(unix)`: `CreateProcessW` cannot execute a `.bat`
// directly, so this technique does not transplant to Windows. It does not
// need to -- on Windows the real `cl` is available.
// ============================================================================

/// A shim that records its argv and succeeds, standing in for `cl.exe`,
/// `lib.exe` and `link.exe` at once.
#[cfg(unix)]
fn install_msvc_recorder(tmp: &std::path::Path) -> (PathBuf, PathBuf) {
    let records = tmp.join("msvc-argv");
    fs::create_dir_all(&records).unwrap();
    let shim = tmp.join("fake-cl");
    fs::write(
        &shim,
        format!(
            "#!/bin/sh\n\
             out=\"{}/$$.$(od -An -N2 -tu2 /dev/urandom | tr -d ' ')\"\n\
             printf '%s\\n' \"$@\" > \"$out\"\n\
             exit 0\n",
            records.display()
        ),
    )
    .unwrap();
    let mut perms = fs::metadata(&shim).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    fs::set_permissions(&shim, perms).unwrap();
    (shim, records)
}

/// What Harbour hands `cl.exe` and `link.exe` when answering probes.
///
/// Every probe answers `Present` here, because the shim exits 0 no matter
/// what -- the answers are meaningless and are not asserted on. The argv is
/// the whole point.
#[cfg(unix)]
#[test]
fn msvc_probe_command_lines_are_msvc_shaped_and_libm_does_not_exist() {
    use harbour::builder::probe::{run_probes, ProbeEnv};
    use harbour::builder::toolchain::MsvcToolchain;
    use harbour::core::probe::{ProbeKind, ProbeSet};
    use harbour::core::target::{CStandard, CStandardSpec};

    let tmp = temp_dir();
    let (shim, records) = install_msvc_recorder(tmp.path());
    let toolchain = MsvcToolchain::new(shim.clone(), shim.clone(), shim.clone());

    let mut set = ProbeSet::default();
    set.probes.insert(
        "HAVE_WINDOWS_H".to_string(),
        ProbeKind::Header {
            header: "windows.h".to_string(),
            prelude: vec![],
        },
    );
    set.probes.insert(
        "SIZEOF_TIME_T".to_string(),
        ProbeKind::Sizeof {
            ty: "time_t".to_string(),
            prelude: vec![],
        },
    );
    // The entry this test exists for: a manifest written for Unix says
    // `libs = ["m"]`, because that is where `sqrt` lives on every Unix.
    set.probes.insert(
        "HAVE_SQRT".to_string(),
        ProbeKind::Symbol {
            symbol: "sqrt".to_string(),
            prelude: vec!["math.h".to_string()],
            libs: vec!["m".to_string()],
        },
    );

    let scratch = tmp.path().join("scratch");
    let env = ProbeEnv {
        toolchain: &toolchain,
        include_dirs: vec![PathBuf::from("/inc/one")],
        defines: vec![("_WIN32_WINNT".to_string(), Some("0x0601".to_string()))],
        target_cflags: vec![],
        target_ldflags: vec![],
        c_std: Some(CStandardSpec::iso(CStandard::C11)),
        scratch: scratch.clone(),
        toolchain_key: "fake-msvc".to_string(),
    };

    run_probes(&env, &set, "spike/msvc").expect("the shim always exits 0");

    let argvs = recorded_argvs(&records);
    assert!(!argvs.is_empty(), "no command was recorded at all");

    // --- the compile line ---
    let compile = argvs
        .iter()
        .find(|a| a.iter().any(|x| x.ends_with("probe.c")))
        .expect("a probe compile must have been recorded");

    assert_eq!(
        compile[0], "/nologo",
        "MSVC probe compiles must be `cl`-shaped: {compile:?}"
    );
    assert!(
        compile.contains(&"/c".to_string()),
        "a probe compile must not link: {compile:?}"
    );
    assert!(
        compile.iter().any(|a| a.starts_with("/Fo")),
        "MSVC names its object with `/Fo`, not `-o`: {compile:?}"
    );
    assert!(
        compile.iter().any(|a| a.ends_with("probe.obj")),
        "the probe object must use MSVC's `.obj` extension: {compile:?}"
    );
    assert!(
        compile.contains(&"/I/inc/one".to_string()),
        "the surface's include dirs must reach the probe as `/I`: {compile:?}"
    );
    assert!(
        compile.contains(&"/D_WIN32_WINNT=0x0601".to_string()),
        "the surface's defines must reach the probe as `/D`: {compile:?}"
    );
    assert!(
        compile.contains(&"/std:c11".to_string()),
        "a target pinning `c_std = \"11\"` must have its probes measured in \
         that dialect: {compile:?}"
    );
    assert!(
        !compile.iter().any(|a| a.starts_with("-")),
        "nothing GCC-shaped may reach `cl`: {compile:?}"
    );

    // --- the link line, and the bugs ---
    let link = argvs
        .iter()
        .find(|a| a.iter().any(|x| x == "m.lib"))
        .expect("the symbol probe must have produced a link");

    // Found by running this, not by reading: every extension accessor in
    // this codebase is dotless (`"exe"`, `"obj"`, `"lib"`), and
    // `TargetKind::output_filename` joins them with an explicit `.`.
    // `src/builder/probe.rs` writes `format!("probe{}", exe_extension())`
    // for the executable while correctly writing `format!("probe.{}",
    // object_extension())` for the object -- so the MSVC probe executable is
    // named `probeexe`. Harmless today, because a probe only reads the exit
    // code and never opens the file; recorded here so the next reader does
    // not have to rediscover it.
    assert!(
        link.iter().any(|a| a.contains("probeexe")),
        "documenting the dotless-extension defect in src/builder/probe.rs; \
         if this assertion starts failing the bug has been fixed and this \
         test should be inverted: {link:?}"
    );
    // The concrete defect this spike was asked about. `m.lib` does not exist in any MSVC
    // installation -- the math functions are in the CRT, which `cl`'s
    // embedded `/DEFAULTLIB` directives already pull in. `link.exe` answers
    // a missing library with LNK1104 and a non-zero exit, which
    // `src/builder/probe.rs` reads as "the symbol is absent". So every
    // `libs = ["m"]` symbol probe answers `no` on Windows for a function
    // that is right there.
    assert!(
        link.contains(&"m.lib".to_string()),
        "the mapping under test: `libs = [\"m\"]` becomes `m.lib`: {link:?}"
    );
    assert!(
        !link.iter().any(|a| a == "-lm"),
        "the GCC spelling must not reach `link.exe`: {link:?}"
    );

    // --- the snippets, as `cl` would see them ---
    let mut sizeof_src = String::new();
    let mut stack = vec![scratch.clone()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.file_name().is_some_and(|n| n == "probe.c") {
                let text = fs::read_to_string(&p).unwrap();
                if text.contains("sizeof(time_t)") {
                    sizeof_src = text;
                }
            }
        }
    }
    assert!(
        sizeof_src.contains("#ifdef __has_include"),
        "the `sizeof` preamble must be `__has_include`-guarded: {sizeof_src}"
    );
    assert!(
        sizeof_src.contains("? 1 : -1"),
        "the `sizeof` mechanism is a negative array bound: {sizeof_src}"
    );
}

// ============================================================================
// MSVC probes, measured on a real `cl.exe`.
//
// Spike document: docs/superpowers/specs/2026-09-12-msvc-probes-spike.md
//
// Every probe integration test in this file above here is
// `cfg(not(windows))`, because they witness a compile through a recording
// `CC` shell shim and Windows has no equivalent -- `CreateProcessW` cannot
// execute a `.bat`, and pointing `CC` at anything makes Harbour pick its
// GCC-shaped toolchain, which is not the thing under test.
//
// The witness that *does* work on Windows is the generated config header.
// It is written by the real probe run from real `cl.exe` and `link.exe` exit
// codes, so asserting on its contents turns "MSVC probes presumably work"
// into a measurement. That is what these two tests are for, and the reason
// they exist at all: the whole probe subsystem had never been run under MSVC
// once.
// ============================================================================

/// Build a package whose probes emit a header, and return that header's text.
///
/// Panics with the build log if the build failed -- which is itself part of
/// the claim, because a `sizeof` probe that cannot see its type is a hard
/// error rather than a wrong answer, and the probe baseline check compiles
/// *and links* an empty program before anything else runs.
#[cfg(target_env = "msvc")]
fn probe_header_for(
    tmp: &std::path::Path,
    home: &std::path::Path,
    label: &str,
    probes: &str,
) -> String {
    let dir = tmp.join(label);
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join("Harbour.toml"),
        format!(
            "[package]\nname = \"probed\"\nversion = \"0.1.0\"\n\n\
             [targets.probed]\nkind = \"exe\"\nsources = [\"src/main.c\"]\n\n\
             [targets.probed.probes]\nemit = {{ header = \"probe_config.h\" }}\n{probes}\n"
        ),
    )
    .unwrap();
    fs::write(
        dir.join("src/main.c"),
        "#include \"probe_config.h\"\nint main(void) { return 0; }\n",
    )
    .unwrap();

    harbour_run(home, &dir, &["build"]).success();

    let mut found = Vec::new();
    fn walk(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.file_name().is_some_and(|n| n == "probe_config.h") {
                out.push(p);
            }
        }
    }
    walk(&dir, &mut found);
    found.sort();
    let path = found
        .first()
        .unwrap_or_else(|| panic!("the build succeeded but wrote no generated header"));
    fs::read_to_string(path).unwrap()
}

/// The `header`, `sizeof` and `symbol` probe kinds, answered by a real MSVC
/// toolchain.
///
/// **Not every kind, and the doc comment said "all three probe kinds" while
/// six existed.** `type` and `constant` are unexercised on Windows, and so
/// was `flag` for as long as it existed -- which mattered, because `flag`
/// was the one kind with MSVC-specific behaviour (`/WX`, to make `D9002`
/// fatal) and that behaviour was never observed on a Windows host. The
/// `flag` kind has since been removed; `type` and `constant` remain
/// unmeasured here and that is a known gap rather than a covered case.
///
/// The answers here are all independently known, which is the property that
/// makes the test mean something: a probe subsystem returning a constant, or
/// one whose every answer is `no` because the toolchain is broken, fails
/// this.
///
/// Worth stating what each group establishes, because each was an open
/// question before this ran:
///
/// - `HAVE_*` for headers: `Toolchain::compile_command` is backend-agnostic
///   and `cl /c` reports a missing include with a non-zero exit, so the
///   `header` kind needs nothing MSVC-specific.
/// - `SIZEOF_TIME_T`: only reachable through the `__has_include`-guarded
///   preamble in `sizeof_preamble`. If `cl` did not honour
///   `#ifdef __has_include` the type would be invisible and the probe would
///   fail the build with `SizeOutOfRange`, so a number here is the proof
///   that the preamble fires. `SIZEOF_OFF_T` does the same for
///   `<sys/types.h>`, which exists in the UCRT.
/// - `SIZEOF_LONG 4`: MSVC's `long` is 32-bit on 64-bit Windows. A probe
///   subsystem that had quietly inherited a Unix answer would say 8.
/// - `HAVE_MALLOC` / `HAVE_POLL`: the `symbol` kind is the only one that
///   links, so these establish that `link.exe` resolves a CRT symbol from
///   the `/DEFAULTLIB` directives `cl` embeds in the object, with no
///   library named on the link line at all -- and that a symbol Windows
///   does not have answers `no` rather than erroring.
#[cfg(target_env = "msvc")]
#[test]
fn msvc_answers_every_probe_kind_correctly() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let header = probe_header_for(
        tmp.path(),
        &home,
        "msvc-probes",
        "check_headers = [\"stdio.h\", \"windows.h\", \"sys/types.h\", \
         \"unistd.h\", \"sys/socket.h\"]\n\
         check_sizeof = [\"int\", \"long\", \"size_t\", \"void *\", \"time_t\", \"off_t\"]\n\
         check_symbols = [\"printf\", \"malloc\", \"poll\", \"strerror_r\"]",
    );

    for present in [
        "#define HAVE_STDIO_H 1",
        "#define HAVE_WINDOWS_H 1",
        "#define HAVE_SYS_TYPES_H 1",
        // 4, not 8: MSVC keeps `long` 32-bit on 64-bit Windows.
        "#define SIZEOF_LONG 4",
        "#define SIZEOF_INT 4",
        "#define SIZEOF_SIZE_T 8",
        "#define SIZEOF_VOID_P 8",
        // The `__has_include` preamble fired. See the doc comment.
        "#define SIZEOF_TIME_T 8",
        "#define SIZEOF_OFF_T 4",
        // The `symbol` kind links, and `link.exe` found the CRT with no
        // library named on the command line.
        "#define HAVE_PRINTF 1",
        "#define HAVE_MALLOC 1",
    ] {
        assert!(
            header.contains(present),
            "expected `{present}` in the MSVC-generated config header:\n{header}"
        );
    }

    for absent in [
        // Genuinely not on Windows. A subsystem answering `yes` here would
        // configure a package for a POSIX platform and then fail to compile.
        "HAVE_UNISTD_H",
        "HAVE_SYS_SOCKET_H",
        "HAVE_POLL",
        "HAVE_STRERROR_R",
    ] {
        assert!(
            header.contains(&format!("/* #undef {absent} */")),
            "`{absent}` must be recorded as asked-and-answered-no:\n{header}"
        );
        assert!(
            !header.contains(&format!("#define {absent}")),
            "`{absent}` must not be defined at all:\n{header}"
        );
    }
}

/// **A known defect, demonstrated rather than fixed.** `#[ignore]`d on
/// purpose: it asserts the *wrong* behaviour, so leaving it live would block
/// the fix it exists to describe.
///
/// `libs = ["m"]` is how every Unix manifest asks for the math library, and
/// `MsvcToolchain::link_exe_command` renders a `libs` entry as `<name>.lib`.
/// There is no `m.lib` in any MSVC installation -- the math functions are in
/// the CRT, which `cl`'s embedded `/DEFAULTLIB` directives already pull in --
/// so `link.exe` fails on a missing input file and
/// `src/builder/probe.rs::compile_and_link` reads that as "the symbol is
/// absent".
///
/// Measured on `windows-latest` with MSVC 14.51: `HAVE_SQRT` is
/// `/* #undef */` with `libs = ["m"]` and `1` without it. The build
/// *succeeds* either way, so the failure is silent -- a package told there
/// is no `sqrt` on a platform that has one.
///
/// Run it with `cargo test -- --ignored msvc_libs_entry` on a Windows host.
#[cfg(target_env = "msvc")]
#[test]
#[ignore = "documents a defect; asserts the wrong answer on purpose"]
fn msvc_libs_entry_named_for_unix_makes_a_symbol_probe_answer_no() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let with_libm = probe_header_for(
        tmp.path(),
        &home,
        "msvc-libm",
        "[targets.probed.probes.named.HAVE_SQRT]\n\
         symbol = \"sqrt\"\nprelude = [\"math.h\"]\nlibs = [\"m\"]",
    );
    let without = probe_header_for(
        tmp.path(),
        &home,
        "msvc-no-libm",
        "[targets.probed.probes.named.HAVE_SQRT]\n\
         symbol = \"sqrt\"\nprelude = [\"math.h\"]",
    );

    assert!(
        without.contains("#define HAVE_SQRT 1"),
        "`sqrt` resolves from the CRT with nothing on the link line, so the \
         probe must answer yes:\n{without}"
    );
    assert!(
        with_libm.contains("/* #undef HAVE_SQRT */"),
        "the defect: adding the Unix library name makes the same question \
         answer no. If this assertion fails, the library-name mapping has \
         been fixed and this test should be deleted:\n{with_libm}"
    );

    // And the mapping is not wrong in general -- a `libs` entry whose name
    // already is the Windows one works. So this is a name-translation
    // problem, not a reason to redesign the link step.
    let ws2 = probe_header_for(
        tmp.path(),
        &home,
        "msvc-ws2",
        "[targets.probed.probes.named.HAVE_HTONL]\n\
         symbol = \"htonl\"\nprelude = [\"winsock2.h\"]\nlibs = [\"ws2_32\"]",
    );
    assert!(
        ws2.contains("#define HAVE_HTONL 1"),
        "`libs = [\"ws2_32\"]` -> `ws2_32.lib` is correct and must keep \
         working:\n{ws2}"
    );
}

// ---------------------------------------------------------------------------
// `type` and `constant` probe kinds.
//
// Appended at the end of the file, as everything else here is: a split of
// this module is deliberately deferred until the concurrent work settles,
// because interleaved appends conflict less than a restructure does.
//
// Every fixture below is chosen so the *right answer is known independently*
// and so that at least one answer in each kind is `no`. A probe subsystem
// whose fixtures all answer `yes` is indistinguishable from a constant, and
// the specific degradation each kind can suffer -- a `member` that stops
// reaching the snippet, a `constant` that becomes a compile-only symbol
// check -- is what these pin.
// ---------------------------------------------------------------------------

/// A fixture exercising both kinds, with a known `yes` and a known `no` in
/// each.
///
/// `struct timeval` exists on every hosted platform and
/// `struct harbour_no_such_struct` exists nowhere. `O_NONBLOCK` is defined
/// by `<fcntl.h>` everywhere POSIX; `HARBOUR_NO_SUCH_CONSTANT` is defined
/// nowhere. `poll` is a function, so it answers `yes` as a `symbol` and
/// `no` as a `constant`, which is the pair that keeps `constant` from
/// degrading into a compile-only symbol check.
#[cfg(not(windows))]
const KINDS_FIXTURE_MANIFEST: &str = r#"[package]
name = "kinds"
version = "0.1.0"

[targets.kinds]
kind = "exe"
sources = ["src/main.c"]

[targets.kinds.probes.named.HAVE_STRUCT_TIMEVAL]
type = "struct timeval"
prelude = ["sys/time.h", "time.h"]

[targets.kinds.probes.named.HAVE_NO_SUCH_STRUCT]
type = "struct harbour_no_such_struct"
prelude = ["stddef.h"]

[targets.kinds.probes.named.HAVE_TIMEVAL_TV_SEC]
type = "struct timeval"
member = "tv_sec"
prelude = ["sys/time.h", "time.h"]

[targets.kinds.probes.named.HAVE_TIMEVAL_NO_SUCH_FIELD]
type = "struct timeval"
member = "harbour_no_such_field"
prelude = ["sys/time.h", "time.h"]

[targets.kinds.probes.named.HAVE_FCNTL_O_NONBLOCK]
constant = "O_NONBLOCK"
prelude = ["fcntl.h"]

[targets.kinds.probes.named.HAVE_NO_SUCH_CONSTANT]
constant = "HARBOUR_NO_SUCH_CONSTANT"
prelude = ["stddef.h"]

[targets.kinds.probes.named.HAVE_POLL_AS_A_CONSTANT]
constant = "poll"
prelude = ["poll.h"]

[targets.kinds.probes.named.HAVE_POLL_AS_A_SYMBOL]
symbol = "poll"
prelude = ["poll.h"]
"#;

/// The consumer. Every claim is a *compile-time* assertion, so a wrong probe
/// answer is a build failure rather than a different line on stdout.
///
/// `HAVE_TIMEVAL_TV_SEC` and `HAVE_TIMEVAL_NO_SUCH_FIELD` are the pair that
/// matters: they are the same type with two different members, so they can
/// only differ if `member` reaches the generated snippet. If it stopped
/// doing so, both would answer `yes` and this file would not compile.
#[cfg(not(windows))]
const KINDS_FIXTURE_SOURCE: &str = r#"#include <stdio.h>

#ifndef HAVE_STRUCT_TIMEVAL
#error "a type that exists on every hosted platform answered no"
#endif
#ifdef HAVE_NO_SUCH_STRUCT
#error "a type that exists nowhere answered yes"
#endif
#ifndef HAVE_TIMEVAL_TV_SEC
#error "struct timeval has tv_sec; the member probe answered no"
#endif
#ifdef HAVE_TIMEVAL_NO_SUCH_FIELD
#error "struct timeval has no such field, so `member` is not reaching the snippet"
#endif
#ifndef HAVE_FCNTL_O_NONBLOCK
#error "O_NONBLOCK is defined by <fcntl.h> on every POSIX platform"
#endif
#ifdef HAVE_NO_SUCH_CONSTANT
#error "a constant that exists nowhere answered yes"
#endif
#ifdef HAVE_POLL_AS_A_CONSTANT
#error "poll is a function, not an integer constant: the constant kind has degraded to a compile-only symbol check"
#endif
#ifndef HAVE_POLL_AS_A_SYMBOL
#error "poll is a symbol that links on every POSIX platform"
#endif

int main(void) {
    printf("timeval=yes\n");
    printf("member-present=yes\n");
    printf("member-absent=no\n");
    printf("o_nonblock=yes\n");
    printf("poll-as-constant=no\n");
    return 0;
}
"#;

#[cfg(not(windows))]
fn write_kinds_fixture(dir: &std::path::Path) {
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(dir.join("Harbour.toml"), KINDS_FIXTURE_MANIFEST).unwrap();
    fs::write(dir.join("src/main.c"), KINDS_FIXTURE_SOURCE).unwrap();
}

/// Both kinds, answered by the real toolchain, with the answers reaching
/// the real compile command.
///
/// This is deliberately one test over one fixture rather than three, because
/// the interesting assertions are the *pairs* -- a type with and without a
/// member, a name asked as a constant and as a symbol -- and splitting them
/// would let one half pass while the other was never built.
#[test]
#[cfg(not(windows))]
fn the_type_and_constant_kinds_answer_correctly_and_reach_the_real_compile_command() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let (shim, records) = install_cc_recorder(tmp.path());
    let app = tmp.path().join("kinds");
    write_kinds_fixture(&app);

    // The `#error`s in the fixture mean a wrong answer is a build failure,
    // so `.success()` is itself an assertion about eight probe answers.
    harbour_run_env(&home, &app, &["build"], &[("CC", shim.to_str().unwrap())]).success();

    let probe_defines = recorded_probe_defines(&records);
    let names: Vec<String> = probe_defines
        .iter()
        .map(|d| {
            d.trim_start_matches("-D")
                .split('=')
                .next()
                .unwrap()
                .to_string()
        })
        .collect();

    // The true answers, in the named entries' manifest order. Every false
    // answer contributes *nothing* -- not `=0`, which `#ifdef` would accept.
    assert_eq!(
        names,
        vec![
            "HAVE_STRUCT_TIMEVAL",
            "HAVE_TIMEVAL_TV_SEC",
            "HAVE_FCNTL_O_NONBLOCK",
            "HAVE_POLL_AS_A_SYMBOL",
        ],
        "the defines the compiler actually received, in order; got \
         {probe_defines:?}"
    );

    // The built program is the independent witness: it reports what the
    // preprocessor saw, so the claim does not rest only on the argv capture.
    let out = run_built_exe(&app, "kinds");
    for line in [
        "timeval=yes",
        "member-present=yes",
        "member-absent=no",
        "o_nonblock=yes",
        "poll-as-constant=no",
    ] {
        assert!(
            out.out().contains(line),
            "the built program must report `{line}`: {}",
            out.out()
        );
    }
}

// Optional dependencies (issue #108)
//
// `optional = true` used to parse and change nothing: the dependency was
// resolved, fetched, built and linked regardless. The assertions below are
// deliberately about the *filesystem and the produced binary*, not about
// exit status or log lines, because "the build succeeded" is exactly what
// the broken behaviour also did:
//
//   - disabled: no archive for the dependency exists anywhere under the
//     target directory, the dependency's name appears in no compiler argv,
//     it is absent from `Harbour.lock`, and the binary takes the
//     without-it branch;
//   - a disabled *git* optional dependency points at a URL that cannot
//     resolve, so a build that succeeds proves nothing asked its source a
//     question -- and flipping the same manifest to activate it makes the
//     build fail, which is what rules out "git deps are just ignored";
//   - enabled: the archive exists, the argv carries it, and the binary
//     prints the number only the dependency can compute.
// ============================================================================

/// A dependency with a distinctive name (so a substring search over compiler
/// argv cannot collide with the consumer's own paths) exposing one function.
fn write_optional_dep(dir: &std::path::Path) {
    fs::create_dir_all(dir.join("include")).unwrap();
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join("Harbour.toml"),
        r#"[package]
name = "zzoptional"
version = "0.1.0"

[targets.zzoptional]
kind = "staticlib"
sources = ["src/lib.c"]

[targets.zzoptional.surface.compile.public]
include_dirs = ["include"]
"#,
    )
    .unwrap();
    fs::write(
        dir.join("include/zzoptional.h"),
        "#ifndef ZZOPTIONAL_H\n#define ZZOPTIONAL_H\nint zzoptional_answer(void);\n#endif\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/lib.c"),
        "#include \"zzoptional.h\"\nint zzoptional_answer(void) { return 42; }\n",
    )
    .unwrap();
}

/// The consumer. `withopt` activates the optional dependency through its
/// implicit feature, and the `when` block is what makes the *binary*
/// observably different rather than merely the link line.
fn write_optional_consumer(dir: &std::path::Path, default_features: &str) {
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join("Harbour.toml"),
        format!(
            r#"[package]
name = "optapp"
version = "0.1.0"

[dependencies]
zzoptional = {{ path = "../zzoptional", optional = true }}

[features]
default = [{default_features}]
withopt = ["zzoptional"]

[targets.optapp]
kind = "exe"
sources = ["src/main.c"]

[[targets.optapp.when]]
feature = "withopt"
defines = ["WITH_OPT=1"]
"#
        ),
    )
    .unwrap();
    fs::write(
        dir.join("src/main.c"),
        r#"#include <stdio.h>
#ifdef WITH_OPT
#include "zzoptional.h"
#endif

int main(void) {
#ifdef WITH_OPT
    printf("%d\n", zzoptional_answer());
#else
    printf("absent\n");
#endif
    return 0;
}
"#,
    )
    .unwrap();
}

/// Whether an archive for `name` exists anywhere under the target tree.
///
/// The counterpart of [`built_archive_path`], which panics when there is
/// none -- here *none* is the interesting answer.
fn archive_exists(dir: &std::path::Path, name: &str) -> bool {
    let wanted = [format!("lib{name}.a"), format!("{name}.lib")];
    snapshot_tree(&target_dir(dir)).into_keys().any(|p| {
        p.file_name()
            .map(|f| wanted.iter().any(|w| w.as_str() == f))
            .unwrap_or(false)
    })
}

/// The load-bearing test: disabled means not built, not linked, not in the
/// lockfile, and a different binary.
#[test]
fn test_optional_dependency_is_absent_from_the_build_until_a_feature_activates_it() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    write_optional_dep(&tmp.path().join("zzoptional"));

    let app = tmp.path().join("optapp");
    write_optional_consumer(&app, "");

    harbour_run(&home, &app, &["build"]).success();

    assert!(
        !archive_exists(&app, "zzoptional"),
        "nothing activated `zzoptional`, so it must not have been built.\n\
         build tree:\n{:#?}",
        snapshot_tree(&target_dir(&app))
            .into_keys()
            .collect::<Vec<_>>()
    );

    let lock = fs::read_to_string(app.join("Harbour.lock")).unwrap();
    assert!(
        !lock.contains("zzoptional"),
        "an inactive optional dependency must not be in the lockfile:\n{lock}"
    );

    let exe = built_exe_path(&app, "optapp");
    let out = Command::new(&exe).output().unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "absent",
        "with the feature off the binary must take the without-it branch"
    );

    // `--locked` is the honest way to assert the lockfile is not considered
    // stale: it refuses to run if resolution would change it. An optional
    // dependency that is absent from the graph must not make every build
    // look like it needs re-resolving.
    harbour_run(&home, &app, &["--locked", "build"]).success();

    // Now activate it, and everything flips.
    write_optional_consumer(&app, "\"withopt\"");
    harbour_run(&home, &app, &["build"]).success();

    assert!(
        archive_exists(&app, "zzoptional"),
        "`default = [\"withopt\"]` activates the implicit `zzoptional` feature, \
         so the dependency must now be built.\nbuild tree:\n{:#?}",
        snapshot_tree(&target_dir(&app))
            .into_keys()
            .collect::<Vec<_>>()
    );
    let lock = fs::read_to_string(app.join("Harbour.lock")).unwrap();
    assert!(
        lock.contains("zzoptional"),
        "an active optional dependency must be recorded in the lockfile:\n{lock}"
    );

    let out = Command::new(&exe).output().unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "42",
        "the binary must now call into the dependency"
    );

    harbour_run(&home, &app, &["--locked", "build"]).success();
}

/// The same thing asserted on the **real argv the compiler and linker were
/// handed**, which is the only place "built but not linked" and "linked but
/// not compiled against" can be told apart.
#[cfg(not(windows))]
#[test]
fn test_an_inactive_optional_dependency_reaches_no_compiler_argv() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    write_optional_dep(&tmp.path().join("zzoptional"));

    let app = tmp.path().join("optapp");
    write_optional_consumer(&app, "");

    let off_root = tmp.path().join("off");
    fs::create_dir_all(&off_root).unwrap();
    let (shim, off_records) = install_cc_recorder(&off_root);
    harbour_run_env(&home, &app, &["build"], &[("CC", shim.to_str().unwrap())]).success();

    let off: Vec<String> = recorded_argvs(&off_records).into_iter().flatten().collect();
    assert!(
        !off.is_empty(),
        "the recording shim captured nothing, so this test proves nothing"
    );
    assert!(
        !off.iter().any(|a| a.contains("zzoptional")),
        "an inactive optional dependency must not appear in any compile or link \
         argv -- not as an `-I`, not as an archive operand.\nargv:\n{off:#?}"
    );

    // Activate it and the same capture must now show it, on both sides: an
    // `-I` into its include directory, and its archive as a link operand.
    write_optional_consumer(&app, "\"withopt\"");
    let on_root = tmp.path().join("on");
    fs::create_dir_all(&on_root).unwrap();
    let (shim, on_records) = install_cc_recorder(&on_root);
    harbour_run_env(&home, &app, &["build"], &[("CC", shim.to_str().unwrap())]).success();

    let on = recorded_argvs(&on_records);
    let flat: Vec<String> = on.into_iter().flatten().collect();
    assert!(
        flat.iter()
            .any(|a| a.starts_with("-I") && a.contains("zzoptional")),
        "the consumer's compile must see the dependency's include dir.\nargv:\n{flat:#?}"
    );
    assert!(
        flat.iter()
            .any(|a| a.contains("zzoptional") && (a.ends_with(".a") || a.ends_with(".lib"))),
        "the dependency's archive must be a link operand.\nargv:\n{flat:#?}"
    );
}

/// An optional **git** dependency nobody activated is never cloned.
///
/// The URL cannot resolve, so "the build succeeded" is the assertion: if
/// anything queried the source, the build would fail. The second half rules
/// out the alternative explanation -- activating the same dependency must
/// make the build fail, proving the source really is unreachable and that
/// the first build's success came from pruning.
#[test]
fn test_an_inactive_optional_git_dependency_is_never_cloned() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let app = tmp.path().join("gitoptapp");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(app.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();
    let manifest = |default: &str| {
        format!(
            r#"[package]
name = "gitoptapp"
version = "0.1.0"

[dependencies]
zznotcloned = {{ git = "https://harbour.invalid/zznotcloned.git", optional = true }}

[features]
default = [{default}]

[targets.gitoptapp]
kind = "exe"
sources = ["src/main.c"]
"#
        )
    };

    fs::write(app.join("Harbour.toml"), manifest("")).unwrap();
    harbour_run(&home, &app, &["build"]).success();
    assert!(built_exe_path(&app, "gitoptapp").exists());

    // Nothing anywhere under the Harbour home may mention it: no clone, no
    // cache entry, no index file.
    let mentions: Vec<_> = snapshot_tree(&home)
        .into_keys()
        .filter(|p| p.to_string_lossy().contains("zznotcloned"))
        .collect();
    assert!(
        mentions.is_empty(),
        "an inactive optional git dependency left traces in the cache: {mentions:#?}"
    );

    // Activating it must fail, which is what proves the URL was never
    // reachable and the first build's success was the pruning.
    fs::write(app.join("Harbour.toml"), manifest("\"zznotcloned\"")).unwrap();
    let run = harbour_run(&home, &app, &["build"]);
    assert!(
        !run.status.success(),
        "activating an unreachable git dependency must fail; if this passes, the \
         dependency is being ignored rather than pruned\n{run}"
    );
}

/// Activation is unified across the graph, exactly as feature sets are.
///
/// `core` declares `extra` optional. `mid_on` asks for it, `mid_off` does
/// not, and both are in the same build -- so there is one `core` archive and
/// it must be the one with `extra`. Asserted through `core`'s own
/// `when feature = "extra"` define, which reaches the *binary*: a
/// per-dependent activation would give `core` no `HAVE_EXTRA` (the union
/// being computed per edge rather than per package) and the program would
/// print 0.
#[test]
fn test_optional_dependency_activation_is_unified_across_the_whole_graph() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    write_optional_dep(&tmp.path().join("zzoptional"));

    let core = tmp.path().join("core");
    fs::create_dir_all(core.join("include")).unwrap();
    fs::create_dir_all(core.join("src")).unwrap();
    fs::write(
        core.join("Harbour.toml"),
        r#"[package]
name = "core"
version = "0.1.0"

[dependencies]
zzoptional = { path = "../zzoptional", optional = true }

[targets.core]
kind = "staticlib"
sources = ["src/lib.c"]

[targets.core.surface.compile.public]
include_dirs = ["include"]

[[targets.core.when]]
feature = "zzoptional"
defines = ["HAVE_EXTRA=1"]
"#,
    )
    .unwrap();
    fs::write(
        core.join("include/core.h"),
        "#ifndef CORE_H\n#define CORE_H\nint core_extra(void);\n#endif\n",
    )
    .unwrap();
    fs::write(
        core.join("src/lib.c"),
        r#"#include "core.h"
#ifdef HAVE_EXTRA
#include "zzoptional.h"
#endif

int core_extra(void) {
#ifdef HAVE_EXTRA
    return zzoptional_answer();
#else
    return 0;
#endif
}
"#,
    )
    .unwrap();

    // Two intermediate libraries over the same `core`: one asks for the
    // optional dependency's implicit feature, the other says nothing.
    for (name, dep_line) in [
        (
            "mid_on",
            r#"core = { path = "../core", features = ["zzoptional"] }"#,
        ),
        ("mid_off", r#"core = { path = "../core" }"#),
    ] {
        let dir = tmp.path().join(name);
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::create_dir_all(dir.join("include")).unwrap();
        fs::write(
            dir.join("Harbour.toml"),
            format!(
                r#"[package]
name = "{name}"
version = "0.1.0"

[dependencies]
{dep_line}

[targets.{name}]
kind = "staticlib"
sources = ["src/lib.c"]

[targets.{name}.surface.compile.public]
include_dirs = ["include"]
"#
            ),
        )
        .unwrap();
        fs::write(
            dir.join(format!("include/{name}.h")),
            format!("int {name}_value(void);\n"),
        )
        .unwrap();
        fs::write(
            dir.join("src/lib.c"),
            format!(
                "#include \"core.h\"\n#include \"{name}.h\"\nint {name}_value(void) {{ return core_extra(); }}\n"
            ),
        )
        .unwrap();
    }

    let app = tmp.path().join("uniapp");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(
        app.join("Harbour.toml"),
        r#"[package]
name = "uniapp"
version = "0.1.0"

[dependencies]
mid_on = { path = "../mid_on" }
mid_off = { path = "../mid_off" }

[targets.uniapp]
kind = "exe"
sources = ["src/main.c"]
"#,
    )
    .unwrap();
    fs::write(
        app.join("src/main.c"),
        r#"#include <stdio.h>
#include "mid_on.h"
#include "mid_off.h"

int main(void) {
    printf("%d %d\n", mid_on_value(), mid_off_value());
    return 0;
}
"#,
    )
    .unwrap();

    harbour_run(&home, &app, &["build"]).success();

    // One `core` archive, one `zzoptional` archive -- a C graph links one
    // copy of each library, so a per-dependent activation would have to
    // produce two of one of them (or drop the dependency).
    assert!(archive_exists(&app, "zzoptional"));
    let out = Command::new(built_exe_path(&app, "uniapp"))
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "42 42",
        "`mid_off` never asked for the optional dependency, but there is only one \
         `core` in the link, so it gets the same one `mid_on` asked for"
    );
}

/// `[targets.X.deps]` naming an optional dependency that nothing activated
/// is an error, not a silent no-op.
#[test]
fn test_target_deps_naming_an_inactive_optional_dependency_is_an_error() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    write_optional_dep(&tmp.path().join("zzoptional"));

    let app = tmp.path().join("tdapp");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(app.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();
    fs::write(
        app.join("Harbour.toml"),
        r#"[package]
name = "tdapp"
version = "0.1.0"

[dependencies]
zzoptional = { path = "../zzoptional", optional = true }

[targets.tdapp]
kind = "exe"
sources = ["src/main.c"]

[targets.tdapp.deps.zzoptional]
link = "private"
"#,
    )
    .unwrap();

    let run = harbour_run(&home, &app, &["build"]);
    assert!(
        !run.status.success(),
        "naming an inactive optional dependency in `[targets.X.deps]` must fail \
         rather than be ignored\n{run}"
    );
    assert!(
        run.combined().contains("zzoptional"),
        "the diagnostic must name the dependency\n{run}"
    );
}

/// `dep:name` activates without defining a feature of that name, and the
/// suppression of the implicit feature that comes with it is visible from
/// the consumer.
#[test]
fn test_dep_colon_syntax_activates_an_optional_dependency() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    write_optional_dep(&tmp.path().join("zzoptional"));

    let app = tmp.path().join("depcolon");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(
        app.join("src/main.c"),
        "#include <stdio.h>\n#include \"zzoptional.h\"\n\
         int main(void) { printf(\"%d\\n\", zzoptional_answer()); return 0; }\n",
    )
    .unwrap();
    let manifest = r#"[package]
name = "depcolon"
version = "0.1.0"

[dependencies]
zzoptional = { path = "../zzoptional", optional = true }

[features]
default = ["tls"]
tls = ["dep:zzoptional"]

[targets.depcolon]
kind = "exe"
sources = ["src/main.c"]
"#;
    fs::write(app.join("Harbour.toml"), manifest).unwrap();

    harbour_run(&home, &app, &["build"]).success();
    assert!(archive_exists(&app, "zzoptional"));
    let out = Command::new(built_exe_path(&app, "depcolon"))
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "42");

    // ... and because some feature said `dep:zzoptional`, the dependency's
    // own name is no longer a feature: asking for it is an error rather
    // than a second way to switch it on.
    fs::write(
        app.join("Harbour.toml"),
        manifest.replace(r#"default = ["tls"]"#, r#"default = ["zzoptional"]"#),
    )
    .unwrap();
    let run = harbour_run(&home, &app, &["build"]);
    assert!(
        !run.status.success(),
        "`dep:` suppresses the implicit feature, so `zzoptional` must not be a \
         feature name here\n{run}"
    );
    assert!(
        run.combined().contains("unknown feature"),
        "and the diagnostic must say so\n{run}"
    );
}

// ============================================================================
// Named profiles (issue #106)
//
// `[profile.NAME]` parsed for any name and only `debug`/`release` could ever
// be selected, because there was no `--profile` flag. The assertions here are
// on the **real argv the compiler was handed** and on where the artifact
// landed, because a named profile whose settings silently did not apply is
// an ordinary build that reports `Finished asan`.
// ============================================================================

/// A consumer with a named profile inheriting from `release`, plus a
/// path dependency, so "whose profile wins" is observable.
fn write_profile_project(root: &std::path::Path) {
    let dep = root.join("profdep");
    fs::create_dir_all(dep.join("include")).unwrap();
    fs::create_dir_all(dep.join("src")).unwrap();
    fs::write(
        dep.join("Harbour.toml"),
        r#"[package]
name = "profdep"
version = "0.1.0"

# Ignored: profiles come from the package being built, never from a
# dependency. A define here reaching the compile line is the defect.
[profile.debug]
cflags = ["-DDEP_PROFILE_LEAKED=1"]

[profile.zzdepprof]
inherits = "release"
cflags = ["-DDEP_NAMED_PROFILE_LEAKED=1"]

[targets.profdep]
kind = "staticlib"
sources = ["src/lib.c"]

[targets.profdep.surface.compile.public]
include_dirs = ["include"]
"#,
    )
    .unwrap();
    fs::write(
        dep.join("include/profdep.h"),
        "#ifndef PROFDEP_H\n#define PROFDEP_H\nint profdep_value(void);\n#endif\n",
    )
    .unwrap();
    fs::write(
        dep.join("src/lib.c"),
        "#include \"profdep.h\"\nint profdep_value(void) { return 7; }\n",
    )
    .unwrap();

    let app = root.join("profapp");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(
        app.join("Harbour.toml"),
        r#"[package]
name = "profapp"
version = "0.1.0"

[dependencies]
profdep = { path = "../profdep" }

[profile.release]
cflags = ["-DFROM_RELEASE=1"]

[profile.asan]
inherits = "release"
opt_level = "1"
cflags = ["-DFROM_ASAN=1"]

[targets.profapp]
kind = "exe"
sources = ["src/main.c"]
"#,
    )
    .unwrap();
    fs::write(
        app.join("src/main.c"),
        r#"#include <stdio.h>
#include "profdep.h"

int main(void) {
#ifdef DEP_PROFILE_LEAKED
    printf("dep-profile-leaked\n");
    return 0;
#endif
#ifdef DEP_NAMED_PROFILE_LEAKED
    printf("dep-named-profile-leaked\n");
    return 0;
#endif
    printf("%d\n", profdep_value());
    return 0;
}
"#,
    )
    .unwrap();
}

/// `--profile asan` selects the named profile: its own keys, its
/// `inherits` ancestor's keys, and its own output directory.
#[cfg(not(windows))]
#[test]
fn test_named_profile_reaches_the_compiler_and_its_own_output_directory() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    write_profile_project(tmp.path());
    let app = tmp.path().join("profapp");

    let asan_root = tmp.path().join("asan-rec");
    fs::create_dir_all(&asan_root).unwrap();
    let (shim, records) = install_cc_recorder(&asan_root);
    harbour_run_env(
        &home,
        &app,
        &["build", "--profile", "asan"],
        &[("CC", shim.to_str().unwrap())],
    )
    .success();

    // The artifact lands under the profile's own name, so two profiles never
    // share a fingerprint cache.
    assert!(
        built_exe_path_in(&app, "asan", "profapp").exists(),
        "`--profile asan` must build into the `asan` output directory.\n{:#?}",
        snapshot_tree(&target_dir(&app))
            .into_keys()
            .collect::<Vec<_>>()
    );

    let argv: Vec<String> = recorded_argvs(&records).into_iter().flatten().collect();
    assert!(!argv.is_empty(), "the shim recorded nothing");
    assert!(
        argv.iter().any(|a| a == "-DFROM_ASAN=1"),
        "the named profile's own `cflags` must reach the compiler.\nargv:\n{argv:#?}"
    );
    assert!(
        argv.iter().any(|a| a == "-DFROM_RELEASE=1"),
        "`inherits = \"release\"` must bring `[profile.release]`'s `cflags` \
         with it.\nargv:\n{argv:#?}"
    );
    assert!(
        argv.iter().any(|a| a == "-O1"),
        "the named profile's own `opt_level` must win over its ancestor's \
         `3`.\nargv:\n{argv:#?}"
    );
    assert!(
        !argv.iter().any(|a| a == "-O3"),
        "`opt_level = \"1\"` must replace the inherited `3`, not sit beside \
         it.\nargv:\n{argv:#?}"
    );

    // Whose profile wins: the root's. A dependency's `[profile.*]` -- named
    // or not -- must not reach any compile line, and the binary says so.
    assert!(
        !argv
            .iter()
            .any(|a| a.contains("DEP_PROFILE_LEAKED") || a.contains("DEP_NAMED_PROFILE_LEAKED")),
        "a dependency's profile must not reach the build.\nargv:\n{argv:#?}"
    );
    let out = Command::new(built_exe_path_in(&app, "asan", "profapp"))
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "7");

    // `--release` is a different profile in a different directory, and the
    // named profile's own flags must not follow it there.
    let rel_root = tmp.path().join("rel-rec");
    fs::create_dir_all(&rel_root).unwrap();
    let (shim, records) = install_cc_recorder(&rel_root);
    harbour_run_env(
        &home,
        &app,
        &["build", "--release"],
        &[("CC", shim.to_str().unwrap())],
    )
    .success();
    assert!(built_exe_path_in(&app, "release", "profapp").exists());
    let argv: Vec<String> = recorded_argvs(&records).into_iter().flatten().collect();
    assert!(
        argv.iter().any(|a| a == "-DFROM_RELEASE=1") && argv.iter().any(|a| a == "-O3"),
        "argv:\n{argv:#?}"
    );
    assert!(
        !argv.iter().any(|a| a == "-DFROM_ASAN=1"),
        "`[profile.asan]`'s own keys must not apply to `release`.\nargv:\n{argv:#?}"
    );
}

/// `--release` and `--profile` are the same setting, so asking for both is
/// refused rather than silently resolved by precedence.
#[test]
fn test_release_and_profile_cannot_both_be_given() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    write_profile_project(tmp.path());
    let app = tmp.path().join("profapp");

    let run = harbour_run(&home, &app, &["build", "--release", "--profile", "asan"]);
    assert!(
        !run.status.success(),
        "`--release` is `--profile release`; both at once must be an error\n{run}"
    );
    // Named, so this cannot pass merely because `--profile` is unrecognised.
    let message = run.combined();
    assert!(
        message.contains("cannot be used with") && message.contains("--profile"),
        "the error must be the conflict, not an unknown flag\n{run}"
    );

    // ... and each on its own is accepted.
    harbour_run(&home, &app, &["build", "--profile", "asan"]).success();
    harbour_run(&home, &app, &["build", "--release"]).success();
}

/// A profile name nothing declares is an error listing what exists, not a
/// silent debug build under a directory named after the typo.
#[test]
fn test_an_unknown_profile_name_is_an_error_listing_what_exists() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    write_profile_project(tmp.path());
    let app = tmp.path().join("profapp");

    let run = harbour_run(&home, &app, &["build", "--profile", "asam"]);
    assert!(
        !run.status.success(),
        "an undeclared profile must fail rather than build something\n{run}"
    );
    let message = run.combined();
    assert!(
        message.contains("no profile named `asam`"),
        "the diagnostic must name what was asked for\n{message}"
    );
    assert!(
        message.contains("`asan`") && message.contains("`release`"),
        "and list the profiles that exist\n{message}"
    );
    assert!(
        !built_exe_path_in(&app, "asam", "profapp").exists(),
        "nothing must have been built under the typo's name"
    );
}

/// A named profile must declare `inherits`, and the build says so rather
/// than guessing a base.
#[test]
fn test_a_named_profile_without_inherits_fails_the_build() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let app = tmp.path().join("noinherit");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(app.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();
    fs::write(
        app.join("Harbour.toml"),
        r#"[package]
name = "noinherit"
version = "0.1.0"

[profile.asan]
opt_level = "1"
sanitizers = ["address"]

[targets.noinherit]
kind = "exe"
sources = ["src/main.c"]
"#,
    )
    .unwrap();

    let run = harbour_run(&home, &app, &["build"]);
    assert!(!run.status.success(), "{run}");
    assert!(
        run.combined().contains("must set `inherits`"),
        "the diagnostic must say what is missing\n{run}"
    );
}

/// `optional` in `[workspace.dependencies]` fails the build, naming the
/// entry and where the key belongs instead.
///
/// Optionality is not a property of the dependency; it is a property of the
/// relationship between one package and it, and it only means anything
/// alongside that package's `[features]` table -- which is the member's.
/// Inheriting it would have given one field two readers that disagree:
/// `resolve_dependency` has workspace context and would prune the
/// dependency, while `surface_resolver::optional_dependency_names` reads the
/// member's raw spec, where `{ workspace = true }` says nothing about
/// optionality -- so the member's `[features]` could not switch back on the
/// dependency the workspace had made optional.
#[test]
fn test_optional_in_workspace_dependencies_is_refused() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let root = tmp.path().join("wsopt");
    fs::create_dir_all(root.join("member/src")).unwrap();
    fs::write(
        root.join("member/src/main.c"),
        "int main(void) { return 0; }\n",
    )
    .unwrap();
    fs::write(
        root.join("member/Harbour.toml"),
        r#"[package]
name = "member"
version = "0.1.0"

[targets.member]
kind = "exe"
sources = ["src/main.c"]
"#,
    )
    .unwrap();
    fs::write(
        root.join("Harbour.toml"),
        r#"[workspace]
members = ["member"]

[workspace.dependencies]
zzws = { version = "1.0", optional = true }
"#,
    )
    .unwrap();

    let run = harbour_run(&home, &root, &["build"]);
    assert!(
        !run.status.success(),
        "`optional` in `[workspace.dependencies]` must fail the build\n{run}"
    );
    let message = run.combined();
    assert!(
        message.contains("`optional` cannot be set here"),
        "the diagnostic must say the key is in the wrong table\n{run}"
    );
    assert!(message.contains("zzws"), "and name the entry\n{run}");
    assert!(
        message.contains("workspace = true, optional = true"),
        "and show where it goes instead\n{run}"
    );

    // Without the key, the same workspace builds.
    fs::write(
        root.join("Harbour.toml"),
        r#"[workspace]
members = ["member"]

[workspace.dependencies]
zzws = { version = "1.0" }
"#,
    )
    .unwrap();
    harbour_run(&home, &root, &["build"]).success();
}

// ============================================================================
// `workspace = true` with a `path` (#133)
//
// Three compounding bugs, all of which had to go for the feature to have any
// working spelling at all:
//
// 1. the workspace-level `path` was anchored at the *member* directory, so
//    `path = "vendored"` in the root's manifest meant `<root>/app/vendored`;
// 2. inheritance then failed anyway, because `Package::summary` went through
//    a second, context-free route from `DependencySpec` to `Dependency`
//    that had never heard of `[workspace.dependencies]`;
// 3. and running from inside the member did not find the parent workspace,
//    so the table one directory up was invisible rather than mis-anchored.
//
// These two tests run the produced binary and assert what it prints. That is
// the only thing that distinguishes "resolved, compiled, archived and
// linked" from "the build reported success": a fix that resolved the
// dependency without linking it would still exit zero and produce an `app`
// that does not run.
// ============================================================================

/// Write the #133 fixture: a virtual workspace whose only member inherits a
/// path dependency on a package that is *not* itself a member.
///
/// `vendored` is deliberately outside `members`, because when it is a member
/// the local-first rule matches it first and the workspace entry is never
/// consulted -- a different case, now refused outright (see
/// `validate_workspace_deps_do_not_name_members`).
fn write_workspace_path_inheritance_fixture(root: &Path) {
    fs::create_dir_all(root.join("app").join("src")).unwrap();
    fs::create_dir_all(root.join("vendored").join("src")).unwrap();

    fs::write(
        root.join("Harbour.toml"),
        r#"[workspace]
members = ["app"]

[workspace.dependencies]
vendored = { path = "vendored" }
"#,
    )
    .unwrap();

    fs::write(
        root.join("app").join("Harbour.toml"),
        r#"[package]
name = "app"
version = "0.1.0"

[dependencies]
vendored = { workspace = true }

[targets.app]
kind = "bin"
sources = ["src/**/*.c"]

[targets.app.deps]
vendored = "vendored"
"#,
    )
    .unwrap();

    fs::write(
        root.join("vendored").join("Harbour.toml"),
        r#"[package]
name = "vendored"
version = "0.1.0"

[targets.vendored]
kind = "staticlib"
sources = ["src/**/*.c"]
"#,
    )
    .unwrap();

    fs::write(
        root.join("vendored").join("src").join("v.c"),
        "int vendored_value(void) { return 133; }\n",
    )
    .unwrap();

    fs::write(
        root.join("app").join("src").join("main.c"),
        "#include <stdio.h>\nint vendored_value(void);\n\
         int main(void) { printf(\"%d\\n\", vendored_value()); return 0; }\n",
    )
    .unwrap();
}

/// Bugs 1 and 2, from the workspace root.
///
/// `path = "vendored"` is the only spelling that makes sense from the
/// manifest it is written in, and it is the spelling that used to fail with
/// "path does not exist: .../app/vendored". The one that got *past* that
/// check, `path = "../vendored"`, then hit "dependency `vendored` must
/// specify `path`, `git`, `registry`, `vcpkg`, or `version`" -- so there was
/// no working spelling at all.
#[test]
fn test_workspace_inherited_path_dependency_links_and_runs() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let root = tmp.path().join("ws");
    write_workspace_path_inheritance_fixture(&root);

    build_ok(&home, &root);

    // The archive has to exist on disk; a resolve that succeeded without
    // building the dependency would leave the link to fail later, or
    // silently drop it.
    let lib = target_dir(&root)
        .join("debug")
        .join("deps")
        .join("vendored-0.1.0")
        .join("lib");
    assert!(
        lib.exists(),
        "expected the inherited dependency's archive under {}, but the \
         directory is not there",
        lib.display()
    );

    let run = run_built_exe(&root, "app");
    assert_eq!(
        run.out(),
        "133",
        "the inherited dependency must actually be linked in, not merely \
         resolved\n{run}"
    );
}

/// Bug 3: the same fixture, built from inside the member directory.
///
/// Cargo walks up to the workspace root; Harbour did not, so
/// `[workspace.dependencies]` was invisible and the error blamed the
/// member's manifest for something declared one directory up.
///
/// The second assertion pins the mechanism rather than the outcome: the
/// artifacts must land in the *workspace root's* target directory, because
/// that is what proves the workspace was found, rather than the member
/// being built as a standalone project that happened to resolve. Paths are
/// built with `Path::join`, never with embedded separators.
#[test]
fn test_workspace_inherited_path_dependency_resolves_from_a_member_directory() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let root = tmp.path().join("ws");
    write_workspace_path_inheritance_fixture(&root);

    let app = root.join("app");
    let run = harbour_run(&home, &app, &["build"]).success();

    assert!(
        !app.join(".harbour").exists(),
        "building from a member must use the workspace root's target \
         directory, not create one inside the member\n{run}"
    );

    let exe = run_built_exe(&root, "app");
    assert_eq!(
        exe.out(),
        "133",
        "a build started from the member directory must produce the same \
         linked binary as one started from the root\n{exe}"
    );
}

// ============================================================================
// What a generator is told about its target (#136)
//
// A `prebuild` generator used to receive only the `env` its own block
// declared: no `HARBOUR_*`, no triple, no toolchain. That is not a
// convenience gap. openssl's x86_64 perlasm scripts shell out to `$ENV{CC}`
// to ask the assembler which encodings it accepts, and with `CC` unset
// `sha512-x86_64.pl` emits 49,912 bytes instead of 97,936 -- dropping the
// AVX2 and SHA-extension paths, while still assembling, linking and
// computing correct digests. Measured, and asserted for real in
// `ci/canary/openssl/run.sh`; what is asserted here is the environment
// itself, observed from inside a generator Harbour actually ran.
// ============================================================================

/// Write a generator that dumps its entire environment to
/// `generated/env.txt`, and return the `prebuild` block that runs it.
///
/// The whole environment rather than the variables under test: a dump cannot
/// be written to agree with the assertions, and a variable that is missing
/// shows up as missing rather than as an empty string.
fn write_env_dumping_generator(dir: &std::path::Path, extra_toml: &str) -> String {
    if cfg!(windows) {
        fs::write(
            dir.join("dumpenv.cmd"),
            "@echo off\r\n\
             if not exist generated mkdir generated\r\n\
             set > generated\\env.txt\r\n",
        )
        .unwrap();
        format!(
            "[[targets.app.prebuild]]\n\
             program = \"cmd\"\n\
             args = [\"/C\", \"dumpenv.cmd\"]\n\
             outputs = [\"generated/env.txt\"]\n\
             {extra_toml}"
        )
    } else {
        fs::write(
            dir.join("dumpenv.sh"),
            "#!/bin/sh\n\
             mkdir -p generated\n\
             env > generated/env.txt\n",
        )
        .unwrap();
        format!(
            "[[targets.app.prebuild]]\n\
             program = \"sh\"\n\
             args = [\"dumpenv.sh\"]\n\
             outputs = [\"generated/env.txt\"]\n\
             {extra_toml}"
        )
    }
}

/// Look one variable up in a `KEY=VALUE` environment dump.
///
/// Returns `None` for a variable that is absent, which is a different answer
/// from one that is present and empty -- `HARBOUR_TARGET_OS` is legitimately
/// empty on a bare-metal target, and `HARBOUR_TARGET_ENV` is legitimately
/// absent on a triple with no environment component.
fn env_dump_get(dump: &str, key: &str) -> Option<String> {
    dump.lines().find_map(|line| {
        let (k, v) = line.split_once('=')?;
        (k == key).then(|| v.to_string())
    })
}

/// Lay out an `app` package whose only `prebuild` step dumps its environment.
fn write_env_dumping_app(app_dir: &std::path::Path, extra_toml: &str) {
    let prebuild = write_env_dumping_generator(app_dir, extra_toml);
    fs::write(
        app_dir.join("Harbour.toml"),
        format!(
            "[package]\n\
             name = \"app\"\n\
             version = \"0.1.0\"\n\
             \n\
             [targets.app]\n\
             kind = \"bin\"\n\
             sources = [\"src/**/*.c\"]\n\
             \n\
             {prebuild}"
        ),
    )
    .unwrap();
    fs::write(app_dir.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();
}

/// The architecture token a triple uses for this host, as an independent
/// oracle for `HARBOUR_TARGET_ARCH`.
///
/// Rust's `ARCH` agrees with the triple's first component on every platform
/// this project builds on, with one exception: 32-bit x86 is `x86` to Rust
/// and `i386`..`i686` in a triple, so that case accepts the family.
fn host_arch_tokens() -> Vec<&'static str> {
    match std::env::consts::ARCH {
        "x86" => vec!["x86", "i386", "i486", "i586", "i686"],
        other => vec![other],
    }
}

#[test]
fn prebuild_generator_is_told_about_its_target_and_toolchain() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");
    write_env_dumping_app(&app_dir, "");

    build_ok(&home, &app_dir);
    let dump = fs::read_to_string(app_dir.join("generated/env.txt")).unwrap();
    let get = |k: &str| {
        env_dump_get(&dump, k)
            .unwrap_or_else(|| panic!("`{k}` was not in the generator's environment:\n{dump}"))
    };

    // Parity with `recipe`: where the sources are, and where the artifacts
    // are expected.
    let package_root = PathBuf::from(get("HARBOUR_PACKAGE_ROOT"));
    assert_eq!(
        package_root.canonicalize().unwrap(),
        app_dir.canonicalize().unwrap(),
        "HARBOUR_PACKAGE_ROOT must be the declaring package's root\n{dump}"
    );
    let artifact_dir = get("HARBOUR_ARTIFACT_DIR");
    assert!(
        artifact_dir.contains(".harbour") && artifact_dir.ends_with("lib"),
        "HARBOUR_ARTIFACT_DIR must be this target's lib directory, got \
         `{artifact_dir}`\n{dump}"
    );

    // What platform this is. `HARBOUR_TARGET_OS` is the value a `when` block
    // matches, so it is `macos` and never `darwin` -- checked against Rust's
    // own name for the host, which uses the same spelling.
    assert_eq!(
        get("HARBOUR_TARGET_OS"),
        std::env::consts::OS,
        "HARBOUR_TARGET_OS must be the normalized name a `when` condition \
         matches\n{dump}"
    );
    let arch = get("HARBOUR_TARGET_ARCH");
    assert!(
        host_arch_tokens().contains(&arch.as_str()),
        "HARBOUR_TARGET_ARCH `{arch}` is not this host's architecture \
         ({:?})\n{dump}",
        host_arch_tokens()
    );

    // A native build: these two invariants need no oracle at all, and they
    // are the ones a cross build inverts.
    assert_eq!(
        get("HARBOUR_TARGET_TRIPLE"),
        get("HARBOUR_HOST_TRIPLE"),
        "a native build must report the same triple twice\n{dump}"
    );
    assert_eq!(
        get("HARBOUR_CROSS_COMPILING"),
        "0",
        "a native build is not a cross build\n{dump}"
    );

    // The toolchain. This is the group that turns openssl's `CC = "cc"`
    // guess into a fact, so it is not enough for the variables to exist:
    // each must name a tool that is really on this machine.
    for var in ["CC", "CXX", "AR"] {
        let tool = get(var);
        assert!(
            !tool.is_empty(),
            "{var} must name the tool Harbour resolved for the target\n{dump}"
        );
        assert!(
            std::path::Path::new(&tool).exists(),
            "{var}=`{tool}` does not exist, so it is a guess rather than the \
             resolved toolchain\n{dump}"
        );
    }
}

#[test]
fn prebuild_manifest_env_overrides_harbours_own() {
    // The contract must stay overridable, exactly as it already was for a
    // `recipe`: a package that knows better -- or is working around a
    // generator that mis-parses a path -- keeps the last word.
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");
    write_env_dumping_app(
        &app_dir,
        "env = { CC = \"manifest-said-so\", HARBOUR_TARGET_ARCH = \"z80\" }\n",
    );

    build_ok(&home, &app_dir);
    let dump = fs::read_to_string(app_dir.join("generated/env.txt")).unwrap();
    assert_eq!(
        env_dump_get(&dump, "CC").as_deref(),
        Some("manifest-said-so"),
        "the manifest's `env` must win over Harbour's `CC`\n{dump}"
    );
    assert_eq!(
        env_dump_get(&dump, "HARBOUR_TARGET_ARCH").as_deref(),
        Some("z80"),
        "and over the target description too\n{dump}"
    );
}

// ============================================================================
// A probe answer reaching a generator (#135)
//
// openssl's generated config has exactly one genuine measurement of the
// target in it -- `sizeof(long)`, which decides `SIXTY_FOUR_BIT_LONG` vs
// `THIRTY_TWO_BIT` -- and before this it had to be expressed by *enumerating*
// 32-bit architectures in `when` blocks, because a generator could not see a
// probe answer. perl could have measured it, and would have measured the
// host: wrong in exactly the case that matters.
// ============================================================================

/// Lay out an `app` package whose generator writes a C source *from* a probe
/// answer, and whose `main` then checks that answer against what the
/// compiler really thinks.
///
/// The self-check is the point. A test that asserted `8` would pass on a
/// machine where the probe was wrong and the compiler agreed with it by
/// accident; comparing the generated value with `sizeof(long)` in the same
/// translation unit cannot.
fn write_probe_consuming_app(app_dir: &std::path::Path) {
    if cfg!(windows) {
        fs::write(
            app_dir.join("gen.cmd"),
            "@echo off\r\n\
             if not exist generated mkdir generated\r\n\
             >generated\\table.h echo extern int probed_sizeof_long;\r\n\
             >generated\\table.c echo int probed_sizeof_long = %HARBOUR_PROBE_SIZEOF_LONG%;\r\n\
             set > generated\\env.txt\r\n",
        )
        .unwrap();
    } else {
        fs::write(
            app_dir.join("gen.sh"),
            "#!/bin/sh\n\
             mkdir -p generated\n\
             echo 'extern int probed_sizeof_long;' > generated/table.h\n\
             echo \"int probed_sizeof_long = $HARBOUR_PROBE_SIZEOF_LONG;\" > generated/table.c\n\
             env > generated/env.txt\n",
        )
        .unwrap();
    }

    let prebuild = if cfg!(windows) {
        "[[targets.app.prebuild]]\n\
         program = \"cmd\"\n\
         args = [\"/C\", \"gen.cmd\"]\n\
         outputs = [\"generated/table.c\", \"generated/table.h\", \"generated/env.txt\"]\n"
    } else {
        "[[targets.app.prebuild]]\n\
         program = \"sh\"\n\
         args = [\"gen.sh\"]\n\
         outputs = [\"generated/table.c\", \"generated/table.h\", \"generated/env.txt\"]\n"
    };

    fs::write(
        app_dir.join("Harbour.toml"),
        format!(
            "[package]\n\
             name = \"app\"\n\
             version = \"0.1.0\"\n\
             \n\
             [targets.app]\n\
             kind = \"bin\"\n\
             sources = [\"src/**/*.c\", \"generated/*.c\"]\n\
             \n\
             [targets.app.private]\n\
             include_dirs = [\"generated\"]\n\
             \n\
             [targets.app.probes]\n\
             check_sizeof = [\"long\"]\n\
             check_headers = [\"stdio.h\"]\n\
             \n\
             [targets.app.probes.named.HAVE_HARBOUR_NO_SUCH_HEADER]\n\
             header = \"harbour_no_such_header_42.h\"\n\
             \n\
             {prebuild}"
        ),
    )
    .unwrap();

    fs::write(
        app_dir.join("src/main.c"),
        "#include <stdio.h>\n\
         #include \"table.h\"\n\
         \n\
         int main(void) {\n\
         \tprintf(\"%d %d\\n\", probed_sizeof_long, (int)sizeof(long));\n\
         \treturn probed_sizeof_long == (int)sizeof(long) ? 0 : 1;\n\
         }\n",
    )
    .unwrap();
}

#[test]
fn prebuild_generator_receives_probe_answers() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");
    write_probe_consuming_app(&app_dir);

    build_ok(&home, &app_dir);

    // The end-to-end claim: a measurement made by compiling for the target
    // reached a generator, became a translation unit, and agrees with the
    // compiler that built it.
    let run = run_built_exe(&app_dir, "app");
    let out = run.out().to_string();
    let (generated, actual) = out.split_once(' ').unwrap_or(("", ""));
    assert_eq!(
        generated, actual,
        "the generator wrote `{generated}` from HARBOUR_PROBE_SIZEOF_LONG, \
         but the compiler says sizeof(long) is `{actual}`"
    );
    assert!(
        !generated.is_empty(),
        "the generator produced no value at all; output was `{out}`"
    );

    // And the shape of the variables, read out of the generator's own
    // environment dump.
    let dump = fs::read_to_string(app_dir.join("generated/env.txt")).unwrap();
    assert_eq!(
        env_dump_get(&dump, "HARBOUR_PROBE_SIZEOF_LONG").as_deref(),
        Some(actual),
        "a `sizeof` answer arrives as the number\n{dump}"
    );
    assert_eq!(
        env_dump_get(&dump, "HARBOUR_PROBE_HAVE_STDIO_H").as_deref(),
        Some("1"),
        "a header that exists answers 1\n{dump}"
    );
    // A false answer is `0`, *not* an absent variable -- unlike the define,
    // which is left undefined so that `#ifdef` works. An environment has no
    // `#ifdef`, and an absent variable would be indistinguishable from a
    // misspelled one.
    assert_eq!(
        env_dump_get(&dump, "HARBOUR_PROBE_HAVE_HARBOUR_NO_SUCH_HEADER").as_deref(),
        Some("0"),
        "a header that does not exist must answer `0` rather than vanish: a \
         generator cannot tell an absent variable from a typo\n{dump}"
    );
}

// ============================================================================
// `arch` is the literal triple component (#138)
//
// The same Debian armhf compiler is `arch = "arm"` through one triple and
// `arch = "armv7"` through another, and a manifest keyed on one silently
// does not apply to the other -- with no warning, because an unmatched
// `when` block is normal and expected. openssl caught that one step before
// it produced a 64-bit `bn_conf.h` on a 32-bit target.
// ============================================================================

/// A spelling in the *same* ISA family as this host's architecture, but not
/// the same architecture.
///
/// Panics rather than skipping on an architecture it has no entry for: a
/// test that quietly does nothing on a new platform is a test that stopped
/// checking, and the fix is one line here.
fn sibling_arch_spelling() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "aarch64_be",
        "x86_64" => "x86_64h",
        "x86" => "i686",
        "arm" => "armv7",
        "powerpc64" => "powerpc64le",
        other => panic!(
            "no sibling architecture spelling recorded for `{other}`; add one \
             to `sibling_arch_spelling` (it must be a different spelling in \
             the same ISA family)"
        ),
    }
}

#[test]
fn a_when_block_for_a_sibling_arch_spelling_is_reported() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");

    let sibling = sibling_arch_spelling();
    fs::write(
        app_dir.join("Harbour.toml"),
        format!(
            "[package]\n\
             name = \"app\"\n\
             version = \"0.1.0\"\n\
             \n\
             [targets.app]\n\
             kind = \"bin\"\n\
             sources = [\"src/**/*.c\"]\n\
             \n\
             [[targets.app.when]]\n\
             arch = \"{sibling}\"\n\
             defines = [\"NEVER_MATCHES\"]\n"
        ),
    )
    .unwrap();
    fs::write(app_dir.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();

    // The build must still succeed: this is an advisory, not an error. A
    // package legitimately may have no block for the current architecture.
    let log = build_ok(&home, &app_dir);
    let text = log.combined();
    assert!(
        text.contains("same architecture family spelled differently"),
        "a `when` block naming a sibling spelling of this architecture \
         ({sibling} vs {}) must be reported, since it contributes nothing \
         and looks like it was meant to\n{log}",
        std::env::consts::ARCH
    );
    assert!(
        text.contains(sibling),
        "the advisory must name the spelling that did not match\n{log}"
    );
}

#[test]
fn a_when_block_for_an_unrelated_arch_is_not_reported() {
    // The normal case, and the reason the advisory is narrow. openssl has
    // aarch64 and x86_64 assembly and nothing for 32-bit ARM; that build is
    // *supposed* to match nothing and compile the portable baseline. A
    // warning there would fire on every correct manifest and be tuned out,
    // which is how a real warning stops being read.
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");

    // `mips` is in no family this project's CI hosts belong to.
    fs::write(
        app_dir.join("Harbour.toml"),
        "[package]\n\
         name = \"app\"\n\
         version = \"0.1.0\"\n\
         \n\
         [targets.app]\n\
         kind = \"bin\"\n\
         sources = [\"src/**/*.c\"]\n\
         \n\
         [[targets.app.when]]\n\
         arch = \"mips\"\n\
         defines = [\"NEVER_MATCHES\"]\n",
    )
    .unwrap();
    fs::write(app_dir.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();

    let log = build_ok(&home, &app_dir);
    assert!(
        !log.combined().contains("same architecture family"),
        "an unrelated architecture block must stay silent\n{log}"
    );
}
