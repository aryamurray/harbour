//! Helpers shared by more than one area of the CLI integration tests.
#![allow(unused_imports, dead_code)]

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
pub(crate) fn harbour(home: &std::path::Path) -> Command {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("harbour"));
    // What is redirected and cleared (and what is deliberately left alone) is
    // defined once, in `tests/common/mod.rs`.
    crate::shared::isolate(&mut cmd, home);
    cmd
}

/// Create a temporary directory for test projects.
pub(crate) fn temp_dir() -> TempDir {
    TempDir::new().unwrap()
}

/// Derive an isolated "home" directory from a test's temp dir, used to keep
/// `harbour`'s global cache/config out of the developer's real home
/// directory (see [`harbour`]).
pub(crate) fn harbour_home(tmp: &TempDir) -> PathBuf {
    let home = tmp.path().join(".harbour-home");
    fs::create_dir_all(&home).unwrap();
    home
}

/// A finished process, with **both** output streams retained.
///
/// Keeping only one stream is what made the first Windows diagnosis useless.
/// Harbour spreads its build narration across both: the per-file decisions
/// (`Compiling N file(s)`, `All N file(s) up to date`) are `tracing` records
/// on stderr, `--message-format json` writes to stdout, and a compiler's own
/// diagnostics can land on either. Assertion helpers below always render the
/// whole thing.
#[derive(Debug)]
pub(crate) struct RunLog {
    pub(crate) what: String,
    pub(crate) status: std::process::ExitStatus,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

impl RunLog {
    /// Run `cmd` to completion, labelling it `what` in failure messages.
    pub(crate) fn capture(what: impl Into<String>, cmd: &mut Command) -> RunLog {
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
    pub(crate) fn combined(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }

    /// The process's stdout with surrounding whitespace removed -- the usual
    /// form for "what did the built program print".
    pub(crate) fn out(&self) -> &str {
        self.stdout.trim()
    }

    /// Assert the process exited zero, reporting both streams if not.
    pub(crate) fn success(self) -> RunLog {
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
pub(crate) fn target_dir(dir: &std::path::Path) -> PathBuf {
    dir.join(".harbour").join("target")
}

/// Run `harbour <args>` in `dir` and capture the result without asserting.
pub(crate) fn harbour_run(home: &std::path::Path, dir: &std::path::Path, args: &[&str]) -> RunLog {
    RunLog::capture(
        format!("harbour {}", args.join(" ")),
        harbour(home).args(args).current_dir(dir),
    )
}

/// As [`harbour_run`], with extra environment variables -- used to change
/// the compiler between two builds of the same tree on purpose.
pub(crate) fn harbour_run_env(
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
pub(crate) fn build_ok(home: &std::path::Path, dir: &std::path::Path) -> RunLog {
    harbour_run(home, dir, &["build"]).success()
}

/// Run a binary Harbour just built and assert the binary itself ran.
///
/// The failure message names the artifact and shows its output, so
/// "the program crashed" and "the program printed the wrong number" are
/// never confused with "the build failed".
pub(crate) fn run_built_exe(app_dir: &std::path::Path, name: &str) -> RunLog {
    run_built_exe_in(app_dir, "debug", name)
}

/// As [`run_built_exe`], for a named profile (`debug` / `release`).
pub(crate) fn run_built_exe_in(app_dir: &std::path::Path, profile: &str, name: &str) -> RunLog {
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
pub(crate) type TreeSnapshot = std::collections::BTreeMap<PathBuf, (u64, std::time::SystemTime)>;

pub(crate) fn snapshot_tree(dir: &std::path::Path) -> TreeSnapshot {
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
pub(crate) struct TreeDiff {
    pub(crate) created: Vec<String>,
    pub(crate) modified: Vec<String>,
    pub(crate) removed: Vec<String>,
    pub(crate) unchanged: Vec<String>,
    /// The tree the snapshots were taken of, kept so a failure can read the
    /// fingerprint cache and say *why* a rebuild was not a no-op.
    pub(crate) root: PathBuf,
}

/// Compare two [`snapshot_tree`] results taken around a build.
pub(crate) fn describe_artifact_changes(
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
pub(crate) fn is_bookkeeping(path: &str) -> bool {
    path.rsplit('/')
        .next()
        .map(|f| f.starts_with('.') || f == "compile_commands.json")
        .unwrap_or(false)
}

impl TreeDiff {
    /// Artifacts the build created or rewrote, bookkeeping excluded.
    pub(crate) fn touched(&self) -> impl Iterator<Item = &str> {
        self.created
            .iter()
            .chain(self.modified.iter())
            .map(String::as_str)
            .filter(|p| !is_bookkeeping(p))
    }

    pub(crate) fn touched_any(&self, needle: &str) -> bool {
        self.touched().any(|p| p.contains(needle))
    }

    /// Assert the build rewrote something whose path contains `needle`
    /// (e.g. `"main.o"`, or a target name to catch its relink).
    pub(crate) fn assert_touched(&self, needle: &str, why: &str) {
        assert!(
            self.touched_any(needle),
            "expected the build to rewrite an artifact matching `{needle}`: {why}\n{self}"
        );
    }

    /// Assert the build left everything matching `needle` alone -- the
    /// "this really was cached" half of an incremental assertion.
    pub(crate) fn assert_untouched(&self, needle: &str, why: &str) {
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
    pub(crate) fn link_cache_report(&self) -> String {
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
    pub(crate) fn assert_nothing_touched(&self, why: &str) {
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
pub(crate) fn rebuild_and_diff(
    home: &std::path::Path,
    dir: &std::path::Path,
) -> (RunLog, TreeDiff) {
    rebuild_and_diff_env(home, dir, &[])
}

/// As [`rebuild_and_diff`], with extra environment variables.
pub(crate) fn rebuild_and_diff_env(
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
pub(crate) fn recorded_toolchain_hashes(
    dir: &std::path::Path,
) -> std::collections::BTreeSet<String> {
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
pub(crate) struct Rebuild {
    pub(crate) log: RunLog,
    pub(crate) diff: TreeDiff,
    pub(crate) toolchain_before: std::collections::BTreeSet<String>,
    pub(crate) toolchain_after: std::collections::BTreeSet<String>,
}

/// Rebuild `dir`, recording both what moved on disk and whether the
/// toolchain changed underneath the build.
pub(crate) fn rebuild(home: &std::path::Path, dir: &std::path::Path) -> Rebuild {
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
    pub(crate) fn assert_reused_everything(&self, context: &str) {
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
pub(crate) struct BuildTwice {
    pub(crate) clean: RunLog,
    pub(crate) second: Rebuild,
}

pub(crate) fn build_twice(home: &std::path::Path, dir: &std::path::Path) -> BuildTwice {
    let clean = build_ok(home, dir);
    let second = rebuild(home, dir);
    BuildTwice { clean, second }
}

impl BuildTwice {
    /// See [`Rebuild::assert_reused_everything`].
    pub(crate) fn assert_incremental_is_a_no_op(&self) {
        self.second.assert_reused_everything(&format!(
            "nothing changed between the two builds, so every object and \
             artifact must have been reused\n\nclean build:\n{}",
            self.clean
        ));
    }
}

/// The archive Harbour produced for a staticlib target, wherever it landed.
///
/// A root target's archive is written to `debug/lib`, a path dependency's to
/// `debug/deps/<pkg>-<version>/lib`, and the extension is `.a` or `.lib`
/// depending on the toolchain -- so this searches rather than guessing.
pub(crate) fn built_archive_path(dir: &std::path::Path, name: &str) -> PathBuf {
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

/// Path to a binary produced under `<app_dir>/.harbour/target/debug/bin`,
/// with the platform-appropriate executable extension.
pub(crate) fn built_exe_path(app_dir: &std::path::Path, name: &str) -> PathBuf {
    built_exe_path_in(app_dir, "debug", name)
}

/// As [`built_exe_path`], for a named profile (`debug` / `release`).
pub(crate) fn built_exe_path_in(app_dir: &std::path::Path, profile: &str, name: &str) -> PathBuf {
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

/// Install a `cc` wrapper that records every argv it is given, and return
/// (path to the wrapper, directory the records land in).
///
/// One file per invocation: Harbour compiles in parallel, and concurrent
/// appends to a single log interleave mid-line -- which, the first time this
/// was tried by hand, produced a "difference" that was purely the log
/// corrupting itself.
#[cfg(not(windows))]
pub(crate) fn install_cc_recorder(tmp: &std::path::Path) -> (PathBuf, PathBuf) {
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
pub(crate) fn recorded_argvs(records: &std::path::Path) -> Vec<Vec<String>> {
    let mut all = Vec::new();
    for entry in fs::read_dir(records).unwrap() {
        let text = fs::read_to_string(entry.unwrap().path()).unwrap();
        all.push(text.lines().map(|l| l.to_string()).collect());
    }
    all
}

/// The flags `harbour flags` printed, in order, with the `# from:`
/// attribution stripped.
pub(crate) fn reported_flags(output: &str) -> Vec<String> {
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
/// Write a generator that dumps its entire environment to
/// `generated/env.txt`, and return the `prebuild` block that runs it.
///
/// The whole environment rather than the variables under test: a dump cannot
/// be written to agree with the assertions, and a variable that is missing
/// shows up as missing rather than as an empty string.
pub(crate) fn write_env_dumping_generator(dir: &std::path::Path, extra_toml: &str) -> String {
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
pub(crate) fn env_dump_get(dump: &str, key: &str) -> Option<String> {
    dump.lines().find_map(|line| {
        let (k, v) = line.split_once('=')?;
        (k == key).then(|| v.to_string())
    })
}

/// Every regular file under `dir`, recursively.
pub(crate) fn walk_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                out.extend(walk_files(&path));
            } else {
                out.push(path);
            }
        }
    }
    out
}
