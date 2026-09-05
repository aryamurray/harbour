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
/// Every file under `dir` with its length and modification time.
///
/// Used to tell "the object was recompiled" from "the object was reused"
/// without relying on log output: Harbour's INFO lines are not emitted on
/// the Windows runners, so `Compiling N file(s)` cannot be the discriminator
/// there.
fn snapshot_tree(
    dir: &std::path::Path,
) -> std::collections::BTreeMap<PathBuf, (u64, std::time::SystemTime)> {
    let mut out = std::collections::BTreeMap::new();
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

/// Path to a binary produced under `<app_dir>/.harbour/target/debug/bin`,
/// with the platform-appropriate executable extension.
fn built_exe_path(app_dir: &std::path::Path, name: &str) -> PathBuf {
    let file_name = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    app_dir
        .join(".harbour")
        .join("target")
        .join("debug")
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

    // Snapshot the build tree so a failure can say *which* stage went
    // wrong. The two candidate causes need different fixes -- `inner` was
    // never recompiled (a fingerprint that failed to invalidate on a
    // propagated feature change), or it was recompiled and the executable
    // was not relinked against the new archive -- and `left: "0"` alone
    // cannot tell them apart. Comparing artifacts rather than log lines is
    // deliberate: the INFO output that would say `Compiling N file(s)` is
    // not emitted on the Windows runners, where this fails. This has
    // failed intermittently on Windows only, printing `0` after the
    // rebuild, and the two candidate causes need different fixes: either
    // `inner` was never recompiled (a fingerprint that failed to invalidate
    // on a propagated feature change), or it was recompiled and the
    // executable was not relinked against the new archive. The build log
    // distinguishes them, and guessing from `left: "0"` alone is what made
    // the first two failures undiagnosable.
    let target_dir = app_dir.join(".harbour").join("target");
    let before = snapshot_tree(&target_dir);

    let rebuild = harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .success()
        .stderr(predicate::str::contains("Finished"));
    let rebuild_log = String::from_utf8_lossy(&rebuild.get_output().stderr).into_owned();

    let after = snapshot_tree(&target_dir);
    let mut changed: Vec<String> = Vec::new();
    let mut unchanged: Vec<String> = Vec::new();
    for (path, stat) in &after {
        let rel = path
            .strip_prefix(&target_dir)
            .unwrap_or(path)
            .display()
            .to_string();
        match before.get(path) {
            Some(old) if old == stat => unchanged.push(rel),
            Some(_) => changed.push(format!("{rel} (modified)")),
            None => changed.push(format!("{rel} (new)")),
        }
    }
    let artifacts = format!(
        "changed during rebuild ({}):\n  {}\nunchanged ({}):\n  {}",
        changed.len(),
        changed.join("\n  "),
        unchanged.len(),
        unchanged.join("\n  ")
    );

    let output = Command::new(&exe).output().unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "1",
        "app now requests `outer/want`; `outer`'s `dep/feature` entry must have \
         propagated `deep` onto `inner`, defined ENABLE_DEEP there, and the \
         fingerprint must have invalidated inner's cached object so the \
         rebuild actually recompiled it.\n\nRebuild log:\n{rebuild_log}\n\n{artifacts}"
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

    harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .success();
    let exe = built_exe_path(&app_dir, "app");
    let out = Command::new(&exe).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "1",
        "sanity: the first build must link the original definition"
    );

    // Same symbol, different file name and different answer. The old
    // object's member name is no longer produced by any source.
    fs::remove_file(lib.join("src/one.c")).unwrap();
    fs::write(lib.join("src/two.c"), "int value(void) { return 2; }\n").unwrap();

    harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .success();

    let out = Command::new(&exe).output().unwrap();
    assert!(
        out.status.success(),
        "the rebuilt program must run; a stale archive member can also \
         surface as a duplicate-symbol link failure"
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "2",
        "the archive must contain only objects for sources that still exist; \
         `1` means the removed source's object is still a member and won \
         symbol resolution"
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
