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
use std::path::PathBuf;
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
    let mut diff = TreeDiff::default();
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

    /// Assert the build produced no new or rewritten artifact at all.
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
             {redone:?} and removed {gone:?}: {why}\n{self}"
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

    harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .success();

    let exe = built_exe_path(&app_dir, "app");
    let out = Command::new(&exe).output().unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "7");
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
