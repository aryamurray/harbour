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

/// Scaffold, then build with warnings fatal, and return the build's outcome
/// together with the compile arguments recorded for the one source file.
///
/// "Builds without the word `warning` in the output" is only as strong as
/// the output: `cl` writes file-level diagnostics to stdout, and Harbour
/// relays stderr from a compile that succeeded. Making the warnings fatal
/// asks the compiler directly, on every toolchain, and the recorded argv
/// proves the level was really raised when it was asked.
fn scaffold_and_build_strictly(args: &[&str], name: &str) -> (bool, String, Vec<String>) {
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
    let dir = tmp.path().join(name);

    let manifest_path = dir.join("Harbour.toml");
    let manifest = std::fs::read_to_string(&manifest_path).unwrap();
    assert_eq!(
        manifest.matches("warnings = \"all\"\n").count(),
        1,
        "the scaffold turns warnings on with the one portable key:\n{manifest}"
    );
    std::fs::write(
        &manifest_path,
        manifest.replace(
            "warnings = \"all\"\n",
            "warnings = \"all\"\nwarnings_as_errors = true\n",
        ),
    )
    .unwrap();

    let built = common::harbour(&home)
        .current_dir(&dir)
        .arg("build")
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&built.stdout),
        String::from_utf8_lossy(&built.stderr)
    );

    let commands = std::fs::read_to_string(dir.join(".harbour/compile_commands.json"))
        .unwrap_or_else(|e| panic!("no compile database: {e}\n{text}"));
    let entries: serde_json::Value = serde_json::from_str(&commands).unwrap();
    let entries = entries.as_array().unwrap();
    assert_eq!(entries.len(), 1, "the scaffold has one source:\n{commands}");
    let argv = entries[0]["arguments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap().to_string())
        .collect();
    (built.status.success(), text, argv)
}

/// What `warnings = "all"` plus `warnings_as_errors = true` is on the
/// compiler these tests run under.
fn strict_flags() -> &'static [&'static str] {
    if cfg!(target_env = "msvc") {
        &["/W4", "/WX"]
    } else {
        &["-Wall", "-Wextra", "-Werror"]
    }
}

/// The generated sources are clean at the level the generated manifest asks
/// for -- so clean that making every warning an error still builds.
#[test]
fn a_fresh_executable_scaffold_is_clean_with_warnings_as_errors() {
    let (ok, text, argv) = scaffold_and_build_strictly(&[], "demo");
    for flag in strict_flags() {
        assert!(
            argv.iter().any(|a| a == flag),
            "`{flag}` must reach the compiler: {argv:?}"
        );
    }
    assert!(
        ok,
        "a fresh executable must have no warnings to fail on\n{text}"
    );
}

#[test]
fn a_fresh_library_scaffold_is_clean_with_warnings_as_errors() {
    let (ok, text, argv) = scaffold_and_build_strictly(&["--lib"], "demo-lib");
    for flag in strict_flags() {
        assert!(
            argv.iter().any(|a| a == flag),
            "`{flag}` must reach the compiler: {argv:?}"
        );
    }
    assert!(
        ok,
        "a fresh library must have no warnings to fail on\n{text}"
    );
}

/// The generated manifest sets `warnings = "all"`, so the generated sources
/// must be clean under it: a fresh project builds with zero warnings.
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
