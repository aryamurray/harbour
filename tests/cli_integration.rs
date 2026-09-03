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

#[test]
fn test_build_simple_project() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    // Create project
    harbour(&home)
        .args(["new", "buildtest"])
        .current_dir(tmp.path())
        .assert()
        .success();

    let project_dir = tmp.path().join("buildtest");

    // Build it
    harbour(&home)
        .args(["build"])
        .current_dir(&project_dir)
        .assert()
        .success()
        .stderr(predicate::str::contains("Finished"));

    // Check output exists
    let target_dir = project_dir.join(".harbour").join("target").join("debug");
    assert!(target_dir.exists());
}

#[test]
fn test_build_release_mode() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "releasetest"])
        .current_dir(tmp.path())
        .assert()
        .success();

    let project_dir = tmp.path().join("releasetest");

    harbour(&home)
        .args(["build", "--release"])
        .current_dir(&project_dir)
        .assert()
        .success();

    let target_dir = project_dir.join(".harbour").join("target").join("release");
    assert!(target_dir.exists());
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

#[test]
fn test_clean_removes_target_directory() {
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

#[test]
fn test_full_workflow_with_dependency() {
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

    // Add header
    fs::create_dir_all(lib_dir.join("include")).unwrap();
    fs::write(
        lib_dir.join("include/myutil.h"),
        r#"#ifndef MYUTIL_H
#define MYUTIL_H
#define MYUTIL_VERSION 1
#endif
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

    // 4. Update the app to use the library's header (just include, no linking)
    fs::write(
        app_dir.join("src/main.c"),
        r#"#include <stdio.h>
#include "myutil.h"

int main(void) {
    printf("Using myutil version %d\n", MYUTIL_VERSION);
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

    // 8. Build the application (this verifies the include path propagates)
    harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .success()
        .stderr(predicate::str::contains("Finished"));

    // 9. Verify outputs exist
    let target_dir = app_dir.join(".harbour").join("target").join("debug");
    assert!(target_dir.exists());
}
