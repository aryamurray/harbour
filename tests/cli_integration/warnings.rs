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
    // Not asserted under MSVC: `cl` writes file-level diagnostics to stdout,
    // and Harbour relays only stderr from a compile that succeeded (see
    // `compiler_warnings_on_a_successful_build_reach_the_user`). The flag on
    // the command line, asserted next, holds everywhere.
    if !cfg!(target_env = "msvc") {
        assert!(
            first.combined().contains("unused_here"),
            "`warnings = \"all\"` must report the unused variable\n{first}"
        );
    }
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
    // Output not asserted under MSVC, for the reason given in
    // `test_changing_warnings_recompiles`; the command line below is.
    if !cfg!(target_env = "msvc") {
        assert!(
            built.combined().contains("unused_in_dep"),
            "a path dependency is code on this machine: its own `warnings` \
             applies when it is built\n{built}"
        );
    }
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
