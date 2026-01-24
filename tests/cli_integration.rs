//! CLI integration tests for Harbour.
//!
//! These tests verify the full CLI workflow from project creation through building.

use std::fs;
use std::process::Command;

use assert_cmd::prelude::*;
use predicates::prelude::*;
use tempfile::TempDir;

/// Get the harbour binary command.
fn harbour() -> Command {
    Command::cargo_bin("harbour").unwrap()
}

/// Create a temporary directory for test projects.
fn temp_dir() -> TempDir {
    TempDir::new().unwrap()
}

// ============================================================================
// harbour new
// ============================================================================

#[test]
fn test_new_creates_executable_project() {
    let tmp = temp_dir();
    let project_dir = tmp.path().join("myapp");

    harbour()
        .args(["new", "myapp"])
        .current_dir(tmp.path())
        .assert()
        .success();

    // Check project structure
    assert!(project_dir.join("Harbor.toml").exists());
    assert!(project_dir.join("src").exists());
    assert!(project_dir.join("src/main.c").exists());

    // Check manifest content
    let manifest = fs::read_to_string(project_dir.join("Harbor.toml")).unwrap();
    assert!(manifest.contains("name = \"myapp\""));
    assert!(manifest.contains("kind = \"exe\""));
}

#[test]
fn test_new_creates_library_project() {
    let tmp = temp_dir();
    let project_dir = tmp.path().join("mylib");

    harbour()
        .args(["new", "mylib", "--lib"])
        .current_dir(tmp.path())
        .assert()
        .success();

    // Check project structure
    assert!(project_dir.join("Harbor.toml").exists());
    assert!(project_dir.join("src").exists());
    assert!(project_dir.join("include").exists());

    // Check manifest content
    let manifest = fs::read_to_string(project_dir.join("Harbor.toml")).unwrap();
    assert!(manifest.contains("name = \"mylib\""));
    assert!(manifest.contains("kind = \"staticlib\""));
}

#[test]
fn test_new_fails_if_directory_exists() {
    let tmp = temp_dir();
    let project_dir = tmp.path().join("existing");
    fs::create_dir(&project_dir).unwrap();

    harbour()
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

    harbour()
        .args(["init"])
        .current_dir(tmp.path())
        .assert()
        .success();

    assert!(tmp.path().join("Harbor.toml").exists());
    assert!(tmp.path().join("src").exists());
}

#[test]
fn test_init_fails_if_manifest_exists() {
    let tmp = temp_dir();
    fs::write(tmp.path().join("Harbor.toml"), "[package]\nname = \"test\"\n").unwrap();

    harbour()
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

    // Create project
    harbour()
        .args(["new", "buildtest"])
        .current_dir(tmp.path())
        .assert()
        .success();

    let project_dir = tmp.path().join("buildtest");

    // Build it
    harbour()
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

    harbour()
        .args(["new", "releasetest"])
        .current_dir(tmp.path())
        .assert()
        .success();

    let project_dir = tmp.path().join("releasetest");

    harbour()
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

    harbour()
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

    harbour()
        .args(["new", "treetest"])
        .current_dir(tmp.path())
        .assert()
        .success();

    let project_dir = tmp.path().join("treetest");

    harbour()
        .args(["tree"])
        .current_dir(&project_dir)
        .assert()
        .success()
        .stdout(predicate::str::contains("treetest"));
}

#[test]
fn test_tree_fails_without_manifest() {
    let tmp = temp_dir();

    harbour()
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

    harbour()
        .args(["new", "flagstest"])
        .current_dir(tmp.path())
        .assert()
        .success();

    let project_dir = tmp.path().join("flagstest");

    harbour()
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

    harbour()
        .args(["new", "flagstest2"])
        .current_dir(tmp.path())
        .assert()
        .success();

    let project_dir = tmp.path().join("flagstest2");

    harbour()
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

    harbour()
        .args(["new", "cleantest"])
        .current_dir(tmp.path())
        .assert()
        .success();

    let project_dir = tmp.path().join("cleantest");

    // Build first to create artifacts
    harbour()
        .args(["build"])
        .current_dir(&project_dir)
        .assert()
        .success();

    let target_dir = project_dir.join(".harbour").join("target");
    assert!(target_dir.exists());

    // Clean
    harbour()
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

    // Create main project
    harbour()
        .args(["new", "mainpkg"])
        .current_dir(tmp.path())
        .assert()
        .success();

    // Create dependency project
    harbour()
        .args(["new", "deppkg", "--lib"])
        .current_dir(tmp.path())
        .assert()
        .success();

    let main_dir = tmp.path().join("mainpkg");

    // Add dependency
    harbour()
        .args(["add", "deppkg", "--path", "../deppkg"])
        .current_dir(&main_dir)
        .assert()
        .success();

    // Check manifest was updated
    let manifest = fs::read_to_string(main_dir.join("Harbor.toml")).unwrap();
    assert!(manifest.contains("[dependencies]"));
    assert!(manifest.contains("deppkg"));
}

#[test]
fn test_add_requires_path_or_git() {
    let tmp = temp_dir();

    harbour()
        .args(["new", "addtest"])
        .current_dir(tmp.path())
        .assert()
        .success();

    let project_dir = tmp.path().join("addtest");

    harbour()
        .args(["add", "somepkg"])
        .current_dir(&project_dir)
        .assert()
        .failure()
        .stderr(predicate::str::contains("--path").or(predicate::str::contains("--git")));
}

#[test]
fn test_remove_dependency() {
    let tmp = temp_dir();

    // Create projects
    harbour()
        .args(["new", "remmain"])
        .current_dir(tmp.path())
        .assert()
        .success();

    harbour()
        .args(["new", "remdep", "--lib"])
        .current_dir(tmp.path())
        .assert()
        .success();

    let main_dir = tmp.path().join("remmain");

    // Add then remove
    harbour()
        .args(["add", "remdep", "--path", "../remdep"])
        .current_dir(&main_dir)
        .assert()
        .success();

    harbour()
        .args(["remove", "remdep"])
        .current_dir(&main_dir)
        .assert()
        .success();

    let manifest = fs::read_to_string(main_dir.join("Harbor.toml")).unwrap();
    assert!(!manifest.contains("remdep"));
}

// ============================================================================
// harbour linkplan
// ============================================================================

#[test]
fn test_linkplan_shows_output() {
    let tmp = temp_dir();

    harbour()
        .args(["new", "linktest"])
        .current_dir(tmp.path())
        .assert()
        .success();

    let project_dir = tmp.path().join("linktest");

    harbour()
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

    harbour()
        .args(["new", "explaintest"])
        .current_dir(tmp.path())
        .assert()
        .success();

    let project_dir = tmp.path().join("explaintest");

    harbour()
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

    harbour()
        .args(["new", "explaintest2"])
        .current_dir(tmp.path())
        .assert()
        .success();

    let project_dir = tmp.path().join("explaintest2");

    harbour()
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

    harbour()
        .args(["new", "testnotest"])
        .current_dir(tmp.path())
        .assert()
        .success();

    let project_dir = tmp.path().join("testnotest");

    harbour()
        .args(["test"])
        .current_dir(&project_dir)
        .assert()
        .success()
        .stdout(predicate::str::contains("No test targets found"));
}

#[test]
fn test_test_discovers_test_target() {
    let tmp = temp_dir();

    harbour()
        .args(["new", "testwithtest"])
        .current_dir(tmp.path())
        .assert()
        .success();

    let project_dir = tmp.path().join("testwithtest");

    // Add a test target to the manifest
    let manifest_path = project_dir.join("Harbor.toml");
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

    harbour()
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

    harbour()
        .args(["new", "toolchaintest"])
        .current_dir(tmp.path())
        .assert()
        .success();

    let project_dir = tmp.path().join("toolchaintest");

    harbour()
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

    // 1. Create a library
    harbour()
        .args(["new", "myutil", "--lib"])
        .current_dir(tmp.path())
        .assert()
        .success();

    let lib_dir = tmp.path().join("myutil");

    // Update manifest to expose include dir
    fs::write(
        lib_dir.join("Harbor.toml"),
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
    harbour()
        .args(["new", "myapp"])
        .current_dir(tmp.path())
        .assert()
        .success();

    let app_dir = tmp.path().join("myapp");

    // 3. Add the library as a dependency
    harbour()
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
    harbour()
        .args(["tree"])
        .current_dir(&app_dir)
        .assert()
        .success()
        .stdout(predicate::str::contains("myapp"))
        .stdout(predicate::str::contains("myutil"));

    // 6. Check flags show the dependency's include path
    harbour()
        .args(["flags", "myapp"])
        .current_dir(&app_dir)
        .assert()
        .success()
        .stdout(predicate::str::contains("myutil"));

    // 7. Check linkplan shows the dependency
    harbour()
        .args(["linkplan", "myapp"])
        .current_dir(&app_dir)
        .assert()
        .success()
        .stdout(predicate::str::contains("myutil"));

    // 8. Build the application (this verifies the include path propagates)
    harbour()
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .success()
        .stderr(predicate::str::contains("Finished"));

    // 9. Verify outputs exist
    let target_dir = app_dir.join(".harbour").join("target").join("debug");
    assert!(target_dir.exists());
}
