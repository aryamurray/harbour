//! A target's `warnings` / `warnings_as_errors`: spelled by the toolchain,
//! ordered ahead of `cflags`, fingerprinted, and private to the target.
#![allow(unused_imports)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::prelude::*;
use predicates::prelude::*;
use tempfile::TempDir;

use crate::common::*;

/// What `warnings = "all"` is on the compiler under test.
fn all_flag() -> &'static str {
    if cfg!(target_env = "msvc") {
        "/W4"
    } else {
        "-Wall"
    }
}

/// A flag that silences everything, as `warnings = "none"` spells it.
fn none_flag() -> &'static str {
    if cfg!(target_env = "msvc") {
        "/w"
    } else {
        "-w"
    }
}

/// A program that is correct and has one unused local variable.
///
/// GCC and clang report it under `-Wall` (`-Wunused-variable`) and `cl`
/// under `/W4` (C4189, a level 4 warning: "local variable is initialized but
/// not referenced"). Neither says a word at its default level, which is what
/// makes it a clean probe for whether the level was raised.
const MAIN_WITH_UNUSED_LOCAL: &str = "#include <stdio.h>\n\
     int main(void) { int unused_here = 1; printf(\"ran\\n\"); return 0; }\n";

fn write_app(dir: &Path, name: &str, target_keys: &str, rest: &str, main_c: &str) {
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(dir.join("src/main.c"), main_c).unwrap();
    fs::write(
        dir.join("Harbour.toml"),
        format!(
            "[package]\n\
             name = \"{name}\"\n\
             version = \"0.1.0\"\n\
             \n\
             {rest}\
             [targets.{name}]\n\
             kind = \"exe\"\n\
             sources = [\"src/main.c\"]\n\
             {target_keys}\n"
        ),
    )
    .unwrap();
}

/// The compile arguments `compile_commands.json` records for the one entry
/// whose source file is called `file_name`.
fn recorded_arguments(project: &Path, file_name: &str) -> Vec<String> {
    let text = fs::read_to_string(project.join(".harbour/compile_commands.json")).unwrap();
    let entries: serde_json::Value = serde_json::from_str(&text).unwrap();
    let matching: Vec<&serde_json::Value> = entries
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| {
            Path::new(e["file"].as_str().unwrap())
                .file_name()
                .and_then(|f| f.to_str())
                == Some(file_name)
        })
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "expected exactly one compile of `{file_name}` in:\n{text}"
    );
    matching[0]["arguments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap().to_string())
        .collect()
}

fn position(args: &[String], flag: &str) -> usize {
    args.iter()
        .position(|a| a == flag)
        .unwrap_or_else(|| panic!("`{flag}` is not on the command line: {args:?}"))
}

/// The warning flags come before the target's own `cflags`, so a `cflags`
/// entry can take a warning -- or its fatality -- back out.
///
/// Three builds of one source, differing only in the manifest:
///
/// - `warnings = "all"` + `warnings_as_errors = true` must **fail**, on the
///   unused variable. This is also the proof the key does anything.
/// - the same plus a `cflags` entry that makes warnings non-fatal again
///   (`-Wno-error` / `/WX-`) must **build and run**. Had the warning flags
///   been appended after the `cflags`, `-Werror` would be last and win.
/// - the same plus a `cflags` entry that disables that one warning
///   (`-Wno-unused-variable` / `/wd4189`) must build without mentioning it.
#[test]
fn test_warning_flags_precede_cflags_so_a_cflag_wins() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let dir = tmp.path().join("warnorder");

    let strict = "warnings = \"all\"\nwarnings_as_errors = true\n";
    let (not_fatal, not_reported) = if cfg!(target_env = "msvc") {
        ("/WX-", "/wd4189")
    } else {
        ("-Wno-error", "-Wno-unused-variable")
    };

    write_app(&dir, "warnorder", strict, "", MAIN_WITH_UNUSED_LOCAL);
    let failed = harbour_run(&home, &dir, &["build"]);
    assert!(
        !failed.status.success(),
        "`warnings = \"all\"` with `warnings_as_errors = true` must fail on an \
         unused variable\n{failed}"
    );
    assert!(
        failed.combined().contains("unused_here"),
        "and it must fail on that variable, not on something unrelated\n{failed}"
    );

    write_app(
        &dir,
        "warnorder",
        &format!("{strict}\n[targets.warnorder.private]\ncflags = [\"{not_fatal}\"]\n"),
        "",
        MAIN_WITH_UNUSED_LOCAL,
    );
    build_ok(&home, &dir);
    assert_eq!(
        run_built_exe(&dir, "warnorder").out(),
        "ran",
        "`{not_fatal}` in the target's cflags comes after the warning flags and \
         must win"
    );

    // The same thing read off the command line, where the order is explicit.
    let args = recorded_arguments(&dir, "main.c");
    let as_errors = if cfg!(target_env = "msvc") {
        "/WX"
    } else {
        "-Werror"
    };
    assert!(
        position(&args, all_flag()) < position(&args, as_errors)
            && position(&args, as_errors) < position(&args, not_fatal),
        "expected `{}` then `{as_errors}` then the target's `{not_fatal}`: {args:?}",
        all_flag()
    );

    write_app(
        &dir,
        "warnorder",
        &format!("{strict}\n[targets.warnorder.private]\ncflags = [\"{not_reported}\"]\n"),
        "",
        MAIN_WITH_UNUSED_LOCAL,
    );
    let quiet = build_ok(&home, &dir);
    assert!(
        !quiet.combined().contains("unused_here"),
        "`{not_reported}` must switch that one warning off again\n{quiet}"
    );
}

/// `warnings` is part of the compile fingerprint: changing it recompiles,
/// and not changing it does not.
///
/// `all` to `none` rather than `all` to `pedantic`, because the latter two
/// are the same command line on MSVC.
#[test]
fn test_changing_warnings_recompiles() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let dir = tmp.path().join("warnfp");

    write_app(
        &dir,
        "warnfp",
        "warnings = \"all\"\n",
        "",
        MAIN_WITH_UNUSED_LOCAL,
    );
    let first = build_ok(&home, &dir);
    assert!(
        first.combined().contains("unused_here"),
        "`warnings = \"all\"` must report the unused variable\n{first}"
    );
    assert!(recorded_arguments(&dir, "main.c").contains(&all_flag().to_string()));

    build_twice(&home, &dir).assert_incremental_is_a_no_op();

    write_app(
        &dir,
        "warnfp",
        "warnings = \"none\"\n",
        "",
        MAIN_WITH_UNUSED_LOCAL,
    );
    let (log, diff) = rebuild_and_diff(&home, &dir);
    diff.assert_touched(
        "main",
        "the warning level is on the compile command line, so changing it \
         must recompile the object",
    );
    assert!(
        !log.combined().contains("unused_here"),
        "`warnings = \"none\"` must silence the compiler\n{log}"
    );
    let args = recorded_arguments(&dir, "main.c");
    assert!(args.contains(&none_flag().to_string()), "{args:?}");
    assert!(!args.contains(&all_flag().to_string()), "{args:?}");
}

fn write_lib(dir: &Path, name: &str, target_keys: &str) {
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::create_dir_all(dir.join("include")).unwrap();
    fs::write(
        dir.join("include").join(format!("{name}.h")),
        format!("int {name}_value(void);\n"),
    )
    .unwrap();
    // The same unused local as the application's, under a name that says
    // whose it is.
    fs::write(
        dir.join("src").join(format!("{name}.c")),
        format!(
            "#include \"{name}.h\"\n\
             int {name}_value(void) {{ int unused_in_dep = 1; return 42; }}\n"
        ),
    )
    .unwrap();
    fs::write(
        dir.join("Harbour.toml"),
        format!(
            "[package]\n\
             name = \"{name}\"\n\
             version = \"1.0.0\"\n\
             \n\
             [targets.{name}]\n\
             kind = \"staticlib\"\n\
             sources = [\"src/{name}.c\"]\n\
             {target_keys}\n\
             [targets.{name}.public]\n\
             include_dirs = [\"include\"]\n"
        ),
    )
    .unwrap();
}

const MAIN_USING_DEP: &str = "#include <stdio.h>\n\
     #include \"wdep.h\"\n\
     int main(void) { printf(\"%d\\n\", wdep_value()); return 0; }\n";

fn has_warning_flag(args: &[String]) -> bool {
    args.iter().any(|a| {
        if cfg!(target_env = "msvc") {
            a.starts_with("/W") || a == "/w"
        } else {
            a.starts_with("-W") || a == "-w"
        }
    })
}

/// The setting stays on the target that wrote it, in both directions.
///
/// - A consumer with `warnings = "all"` and `warnings_as_errors = true`
///   builds a path dependency that has neither and an unused variable: the
///   dependency compiles at the compiler's default, so nothing is reported
///   and nothing fails.
/// - The dependency then turns its own warnings on and the consumer turns
///   its off: now the dependency's file is the only one with a warning flag.
#[test]
fn test_warnings_reach_neither_dependencies_nor_dependents() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let lib = tmp.path().join("wdep");
    let app = tmp.path().join("wapp");
    let deps = "[dependencies]\nwdep = { path = \"../wdep\" }\n\n";

    write_lib(&lib, "wdep", "");
    write_app(
        &app,
        "wapp",
        "warnings = \"all\"\nwarnings_as_errors = true\n",
        deps,
        MAIN_USING_DEP,
    );
    let built = build_ok(&home, &app);
    assert!(
        !built.combined().contains("unused_in_dep"),
        "the consumer's `warnings` must not apply to its dependency\n{built}"
    );
    assert_eq!(run_built_exe(&app, "wapp").out(), "42");
    let dep_args = recorded_arguments(&app, "wdep.c");
    assert!(!has_warning_flag(&dep_args), "{dep_args:?}");
    let app_args = recorded_arguments(&app, "main.c");
    assert!(app_args.contains(&all_flag().to_string()), "{app_args:?}");

    write_lib(&lib, "wdep", "warnings = \"all\"\n");
    write_app(&app, "wapp", "", deps, MAIN_USING_DEP);
    let built = build_ok(&home, &app);
    assert!(
        built.combined().contains("unused_in_dep"),
        "a path dependency is code on this machine: its own `warnings` \
         applies when it is built\n{built}"
    );
    let dep_args = recorded_arguments(&app, "wdep.c");
    assert!(dep_args.contains(&all_flag().to_string()), "{dep_args:?}");
    let app_args = recorded_arguments(&app, "main.c");
    assert!(
        !has_warning_flag(&app_args),
        "a dependency's `warnings` must not reach the target that depends on \
         it: {app_args:?}"
    );
}

/// Commit `dir` as a git repository tagged `v1`, and return its `file://` URL.
fn git_fixture(dir: &Path) -> String {
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "fixture"]);
    git(&["tag", "v1"]);
    url::Url::from_file_path(dir).unwrap().to_string()
}

/// A fetched dependency's `warnings` are not its consumer's business.
///
/// The dependency's author develops with `warnings = "pedantic"` and
/// `warnings_as_errors = true`, and the code has a warning (as it will, the
/// day a newer compiler grows one). Fetched over git, it must build quietly:
/// the consumer can neither act on the diagnostics nor fix the failure. The
/// very same tree as a *path* dependency is code being worked on, and gets
/// what its manifest says -- so there the build fails.
#[test]
fn test_a_fetched_dependencys_warnings_are_not_applied() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let lib = tmp.path().join("wdep");
    let app = tmp.path().join("wapp");

    write_lib(
        &lib,
        "wdep",
        "warnings = \"pedantic\"\nwarnings_as_errors = true\n",
    );
    let url = git_fixture(&lib);

    write_app(
        &app,
        "wapp",
        "",
        &format!("[dependencies]\nwdep = {{ git = \"{url}\", tag = \"v1\" }}\n\n"),
        MAIN_USING_DEP,
    );
    let built = build_ok(&home, &app);
    assert!(
        !built.combined().contains("unused_in_dep"),
        "a git dependency's own warning level must not be applied\n{built}"
    );
    assert_eq!(run_built_exe(&app, "wapp").out(), "42");
    let dep_args = recorded_arguments(&app, "wdep.c");
    assert!(!has_warning_flag(&dep_args), "{dep_args:?}");

    let app2 = tmp.path().join("wapp2");
    write_app(
        &app2,
        "wapp2",
        "",
        "[dependencies]\nwdep = { path = \"../wdep\" }\n\n",
        MAIN_USING_DEP,
    );
    let failed = harbour_run(&home, &app2, &["build"]);
    assert!(
        !failed.status.success(),
        "as a path dependency the same package keeps its `warnings_as_errors`\n{failed}"
    );
    assert!(failed.combined().contains("unused_in_dep"), "{failed}");
}

/// What `warnings_as_errors = true` is on the compiler under test.
fn as_errors_flag() -> &'static str {
    if cfg!(target_env = "msvc") {
        "/WX"
    } else {
        "-Werror"
    }
}

/// `wdep` as a git dependency entry, with `extra` keys after the tag.
fn git_dep(url: &str, extra: &str) -> String {
    format!("[dependencies]\nwdep = {{ git = \"{url}\", tag = \"v1\"{extra} }}\n\n")
}

/// The cap on a fetched dependency's warning settings is the default, is
/// reported, and can be lifted two ways -- neither of which touches the
/// lockfile.
///
/// One dependency throughout: `warnings = "all"` with
/// `warnings_as_errors = true`, and a source with an unused local. Whether
/// its own settings were applied is therefore read off whether the build
/// *fails*, which holds on every compiler, and off the recorded command
/// line.
#[test]
fn test_the_dependency_cap_is_reported_and_can_be_lifted() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let lib = tmp.path().join("wdep");
    let app = tmp.path().join("wapp");

    write_lib(
        &lib,
        "wdep",
        "warnings = \"all\"\nwarnings_as_errors = true\n",
    );
    let url = git_fixture(&lib);
    let lockfile = app.join("Harbour.lock");
    let asked = "warnings = \"all\", warnings_as_errors = true";

    // Capped by default: builds, runs, and an ordinary build says nothing.
    write_app(&app, "wapp", "", &git_dep(&url, ""), MAIN_USING_DEP);
    let built = build_ok(&home, &app);
    assert!(
        !built.combined().contains("capped"),
        "a build without `--verbose` must not mention the cap\n{built}"
    );
    assert_eq!(run_built_exe(&app, "wapp").out(), "42");
    assert!(!has_warning_flag(&recorded_arguments(&app, "wdep.c")));
    let locked = fs::read(&lockfile).unwrap();

    // `--verbose` names the package, what it asked for, and both ways out.
    let verbose = harbour_run(&home, &app, &["--verbose", "build"]).success();
    let note = format!("warning settings capped for 1 fetched package: wdep ({asked})");
    assert!(verbose.combined().contains(&note), "{verbose}");
    assert!(
        verbose.combined().contains("`warnings = true`"),
        "{verbose}"
    );
    assert!(verbose.combined().contains("`--dep-warnings`"), "{verbose}");

    // `harbour flags` for the dependency's target: no warning flag, and the
    // reason there is none.
    let flags = |extra: &[&str]| {
        let mut args = vec!["flags", "wdep", "-p", "wdep", "--compile"];
        args.extend_from_slice(extra);
        harbour_run(&home, &app, &args).success()
    };
    let capped = flags(&[]);
    assert!(
        !has_warning_flag(&reported_flags(&capped.stdout)),
        "{capped}"
    );
    assert!(
        capped
            .stdout
            .contains(&format!("# capped: target wdep asks for {asked}")),
        "{capped}"
    );
    assert!(capped.stdout.contains("# to lift:"), "{capped}");

    // `--dep-warnings` lifts it for everything: the dependency's own
    // `warnings_as_errors` now fails the build, on its own unused variable.
    let failed = harbour_run(&home, &app, &["build", "--dep-warnings"]);
    assert!(
        !failed.status.success(),
        "`--dep-warnings` must apply the dependency's `warnings_as_errors`\n{failed}"
    );
    assert!(failed.combined().contains("unused_in_dep"), "{failed}");
    let dep_args = recorded_arguments(&app, "wdep.c");
    assert!(dep_args.contains(&all_flag().to_string()), "{dep_args:?}");
    assert!(
        dep_args.contains(&as_errors_flag().to_string()),
        "{dep_args:?}"
    );
    let lifted = flags(&["--dep-warnings"]);
    assert!(
        reported_flags(&lifted.stdout).contains(&all_flag().to_string()),
        "{lifted}"
    );
    assert!(!lifted.stdout.contains("# capped"), "{lifted}");

    // Without the flag the cap is back, and the build passes again.
    build_ok(&home, &app);

    // The consumer's manifest lifts it for this one dependency.
    write_app(
        &app,
        "wapp",
        "",
        &git_dep(&url, ", warnings = true"),
        MAIN_USING_DEP,
    );
    let failed = harbour_run(&home, &app, &["build"]);
    assert!(
        !failed.status.success(),
        "`warnings = true` on the dependency must apply its `warnings_as_errors`\n{failed}"
    );
    assert!(failed.combined().contains("unused_in_dep"), "{failed}");
    let dep_args = recorded_arguments(&app, "wdep.c");
    assert!(
        dep_args.contains(&as_errors_flag().to_string()),
        "{dep_args:?}"
    );
    let honoured = flags(&[]);
    assert!(
        honoured
            .stdout
            .contains("# from: target wdep (warnings_as_errors)"),
        "{honoured}"
    );
    assert!(!honoured.stdout.contains("# capped"), "{honoured}");
    let verbose = harbour_run(&home, &app, &["--verbose", "build"]);
    assert!(
        !verbose.combined().contains("warning settings capped"),
        "nothing is capped any more\n{verbose}"
    );

    // The key says how a package is compiled, not which package it is: the
    // lockfile is the same file, byte for byte.
    assert_eq!(
        fs::read(&lockfile).unwrap(),
        locked,
        "`warnings = true` on a dependency must not change Harbour.lock"
    );
    write_app(&app, "wapp", "", &git_dep(&url, ""), MAIN_USING_DEP);
    build_ok(&home, &app);
    assert_eq!(fs::read(&lockfile).unwrap(), locked);
}

/// Lifting or applying the cap changes the dependency's compile command, so
/// its objects are rebuilt -- in both directions, and not otherwise.
///
/// `warnings = "all"` alone, so that every build here succeeds.
#[test]
fn test_lifting_the_dependency_cap_recompiles_the_dependency() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let lib = tmp.path().join("wdep");
    let app = tmp.path().join("wapp");

    write_lib(&lib, "wdep", "warnings = \"all\"\n");
    let url = git_fixture(&lib);

    write_app(&app, "wapp", "", &git_dep(&url, ""), MAIN_USING_DEP);
    build_twice(&home, &app).assert_incremental_is_a_no_op();
    assert!(!has_warning_flag(&recorded_arguments(&app, "wdep.c")));

    write_app(
        &app,
        "wapp",
        "",
        &git_dep(&url, ", warnings = true"),
        MAIN_USING_DEP,
    );
    let (log, diff) = rebuild_and_diff(&home, &app);
    diff.assert_touched(
        "wdep",
        "lifting the cap puts the dependency's warning flags on its compile \
         command, so its object must be rebuilt",
    );
    // Output not asserted under MSVC, for the reason given in
    // `test_changing_warnings_recompiles`; the command line below is.
    if !cfg!(target_env = "msvc") {
        assert!(log.combined().contains("unused_in_dep"), "{log}");
    }
    let dep_args = recorded_arguments(&app, "wdep.c");
    assert!(dep_args.contains(&all_flag().to_string()), "{dep_args:?}");
    build_twice(&home, &app).assert_incremental_is_a_no_op();

    // `--dep-warnings` on top of the manifest's opt-in is the same command
    // line, so nothing is rebuilt for it.
    let root = target_dir(&app);
    let before = snapshot_tree(&root);
    harbour_run(&home, &app, &["build", "--dep-warnings"]).success();
    let diff = describe_artifact_changes(&root, &before, &snapshot_tree(&root));
    diff.assert_untouched(
        "wdep",
        "the cap was already lifted for this dependency by the manifest",
    );

    write_app(&app, "wapp", "", &git_dep(&url, ""), MAIN_USING_DEP);
    let (log, diff) = rebuild_and_diff(&home, &app);
    diff.assert_touched(
        "wdep",
        "applying the cap again takes the flags back off the command line",
    );
    assert!(!log.combined().contains("unused_in_dep"), "{log}");
    assert!(!has_warning_flag(&recorded_arguments(&app, "wdep.c")));
}

/// Only the workspace being built can lift the cap.
///
/// `wmid` is a fetched library that depends on `wdep` and writes
/// `warnings = true` on it. Were that honoured, a library could switch
/// diagnostics -- and `-Werror` -- on in its consumer's build, which is what
/// the cap exists to prevent. The root says nothing, so `wdep` stays capped
/// and the build passes; when the root says it too, it is lifted.
#[test]
fn test_a_dependency_cannot_lift_the_cap_for_its_own_dependencies() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let lib = tmp.path().join("wdep");
    let mid = tmp.path().join("wmid");
    let app = tmp.path().join("wapp");

    write_lib(
        &lib,
        "wdep",
        "warnings = \"all\"\nwarnings_as_errors = true\n",
    );
    let dep_url = git_fixture(&lib);

    fs::create_dir_all(mid.join("src")).unwrap();
    fs::create_dir_all(mid.join("include")).unwrap();
    fs::write(mid.join("include/wmid.h"), "int wmid_value(void);\n").unwrap();
    fs::write(
        mid.join("src/wmid.c"),
        "#include \"wmid.h\"\n#include \"wdep.h\"\n\
         int wmid_value(void) { return wdep_value(); }\n",
    )
    .unwrap();
    fs::write(
        mid.join("Harbour.toml"),
        format!(
            "[package]\n\
             name = \"wmid\"\n\
             version = \"1.0.0\"\n\
             \n\
             {}\
             [targets.wmid]\n\
             kind = \"staticlib\"\n\
             sources = [\"src/wmid.c\"]\n\
             \n\
             [targets.wmid.public]\n\
             include_dirs = [\"include\"]\n",
            git_dep(&dep_url, ", warnings = true")
        ),
    )
    .unwrap();
    let mid_url = git_fixture(&mid);

    let main_c = "#include <stdio.h>\n\
         #include \"wmid.h\"\n\
         int main(void) { printf(\"%d\\n\", wmid_value()); return 0; }\n";
    let mid_dep = format!("wmid = {{ git = \"{mid_url}\", tag = \"v1\" }}\n");

    write_app(
        &app,
        "wapp",
        "",
        &format!("[dependencies]\n{mid_dep}\n"),
        main_c,
    );
    let built = harbour_run(&home, &app, &["--verbose", "build"]).success();
    assert_eq!(run_built_exe(&app, "wapp").out(), "42");
    let dep_args = recorded_arguments(&app, "wdep.c");
    assert!(
        !has_warning_flag(&dep_args),
        "`warnings = true` written by `wmid` must not lift the cap in the \
         root's build: {dep_args:?}"
    );
    assert!(
        built
            .combined()
            .contains("warning settings capped for 1 fetched package: wdep ("),
        "{built}"
    );

    // The root naming the transitive dependency itself is the root's call.
    write_app(
        &app,
        "wapp",
        "",
        &format!(
            "[dependencies]\n{mid_dep}wdep = {{ git = \"{dep_url}\", tag = \"v1\", warnings = true }}\n\n"
        ),
        main_c,
    );
    let failed = harbour_run(&home, &app, &["build"]);
    assert!(!failed.status.success(), "{failed}");
    assert!(failed.combined().contains("unused_in_dep"), "{failed}");
}

/// The two `warnings` values a dependency entry refuses, each with the
/// reason: `false` on a package that is never capped, and either value on a
/// vcpkg port that Harbour does not compile.
#[test]
fn test_a_dependency_warnings_key_that_cannot_apply_is_refused() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let lib = tmp.path().join("wdep");
    let app = tmp.path().join("wapp");
    write_lib(&lib, "wdep", "");

    // `true` on a path dependency states what already happens.
    write_app(
        &app,
        "wapp",
        "",
        "[dependencies]\nwdep = { path = \"../wdep\", warnings = true }\n\n",
        MAIN_USING_DEP,
    );
    build_ok(&home, &app);

    write_app(
        &app,
        "wapp",
        "",
        "[dependencies]\nwdep = { path = \"../wdep\", warnings = false }\n\n",
        MAIN_USING_DEP,
    );
    let refused = harbour_run(&home, &app, &["build"]);
    assert!(!refused.status.success(), "{refused}");
    assert!(
        refused
            .combined()
            .contains("`warnings = false` has no effect on a package that is on disk"),
        "{refused}"
    );

    write_app(
        &app,
        "wapp",
        "",
        "[dependencies]\nwdep = { vcpkg = true, warnings = true }\n\n",
        MAIN_USING_DEP,
    );
    let refused = harbour_run(&home, &app, &["build"]);
    assert!(!refused.status.success(), "{refused}");
    assert!(
        refused
            .combined()
            .contains("`warnings` cannot be set on a vcpkg dependency"),
        "{refused}"
    );

    // Not a level: the level is the dependency's own business.
    write_app(
        &app,
        "wapp",
        "",
        "[dependencies]\nwdep = { path = \"../wdep\", warnings = \"all\" }\n\n",
        MAIN_USING_DEP,
    );
    let refused = harbour_run(&home, &app, &["build"]);
    assert!(!refused.status.success(), "{refused}");
    assert!(
        refused
            .combined()
            .contains("`warnings` must be `true` or `false`, not `\"all\"`"),
        "{refused}"
    );
}

/// `harbour flags` lists the warning flags where the compiler gets them --
/// ahead of the profile's flags and the target's `cflags` -- and says which
/// key each came from.
#[test]
fn test_flags_command_reports_warnings_ahead_of_cflags() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let dir = tmp.path().join("warnflags");

    let own = if cfg!(target_env = "msvc") {
        "/wd4189"
    } else {
        "-Wno-unused-variable"
    };
    write_app(
        &dir,
        "warnflags",
        &format!(
            "warnings = \"all\"\nwarnings_as_errors = true\n\n\
             [targets.warnflags.private]\ncflags = [\"{own}\"]\n"
        ),
        "",
        MAIN_WITH_UNUSED_LOCAL,
    );

    let reported = harbour_run(&home, &dir, &["flags", "warnflags", "--compile"]).success();
    let printed = reported_flags(&reported.stdout);
    let as_errors = if cfg!(target_env = "msvc") {
        "/WX"
    } else {
        "-Werror"
    };
    assert!(
        position(&printed, all_flag()) < position(&printed, as_errors)
            && position(&printed, as_errors) < position(&printed, own),
        "{reported}"
    );
    assert!(
        reported
            .stdout
            .contains("# from: target warnflags (warnings)"),
        "{reported}"
    );
    assert!(
        reported
            .stdout
            .contains("# from: target warnflags (warnings_as_errors)"),
        "{reported}"
    );

    // And it is what the build uses: same flags, same order.
    build_ok(&home, &dir);
    let args = recorded_arguments(&dir, "main.c");
    let in_argv: Vec<&String> = args.iter().filter(|a| printed.contains(a)).collect();
    let in_report: Vec<&String> = printed.iter().filter(|f| args.contains(f)).collect();
    assert_eq!(
        in_argv, in_report,
        "the flags both list must be in the same order\n{reported}\n{args:?}"
    );
}

/// Lines of a build's output that came from a compiler, linker or librarian
/// rather than from Harbour: a source file's bare name (what `cl` echoes
/// before compiling it), a linker progress report, or a warning.
fn tool_chatter(log: &RunLog, source_names: &[&str]) -> Vec<String> {
    log.combined()
        .lines()
        .filter(|line| {
            let line = line.trim();
            source_names.contains(&line)
                || line.starts_with("Creating library")
                || line.contains("Generating code")
                || line.contains("generating code")
                || line.contains("warning")
        })
        .map(str::to_string)
        .collect()
}

/// A build with nothing wrong in it says nothing on the tools' behalf.
///
/// The other half of showing MSVC's warnings. `cl` writes them to stdout, and
/// on that same stream it echoes the name of every file it compiles
/// (`main.c`), `/nologo` or not; `link` reports `Generating code` under
/// `/LTCG`. Relaying stdout whole would put one stray line per source file
/// into every build. So: a library and a program, warnings at `all`, built in
/// both profiles (release is the one with LTO), and not one such line.
///
/// Runs everywhere, since "a clean build is quiet" is true of every
/// toolchain; it is MSVC where it could have stopped being true.
#[test]
fn test_a_clean_build_relays_nothing_from_the_tools() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let lib = tmp.path().join("wdep");
    let app = tmp.path().join("wclean");

    write_lib(&lib, "wdep", "warnings = \"all\"\n");
    // `write_lib`'s source has an unused local on purpose; this one has not.
    fs::write(
        lib.join("src/wdep.c"),
        "#include \"wdep.h\"\nint wdep_value(void) { return 42; }\n",
    )
    .unwrap();
    write_app(
        &app,
        "wclean",
        "warnings = \"all\"\n",
        "[dependencies]\nwdep = { path = \"../wdep\" }\n\n",
        MAIN_USING_DEP,
    );

    for args in [&["build"][..], &["build", "--release"][..]] {
        let built = harbour_run(&home, &app, args).success();
        let chatter = tool_chatter(&built, &["main.c", "wdep.c"]);
        assert!(
            chatter.is_empty(),
            "a build with no diagnostics must not relay anything from the \
             compiler or the linker, but these lines appeared: {chatter:#?}\n{built}"
        );
    }
    assert_eq!(run_built_exe(&app, "wclean").out(), "42");
}

/// The same for a DLL, which is where `link.exe` is talkative: an image that
/// exports something gets an import library, announced on stdout as
/// `Creating library x.lib and object x.exp`. The import library existing is
/// the proof the line was printed and then left out.
#[cfg(target_env = "msvc")]
#[test]
fn test_a_clean_dll_build_does_not_announce_its_import_library() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let dir = tmp.path().join("wshared");

    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join("src/wshared.c"),
        "__declspec(dllexport) int wshared_value(void) { return 7; }\n",
    )
    .unwrap();
    fs::write(
        dir.join("Harbour.toml"),
        "[package]\nname = \"wshared\"\nversion = \"0.1.0\"\n\n\
         [targets.wshared]\nkind = \"sharedlib\"\nsources = [\"src/wshared.c\"]\n\
         warnings = \"all\"\n",
    )
    .unwrap();

    for args in [&["build"][..], &["build", "--release"][..]] {
        let built = harbour_run(&home, &dir, args).success();
        let chatter = tool_chatter(&built, &["wshared.c"]);
        assert!(
            chatter.is_empty(),
            "a clean DLL build must not relay the linker's progress lines: \
             {chatter:#?}\n{built}"
        );
    }

    let tree = snapshot_tree(&target_dir(&dir));
    assert!(
        tree.keys()
            .any(|p| p.extension().is_some_and(|e| e == "exp")),
        "the DLL exports a function, so `link` wrote an import library and \
         an export file and said so; none found, so this test proves nothing: \
         {:#?}",
        tree.keys().collect::<Vec<_>>()
    );
}

/// `link.exe` writes its warnings to stdout too, and a link that succeeded
/// used to have them discarded like the compiler's.
///
/// `LNK4044` is the linker's `D9002`: an option it does not know, ignored,
/// and the image linked anyway.
#[cfg(target_env = "msvc")]
#[test]
fn test_linker_warnings_on_a_successful_link_reach_the_user() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let dir = tmp.path().join("wlink");

    write_app(
        &dir,
        "wlink",
        "",
        "[profile.debug]\nldflags = [\"/NOSUCHOPTION\"]\n\n",
        "#include <stdio.h>\nint main(void) { printf(\"ran\\n\"); return 0; }\n",
    );
    let built = build_ok(&home, &dir);
    let out = built.combined();
    assert!(
        out.contains("LNK4044") && out.contains("NOSUCHOPTION"),
        "`link` ignores an option it does not know and says so (LNK4044); the \
         link succeeded, and the warning must still be shown\n{built}"
    );
    assert_eq!(run_built_exe(&dir, "wlink").out(), "ran");
}

/// And `lib.exe`: two objects defining one symbol are archived with a
/// warning (`LNK4006`, second definition ignored), on stdout, from a command
/// that exits 0.
#[cfg(target_env = "msvc")]
#[test]
fn test_librarian_warnings_on_a_successful_archive_reach_the_user() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let dir = tmp.path().join("wlib");

    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join("src/wlib.c"),
        "int wlib_value(void) { return 1; }\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/again.c"),
        "int wlib_value(void) { return 2; }\n",
    )
    .unwrap();
    fs::write(
        dir.join("Harbour.toml"),
        "[package]\nname = \"wlib\"\nversion = \"0.1.0\"\n\n\
         [targets.wlib]\nkind = \"staticlib\"\n\
         sources = [\"src/wlib.c\", \"src/again.c\"]\n",
    )
    .unwrap();

    let built = build_ok(&home, &dir);
    assert!(
        built.combined().contains("LNK4006"),
        "`lib` warns that `wlib_value` is defined twice and the second \
         definition is ignored (LNK4006); the archive was created, and the \
         warning must still be shown\n{built}"
    );
}
