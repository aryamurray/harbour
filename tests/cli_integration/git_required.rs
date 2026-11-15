//! `git` is a hard requirement: without it a git dependency is a clear error
//! naming what needs git and how to install it, `harbour doctor` says so, and the
//! old `HARBOUR_GIT_BACKEND` switch is reported as gone rather than ignored.

use std::fs;
use std::path::Path;

use harbour::test_support::fixtures::local_registry;

use crate::common::{harbour, harbour_home, harbour_run, harbour_run_env, temp_dir};

/// A library committed as a git repository, and a project depending on it.
fn project_with_a_git_dependency(tmp: &tempfile::TempDir) -> std::path::PathBuf {
    let lib = tmp.path().join("lib");
    fs::create_dir_all(lib.join("src")).unwrap();
    fs::write(
        lib.join("Harbour.toml"),
        "[package]\nname = \"lib\"\nversion = \"1.0.0\"\n\n\
         [targets.lib]\nkind = \"staticlib\"\nsources = [\"src/lib.c\"]\n",
    )
    .unwrap();
    fs::write(lib.join("src/lib.c"), "int lib_value(void) { return 1; }\n").unwrap();
    local_registry::commit_all(&lib, "lib").unwrap();

    let app = tmp.path().join("app");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(
        app.join("Harbour.toml"),
        format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
             [dependencies]\nlib = {{ git = \"{}\" }}\n\n\
             [targets.app]\nkind = \"exe\"\nsources = [\"src/main.c\"]\n",
            local_registry::file_url(&lib)
        ),
    )
    .unwrap();
    fs::write(app.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();
    app
}

/// A `PATH` with nothing on it, so no `git` is found.
fn path_without_git(tmp: &tempfile::TempDir) -> String {
    let empty = tmp.path().join("empty-path");
    fs::create_dir_all(&empty).unwrap();
    empty.to_string_lossy().into_owned()
}

#[test]
fn a_git_dependency_without_git_is_a_clear_error() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let app = project_with_a_git_dependency(&tmp);

    let run = harbour_run_env(
        &home,
        &app,
        &["build"],
        &[("PATH", &path_without_git(&tmp))],
    );
    assert!(!run.status.success(), "{run}");
    let text = run.combined();
    assert!(text.contains("git was not found on PATH"), "{run}");
    assert!(
        text.contains("git dependencies") && text.contains("registries"),
        "the error must say what needs git\n{run}"
    );
    assert!(
        text.contains("apt install git") && text.contains("brew install git"),
        "the error must say how to install it\n{run}"
    );
    assert!(
        !text.contains("No such file or directory") && !text.contains("os error"),
        "a raw spawn error must never reach the user\n{run}"
    );
}

#[test]
fn doctor_reports_a_missing_git_as_required() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let run = harbour_run_env(
        &home,
        tmp.path(),
        &["doctor", "-v"],
        &[("PATH", &path_without_git(&tmp))],
    );
    let text = run.combined();
    assert!(text.contains("[!!] Git"), "{run}");
    assert!(text.contains("Git not found (required"), "{run}");
    assert!(!text.contains("Git (optional)"), "{run}");
}

#[test]
fn doctor_finds_the_git_that_is_installed() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let run = harbour_run(&home, tmp.path(), &["doctor", "-v"]);
    assert!(run.combined().contains("[OK] Git"), "{run}");
}

#[test]
fn the_removed_backend_switch_is_reported_not_silently_ignored() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let app = project_with_a_git_dependency(&tmp);

    // Fetching runs before anything is compiled; whether the build then succeeds
    // is not what this is about.
    let mut cmd = harbour(&home);
    cmd.args(["build"])
        .current_dir(&app)
        .env("HARBOUR_GIT_BACKEND", "libgit2");
    let run = crate::common::RunLog::capture("harbour build", &mut cmd);
    let stderr = &run.stderr;
    assert_eq!(
        stderr
            .matches("HARBOUR_GIT_BACKEND no longer exists")
            .count(),
        1,
        "warned once\n{run}"
    );
    assert!(
        !stderr.contains("libgit2 fallback"),
        "no fallback is offered any more\n{run}"
    );
    assert_cloned(&home);
}

/// The dependency was fetched with the system `git` despite the switch.
fn assert_cloned(home: &Path) {
    let found = walk(home)
        .into_iter()
        .any(|p| p.ends_with("Harbour.toml") && p.components().any(|c| c.as_os_str() == "git"));
    assert!(
        found,
        "the git dependency must have been cloned into the cache"
    );
}

fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    crate::common::walk_files(dir)
}
