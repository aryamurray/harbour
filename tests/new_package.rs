//! The packages `harbour new` scaffolds must compile.
//!
//! A hyphen is legal in a package name and in a directory name but not in a C
//! identifier, and the library template used the name as both. The generated
//! header read `#ifndef BASE-LIB_H` and declared `void base-lib_init(void);`, so
//! `harbour new --lib base-lib && harbour build` failed on every compiler (MSVC
//! says C2008; GCC and clang warn about the guard and then reject the function).
//! Found on a Windows runner, reproduced anywhere.

mod common;

use tempfile::TempDir;

fn scaffold_and_build(args: &[&str], name: &str) -> (bool, String) {
    let tmp = TempDir::new().unwrap();
    let home = common::home(&tmp);
    let made = common::harbour(&home)
        .current_dir(tmp.path())
        .arg("new")
        .args(args)
        .arg(name)
        .output()
        .unwrap();
    assert!(made.status.success(), "`harbour new` itself must work");
    let built = common::harbour(&home)
        .current_dir(tmp.path().join(name))
        .arg("build")
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&built.stdout),
        String::from_utf8_lossy(&built.stderr)
    );
    (built.status.success(), text)
}

#[test]
fn a_hyphenated_library_scaffold_compiles() {
    let (ok, text) = scaffold_and_build(&["--lib"], "base-lib");
    assert!(ok, "the generated library must build\n{text}");
}

#[test]
fn a_hyphenated_executable_scaffold_compiles() {
    let (ok, text) = scaffold_and_build(&[], "my-tool");
    assert!(ok, "the generated executable must build\n{text}");
}

/// The generated manifest turns on `-Wall -Wextra`, so the generated sources
/// must be clean under them: a fresh project builds with zero warnings.
#[test]
fn a_fresh_executable_scaffold_builds_without_warnings() {
    let (ok, text) = scaffold_and_build(&[], "demo");
    assert!(ok, "the generated executable must build\n{text}");
    assert!(
        !text.to_lowercase().contains("warning"),
        "a fresh executable must build without warnings\n{text}"
    );
}

#[test]
fn a_fresh_library_scaffold_builds_without_warnings() {
    let (ok, text) = scaffold_and_build(&["--lib"], "demolib");
    assert!(ok, "the generated library must build\n{text}");
    assert!(
        !text.to_lowercase().contains("warning"),
        "a fresh library must build without warnings\n{text}"
    );
}
