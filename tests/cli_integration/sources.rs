//! The rules for what a target's `sources` must match: a target that compiles
//! nothing is an error, brace patterns are refused, `--strict-sources` makes
//! every applicable pattern count, and `exclude` follows the same path rules as
//! expansion.
#![allow(unused_imports)]

use std::fs;
use std::path::Path;

use assert_cmd::prelude::*;
use predicates::prelude::*;

use crate::common::*;

fn write(dir: &Path, rel: &str, text: &str) {
    let path = dir.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// The OS name a `when` block must use to be active on this machine.
fn this_os() -> &'static str {
    std::env::consts::OS
}

/// A library package `lib` whose single target is described by `target_toml`
/// (the part after `[targets.lib]`).
fn lib_package(dir: &Path, target_toml: &str) {
    write(
        dir,
        "Harbour.toml",
        &format!(
            "[package]\nname = \"lib\"\nversion = \"0.1.0\"\n\n[targets.lib]\n{target_toml}\n"
        ),
    );
}

fn stderr_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// The rule: a library whose patterns match nothing must not "succeed" with no
/// archive. The sources here sit in the package root, the pattern looks in
/// `src/` -- the realistic mistake -- and the message has to name the target,
/// the pattern with its count, the active `when` block that added nothing, and
/// the likely cause.
#[test]
fn test_staticlib_with_no_sources_is_an_error_naming_what_matched_nothing() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let dir = tmp.path().join("lib");
    write(&dir, "a.c", "int a(void) { return 1; }\n");
    lib_package(
        &dir,
        &format!(
            "kind = \"staticlib\"\nsources = [\"src/*.c\"]\n\n\
             [[targets.lib.when]]\nos = \"{}\"\nsources = [\"platform/*.c\"]\n",
            this_os()
        ),
    );

    let out = harbour(&home)
        .args(["build"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(!out.status.success(), "an empty staticlib must not build");
    let err = stderr_of(&out);
    for part in [
        "target 'lib'",
        "package 'lib'",
        "staticlib",
        "`src/*.c` matched 0 files",
        "`platform/*.c` matched 0 files",
        "`[[targets.lib.when]]` block 1",
        "wrong place",
    ] {
        assert!(err.contains(part), "missing `{part}` in:\n{err}");
    }
    assert!(
        snapshot_tree(&target_dir(&dir))
            .keys()
            .all(|p| !p.to_string_lossy().ends_with(".a") && !p.to_string_lossy().ends_with(".lib")),
        "no archive may be produced"
    );
}

#[test]
fn test_exe_with_no_sources_is_an_error() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let dir = tmp.path().join("lib");
    write(&dir, "main.c", "int main(void) { return 0; }\n");
    lib_package(&dir, "kind = \"exe\"\nsources = [\"src/*.c\"]\n");

    let out = harbour(&home)
        .args(["build"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = stderr_of(&out);
    assert!(
        err.contains("target 'lib'") && err.contains("kind exe") && err.contains("matched 0 files"),
        "{err}"
    );
}

/// A pattern that matches files which `exclude` then removes is a different
/// mistake from a wrong directory, and the hint has to say so.
#[test]
fn test_exclude_removing_every_source_is_named_as_the_cause() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let dir = tmp.path().join("lib");
    write(&dir, "src/a.c", "int a(void) { return 1; }\n");
    lib_package(
        &dir,
        "kind = \"staticlib\"\nsources = [\"src/*.c\"]\nexclude = [\"src/a.c\"]\n",
    );

    let out = harbour(&home)
        .args(["build"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = stderr_of(&out);
    assert!(
        err.contains("all removed by `exclude`") && err.contains("`exclude` removed every file"),
        "{err}"
    );
}

/// A dependency is checked the same way, and the message names the package so
/// the culprit is not mistaken for the root.
#[test]
fn test_dependency_with_no_sources_is_an_error_naming_the_package() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let dep = tmp.path().join("dep");
    write(&dep, "dep.c", "int dep(void) { return 1; }\n");
    write(
        &dep,
        "Harbour.toml",
        "[package]\nname = \"dep\"\nversion = \"0.2.0\"\n\n\
         [targets.dep]\nkind = \"staticlib\"\nsources = [\"src/*.c\"]\n",
    );
    let app = tmp.path().join("app");
    write(&app, "src/main.c", "int main(void) { return 0; }\n");
    write(
        &app,
        "Harbour.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\ndep = { path = \"../dep\" }\n\n\
         [targets.app]\nkind = \"exe\"\nsources = [\"src/main.c\"]\n\n\
         [targets.app.deps]\ndep = \"dep\"\n",
    );

    let out = harbour(&home)
        .args(["build"])
        .current_dir(&app)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = stderr_of(&out);
    assert!(
        err.contains("target 'dep' of package 'dep'") && err.contains("`src/*.c` matched 0 files"),
        "{err}"
    );
}

/// `header-only` has nothing to compile; it is the one kind allowed to be empty.
#[test]
fn test_header_only_target_needs_no_sources() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let dir = tmp.path().join("lib");
    write(&dir, "include/lib.h", "int lib(void);\n");
    lib_package(
        &dir,
        "kind = \"header-only\"\n\n[targets.lib.surface.compile.public]\ninclude_dirs = [\"include\"]\n",
    );

    harbour(&home)
        .args(["build"])
        .current_dir(&dir)
        .assert()
        .success();
}

/// One empty glob among matching ones is the documented `src/**/*.S` case: it
/// builds, says nothing by default, notes it under `--verbose`, and is an error
/// under `--strict-sources`.
#[test]
fn test_an_empty_glob_among_matching_ones_is_quiet_verbose_noted_and_strict_fatal() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let dir = tmp.path().join("lib");
    write(&dir, "src/a.c", "int a(void) { return 1; }\n");
    lib_package(
        &dir,
        "kind = \"staticlib\"\nsources = [\"src/**/*.c\", \"src/**/*.S\"]\n",
    );

    let quiet = harbour(&home)
        .args(["build"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(quiet.status.success(), "{}", stderr_of(&quiet));
    assert!(
        !stderr_of(&quiet).contains("src/**/*.S"),
        "normal output must stay clean:\n{}",
        stderr_of(&quiet)
    );

    let verbose = harbour(&home)
        .args(["--verbose", "build"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(verbose.status.success());
    let note = stderr_of(&verbose);
    assert!(
        note.contains("matched no files") && note.contains("`src/**/*.S`"),
        "{note}"
    );

    let strict = harbour(&home)
        .args(["build", "--strict-sources"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(!strict.status.success());
    let err = stderr_of(&strict);
    assert!(
        err.contains("--strict-sources")
            && err.contains("`src/**/*.S`")
            && !err.contains("`src/**/*.c`"),
        "{err}"
    );
}

/// An active `when` block's patterns are held to the strict rule; an inactive
/// block's are not, since its files belong to another platform.
#[test]
fn test_strict_sources_checks_active_when_blocks_only() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let dir = tmp.path().join("lib");
    write(&dir, "src/a.c", "int a(void) { return 1; }\n");
    let manifest = |os: &str| {
        format!(
            "kind = \"staticlib\"\nsources = [\"src/*.c\"]\n\n\
             [[targets.lib.when]]\nos = \"{os}\"\nsources = [\"other/*.c\"]\n"
        )
    };

    lib_package(&dir, &manifest("some-other-os"));
    harbour(&home)
        .args(["build", "--strict-sources"])
        .current_dir(&dir)
        .assert()
        .success();

    lib_package(&dir, &manifest(this_os()));
    harbour(&home)
        .args(["build", "--strict-sources"])
        .current_dir(&dir)
        .assert()
        .failure()
        .stderr(predicate::str::contains("`other/*.c`").and(predicate::str::contains("block 1")));
}

/// `sources` left out means the default alternatives (`.cpp`, `.cc`, `.cxx`);
/// a project that uses one extension must still pass `--strict-sources`.
#[test]
fn test_strict_sources_accepts_the_default_patterns() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let dir = tmp.path().join("lib");
    write(&dir, "src/a.cpp", "int a() { return 1; }\n");
    lib_package(&dir, "kind = \"staticlib\"\nlang = \"c++\"\n");

    harbour(&home)
        .args(["build", "--strict-sources"])
        .current_dir(&dir)
        .assert()
        .success();
}

/// `{a,b}.c` matches a file literally named that, so it used to match nothing
/// and exit 0. It is refused, with the alternatives spelled out -- in every
/// list that is a file pattern, including a `when` block for another platform
/// (which no local build would otherwise ever expand).
#[test]
fn test_brace_patterns_are_rejected_with_a_hint() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let dir = tmp.path().join("lib");
    write(&dir, "adler32.c", "int a(void) { return 1; }\n");
    write(&dir, "crc32.c", "int c(void) { return 1; }\n");

    for (field, toml) in [
        ("`sources`", "sources = [\"{adler32,crc32}.c\"]\n"),
        (
            "`exclude`",
            "sources = [\"*.c\"]\nexclude = [\"{adler32,crc32}.c\"]\n",
        ),
        (
            "`public_headers`",
            "sources = [\"*.c\"]\npublic_headers = [\"{a,b}.h\"]\n",
        ),
        (
            "`sources`",
            "sources = [\"*.c\"]\n\n[[targets.lib.when]]\nos = \"some-other-os\"\nsources = [\"{a,b}.c\"]\n",
        ),
    ] {
        lib_package(&dir, &format!("kind = \"staticlib\"\n{toml}"));
        let out = harbour(&home)
            .args(["build"])
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(!out.status.success(), "{toml} must be rejected");
        let err = stderr_of(&out);
        assert!(
            err.contains(field) && err.contains("does not expand braces") && err.contains("separate entries"),
            "{field}: {err}"
        );
    }

    lib_package(
        &dir,
        "kind = \"staticlib\"\nsources = [\"{adler32,crc32}.c\"]\n",
    );
    let out = harbour(&home)
        .args(["build"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(stderr_of(&out).contains("\"adler32.c\", \"crc32.c\""));
}

/// `exclude` must mean the same thing when checking a named file as when
/// expanding globs. Expansion never lets `*` cross a `/`, but the check used
/// `Pattern::matches` with default options, which does: `src/*.c` "excluded"
/// `src/sub/gone.c`, so a missing vendored file in a subdirectory was excused.
#[test]
fn test_exclude_star_does_not_cross_directories_when_excusing_a_missing_file() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let dir = tmp.path().join("lib");
    write(&dir, "src/ok.c", "int ok(void) { return 1; }\n");
    lib_package(
        &dir,
        "kind = \"staticlib\"\n\
         sources = [\"src/ok.c\", \"src/sub/gone.c\", \"src/gone_too.c\"]\n\
         exclude = [\"src/*.c\"]\n",
    );

    let out = harbour(&home)
        .args(["build"])
        .current_dir(&dir)
        .output()
        .unwrap();
    // `src/ok.c` is excluded too, so nothing is left -- but the error that
    // matters first is the missing named file the exclude does not cover.
    assert!(!out.status.success());
    let err = stderr_of(&out);
    assert!(
        err.contains("source(s) that do not exist")
            && err.contains("src/sub/gone.c")
            && !err.contains("src/gone_too.c"),
        "only the file `src/*.c` does not cover is missing:\n{err}"
    );
}

/// ...and the same rule on the expansion side: `src/*.c` removes the top-level
/// file and leaves the one in the subdirectory.
#[test]
fn test_exclude_star_keeps_files_in_subdirectories() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let dir = tmp.path().join("lib");
    write(&dir, "src/top.c", "int top(void) { return 1; }\n");
    write(&dir, "src/sub/deep.c", "int deep(void) { return 1; }\n");
    lib_package(
        &dir,
        "kind = \"staticlib\"\nsources = [\"src/**/*.c\"]\nexclude = [\"src/*.c\"]\n",
    );

    harbour(&home)
        .args(["build"])
        .current_dir(&dir)
        .assert()
        .success();
    let commands = fs::read_to_string(dir.join(".harbour/compile_commands.json")).unwrap();
    assert!(commands.contains("deep.c"), "{commands}");
    assert!(!commands.contains("top.c"), "{commands}");
}
