//! `[package] build_tools`: programs a package's generators need on the build
//! machine, checked before any generator of the build runs.
//!
//! Kept in a file of its own, not appended to the integration tests' shared
//! tail, which is what made every earlier PR collide with the next.

mod common;

use std::fs;
use std::path::Path;

use common::{harbour, home};
use tempfile::TempDir;

/// A tool no machine has, so "missing" is true everywhere.
const MISSING: &str = "harbour-no-such-tool-7c1e";

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// Run `harbour build` in `dir`; `(succeeded, stdout + stderr)`.
fn build(home: &Path, dir: &Path) -> (bool, String) {
    let out = harbour(home)
        .current_dir(dir)
        .arg("build")
        .output()
        .unwrap();
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// A static library whose only generator is a `sh` step that leaves
/// `generated/ran.txt` behind -- the side effect the tests look for to prove a
/// generator did or did not run. `package_extra` goes under `[package]`,
/// `after_target` after the target table (for `when` blocks and features).
fn generator_package(root: &Path, name: &str, package_extra: &str, unconditional_step: bool) {
    write(root, "src/lib.c", "int lib_fn(void) { return 1; }\n");
    let step = if unconditional_step {
        "[[targets.lib.prebuild]]\nprogram = \"sh\"\n\
         args = [\"-c\", \"mkdir -p generated && echo ran > generated/ran.txt\"]\n\
         outputs = [\"generated/ran.txt\"]\n"
    } else {
        ""
    };
    write(
        root,
        "Harbour.toml",
        &format!(
            "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n{package_extra}\n\
             [targets.lib]\nkind = \"staticlib\"\nsources = [\"src/lib.c\"]\n\n{step}"
        ),
    );
}

#[test]
#[cfg(unix)]
fn a_missing_build_tool_fails_before_any_generator_runs() {
    let tmp = TempDir::new().unwrap();
    let home = home(&tmp);
    let pkg = tmp.path().join("pkg");
    generator_package(
        &pkg,
        "needs-tool",
        &format!("build_tools = [\"{MISSING}\"]\n"),
        true,
    );

    let (ok, text) = build(&home, &pkg);
    assert!(!ok, "a missing build tool must fail the build\n{text}");
    assert!(
        text.contains("needs-tool") && text.contains(MISSING) && text.contains("not on PATH"),
        "the error must name the package and the tool and say it is not on PATH\n{text}"
    );
    assert!(
        text.contains("install"),
        "and say what to do about it\n{text}"
    );
    assert!(
        !pkg.join("generated/ran.txt").exists(),
        "the check must run BEFORE the generator, not be discovered by it"
    );
}

#[test]
#[cfg(unix)]
fn a_present_build_tool_lets_the_generator_run() {
    let tmp = TempDir::new().unwrap();
    let home = home(&tmp);
    let pkg = tmp.path().join("pkg");
    generator_package(&pkg, "has-tool", "build_tools = [\"sh\"]\n", true);

    let (ok, text) = build(&home, &pkg);
    assert!(
        ok,
        "a tool that is on PATH must not block the build\n{text}"
    );
    assert!(pkg.join("generated/ran.txt").exists());
}

#[test]
#[cfg(unix)]
fn a_package_that_declares_nothing_is_not_checked() {
    let tmp = TempDir::new().unwrap();
    let home = home(&tmp);
    let pkg = tmp.path().join("pkg");
    generator_package(&pkg, "undeclared", "", true);

    let (ok, text) = build(&home, &pkg);
    assert!(ok, "no `build_tools` means no check\n{text}");
    assert!(pkg.join("generated/ran.txt").exists());
}

/// A package whose generators will not run in this build does not need its
/// tools: blocking on a tool nothing will call would turn a harmless
/// declaration into a reason a working build refuses to start.
#[test]
fn a_package_with_no_generators_is_never_blocked() {
    let tmp = TempDir::new().unwrap();
    let home = home(&tmp);
    let pkg = tmp.path().join("pkg");
    generator_package(
        &pkg,
        "plain",
        &format!("build_tools = [\"{MISSING}\"]\n"),
        false,
    );

    let (ok, text) = build(&home, &pkg);
    assert!(
        ok,
        "no generator runs, so the missing tool is never needed\n{text}"
    );
}

/// The same package, once with the feature that enables its only generator off
/// and once on. Off: nothing runs, nothing is required. On: the tool is needed.
#[test]
#[cfg(unix)]
fn a_generator_behind_a_feature_that_is_off_does_not_demand_its_tool() {
    let tmp = TempDir::new().unwrap();
    let home = home(&tmp);
    let pkg = tmp.path().join("pkg");

    let manifest = |default: &str| {
        format!(
            "[package]\nname = \"gated\"\nversion = \"0.1.0\"\nbuild_tools = [\"{MISSING}\"]\n\n\
             [features]\ndefault = [{default}]\nwithgen = []\n\n\
             [targets.lib]\nkind = \"staticlib\"\nsources = [\"src/lib.c\"]\n\n\
             [[targets.lib.when]]\nfeature = \"withgen\"\n\
             [[targets.lib.when.prebuild]]\nprogram = \"sh\"\n\
             args = [\"-c\", \"mkdir -p generated && echo ran > generated/ran.txt\"]\n\
             outputs = [\"generated/ran.txt\"]\n"
        )
    };
    write(&pkg, "src/lib.c", "int lib_fn(void) { return 1; }\n");

    write(&pkg, "Harbour.toml", &manifest(""));
    let (ok, text) = build(&home, &pkg);
    assert!(
        ok,
        "feature off: its generator does not run, so no tool is needed\n{text}"
    );
    assert!(!pkg.join("generated/ran.txt").exists());

    write(&pkg, "Harbour.toml", &manifest("\"withgen\""));
    let (ok, text) = build(&home, &pkg);
    assert!(
        !ok,
        "feature on: the generator would run, so the tool is needed\n{text}"
    );
    assert!(text.contains(MISSING) && text.contains("gated"), "{text}");
    assert!(
        !pkg.join("generated/ran.txt").exists(),
        "still before the generator"
    );
}

/// The check covers a package reached as a *dependency*, which is how OpenSSL
/// is used: the app has no generators of its own and never mentions `perl`.
#[test]
#[cfg(unix)]
fn a_dependency_s_missing_build_tool_is_reported_before_its_generator() {
    let tmp = TempDir::new().unwrap();
    let home = home(&tmp);
    let dep = tmp.path().join("gen");
    generator_package(
        &dep,
        "gen",
        &format!("build_tools = [\"{MISSING}\"]\n"),
        true,
    );

    let app = tmp.path().join("app");
    write(&app, "src/main.c", "int main(void) { return 0; }\n");
    write(
        &app,
        "Harbour.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\ngen = { path = \"../gen\" }\n\n\
         [targets.app]\nkind = \"exe\"\nsources = [\"src/main.c\"]\n",
    );

    let (ok, text) = build(&home, &app);
    assert!(!ok, "{text}");
    assert!(
        text.contains("package `gen`") && text.contains(MISSING),
        "the dependency, not the app, must be named\n{text}"
    );
    assert!(
        !dep.join("generated/ran.txt").exists(),
        "the dependency's generator must not have run"
    );
}

/// Names only: nothing that is a path, a flag, or a command line.
#[test]
fn build_tools_entries_must_be_plain_program_names() {
    let tmp = TempDir::new().unwrap();
    let home = home(&tmp);

    let bad = [
        ("", "is empty"),
        ("a/b", "path separator"),
        ("a\\\\b", "path separator"),
        ("/usr/bin/perl", "path separator"),
        ("-x", "flag"),
        ("perl --version", "characters other than"),
        ("..", "not a program name"),
    ];
    for (i, (name, why)) in bad.iter().enumerate() {
        let pkg = tmp.path().join(format!("bad{i}"));
        generator_package(
            &pkg,
            "badname",
            &format!("build_tools = [\"{name}\"]\n"),
            false,
        );
        let (ok, text) = build(&home, &pkg);
        assert!(!ok, "entry `{name}` must be rejected at load\n{text}");
        assert!(
            text.contains("build_tools") && text.contains(why),
            "entry `{name}`: expected the problem `{why}` in\n{text}"
        );
    }

    // Real tool names pass validation (they are only looked up when a generator
    // runs, and there is none here).
    let pkg = tmp.path().join("good");
    generator_package(
        &pkg,
        "goodnames",
        "build_tools = [\"perl\", \"python3\", \"g++\", \"nasm.exe\", \"cmake\"]\n",
        false,
    );
    let (ok, text) = build(&home, &pkg);
    assert!(ok, "plain tool names must load\n{text}");
}
