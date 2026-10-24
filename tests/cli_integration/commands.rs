//! The everyday commands: new, init, build, tree, flags, clean, add, remove, linkplan, explain, test, toolchain.
#![allow(unused_imports)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::prelude::*;
use predicates::prelude::*;
use tempfile::TempDir;

use harbour::test_support::fixtures::local_registry;

use crate::common::*;

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
