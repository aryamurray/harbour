//! `harbour run`: build an executable target and exec it.
//!
//! Like the rest of the suite these need a real C compiler on PATH.

use std::fs;
use std::path::Path;

use tempfile::TempDir;

use crate::common::*;

/// A package named `pkg` with one `[targets.<name>]` exe per entry in `exes`.
/// Each prints its name followed by its arguments as `[arg]`, and exits 3 if
/// its first argument is `fail`, else 0.
fn write_package(dir: &Path, exes: &[&str], extra_toml: &str) {
    fs::create_dir_all(dir.join("src")).unwrap();
    let mut manifest = String::from("[package]\nname = \"pkg\"\nversion = \"0.1.0\"\n\n");
    for name in exes {
        fs::write(
            dir.join(format!("src/{name}.c")),
            format!(
                r#"#include <stdio.h>
#include <string.h>

int main(int argc, char **argv) {{
    printf("{name}");
    for (int i = 1; i < argc; i++) printf(" [%s]", argv[i]);
    printf("\n");
    return (argc > 1 && strcmp(argv[1], "fail") == 0) ? 3 : 0;
}}
"#
            ),
        )
        .unwrap();
        manifest.push_str(&format!(
            "[targets.{name}]\nkind = \"exe\"\nsources = [\"src/{name}.c\"]\n\n"
        ));
    }
    manifest.push_str(extra_toml);
    fs::write(dir.join("Harbour.toml"), manifest).unwrap();
}

fn project(exes: &[&str], extra_toml: &str) -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let dir = tmp.path().join("pkg");
    write_package(&dir, exes, extra_toml);
    (tmp, home, dir)
}

#[test]
fn test_run_builds_and_runs_the_only_executable() {
    let (_tmp, home, dir) = project(&["hello"], "");
    let log = harbour_run(&home, &dir, &["run"]).success();
    assert_eq!(log.out(), "hello", "{log}");
    assert!(log.stderr.contains("Running"), "{log}");
}

#[test]
fn test_run_quiet_suppresses_the_running_line() {
    let (_tmp, home, dir) = project(&["hello"], "");
    let log = harbour_run(&home, &dir, &["--quiet", "run"]).success();
    assert_eq!(log.out(), "hello", "{log}");
    assert!(!log.stderr.contains("Running"), "{log}");
}

#[test]
fn test_run_forwards_arguments_after_double_dash() {
    let (_tmp, home, dir) = project(&["hello"], "");
    let log = harbour_run(&home, &dir, &["run", "--", "a", "--release", "b c"]).success();
    assert_eq!(log.out(), "hello [a] [--release] [b c]", "{log}");
}

#[test]
fn test_run_propagates_the_programs_exit_code() {
    let (_tmp, home, dir) = project(&["hello"], "");
    let log = harbour_run(&home, &dir, &["run", "--", "fail"]);
    assert_eq!(log.status.code(), Some(3), "{log}");
    assert_eq!(log.out(), "hello [fail]", "{log}");
}

#[test]
fn test_run_with_several_executables_names_the_candidates() {
    let (_tmp, home, dir) = project(&["alpha", "beta"], "");
    let log = harbour_run(&home, &dir, &["run"]);
    assert!(!log.status.success(), "{log}");
    let err = log.combined();
    assert!(err.contains("alpha") && err.contains("beta"), "{log}");
    assert!(err.contains("name the one to run"), "{log}");
}

#[test]
fn test_run_runs_the_named_target() {
    let (_tmp, home, dir) = project(&["alpha", "beta"], "");
    let log = harbour_run(&home, &dir, &["run", "beta", "--", "x"]).success();
    assert_eq!(log.out(), "beta [x]", "{log}");
}

#[test]
fn test_run_rejects_a_target_that_is_not_an_executable() {
    let (_tmp, home, dir) = project(
        &["alpha"],
        "[targets.core]\nkind = \"staticlib\"\nsources = [\"src/alpha.c\"]\n",
    );
    let log = harbour_run(&home, &dir, &["run", "core"]);
    assert!(!log.status.success(), "{log}");
    let err = log.combined();
    assert!(err.contains("not an executable target"), "{log}");
    assert!(err.contains("alpha"), "{log}");
}

#[test]
fn test_run_of_a_library_only_package_is_an_error() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let dir = tmp.path().join("pkg");
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(dir.join("src/lib.c"), "int answer(void) { return 42; }\n").unwrap();
    fs::write(
        dir.join("Harbour.toml"),
        "[package]\nname = \"pkg\"\nversion = \"0.1.0\"\n\n\
         [targets.pkg]\nkind = \"staticlib\"\nsources = [\"src/lib.c\"]\n",
    )
    .unwrap();
    let log = harbour_run(&home, &dir, &["run"]);
    assert!(!log.status.success(), "{log}");
    assert!(log.combined().contains("no executable target"), "{log}");
}

#[test]
fn test_run_does_not_default_to_a_test_pattern_target() {
    let (_tmp, home, dir) = project(&["app", "unit_test"], "");
    let log = harbour_run(&home, &dir, &["run"]).success();
    assert_eq!(log.out(), "app", "{log}");

    // With only test targets there is nothing to default to.
    let (_tmp2, home2, dir2) = project(&["unit_test"], "");
    let log = harbour_run(&home2, &dir2, &["run"]);
    assert!(!log.status.success(), "{log}");
    assert!(log.combined().contains("no executable target"), "{log}");
}

#[test]
fn test_run_refuses_a_target_triple_the_host_cannot_run() {
    let (_tmp, home, dir) = project(&["hello"], "");
    let log = harbour_run(
        &home,
        &dir,
        &["run", "--target-triple", "riscv64gc-unknown-none-elf"],
    );
    assert!(!log.status.success(), "{log}");
    assert!(
        log.combined().contains("cannot run a binary built for"),
        "{log}"
    );
}

/// The program starts in the directory `harbour run` was invoked from, not in
/// the build tree.
#[cfg(unix)]
#[test]
fn test_run_leaves_the_working_directory_alone() {
    let (_tmp, home, dir) = project(&[], "");
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join("src/where.c"),
        "#include <stdio.h>\n#include <unistd.h>\n\
         int main(void) { char b[4096]; puts(getcwd(b, sizeof b)); return 0; }\n",
    )
    .unwrap();
    let manifest = fs::read_to_string(dir.join("Harbour.toml")).unwrap();
    fs::write(
        dir.join("Harbour.toml"),
        format!("{manifest}[targets.where]\nkind = \"exe\"\nsources = [\"src/where.c\"]\n"),
    )
    .unwrap();
    fs::create_dir_all(dir.join("sub")).unwrap();
    let sub = dir.join("sub");
    let log = harbour_run(&home, &sub, &["run"]).success();
    assert_eq!(
        fs::canonicalize(log.out()).unwrap(),
        fs::canonicalize(&sub).unwrap(),
        "{log}"
    );
}
