//! There are no build backends to choose between.
//!
//! Harbour builds every target with its native builder; a target that needs
//! other commands (`cmake`, `meson`, `make`, ...) runs them from a
//! `type = "custom"` recipe in its manifest. The menu that used to sit in front
//! of that -- `harbour backend list|show|check`, `harbour build --backend`,
//! `[build] backend` in config -- selected nothing and is gone.
//!
//! Every old spelling is rejected loudly and names the custom recipe; none is
//! silently ignored, and none may go on to build.
#![allow(unused_imports)]

use std::fs;
use std::process::Command;

use assert_cmd::prelude::*;
use predicates::prelude::*;

use crate::common::*;

/// A minimal exe project to run `harbour build` in.
fn new_app(tmp: &tempfile::TempDir, home: &std::path::Path) -> std::path::PathBuf {
    harbour(home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    tmp.path().join("app")
}

/// What both rejections must say: that the thing was removed, that the build
/// is always native, and the recipe to write instead.
fn assert_explains_the_removal(message: &str, run: &dyn std::fmt::Display) {
    assert!(
        message.contains("has been removed"),
        "the error must say the option was removed\n{run}"
    );
    assert!(
        message.contains("always builds natively"),
        "the error must say there is nothing to select\n{run}"
    );
    assert!(
        message.contains("[targets.NAME.recipe]") && message.contains("type = \"custom\""),
        "the error must name the custom recipe\n{run}"
    );
}

/// `harbour build --backend` is an error whatever it is given -- the two names
/// that used to be accepted as much as the two removed earlier -- and nothing
/// is built.
#[test]
fn test_the_backend_flag_is_rejected_whatever_its_value() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let app = new_app(&tmp, &home);

    let spellings: &[&[&str]] = &[
        &["build", "--backend=native"],
        &["build", "--backend=custom"],
        &["build", "--backend=cmake"],
        &["build", "--backend=meson"],
        &["build", "--backend=nonesuch"],
        &["build", "--backend", "native"],
        &["build", "--release", "--backend", "custom", "-j", "1"],
        &["build", "--backend"],
    ];
    for args in spellings {
        let run = harbour_run(&home, &app, args);
        assert!(
            !run.status.success(),
            "`{args:?}` must fail rather than build\n{run}"
        );
        let message = run.combined();
        assert!(
            message.contains("`--backend` has been removed"),
            "`{args:?}` must be refused by name\n{run}"
        );
        assert!(
            !message.contains("unexpected argument"),
            "`{args:?}` must get Harbour's explanation, not clap's bare rejection\n{run}"
        );
        assert_explains_the_removal(&message, &run);
        assert!(
            !app.join(".harbour/target").exists() && !built_exe_path(&app, "app").exists(),
            "nothing may be built after `{args:?}`\n{run}"
        );
    }

    // The same project builds once the flag is dropped.
    let run = harbour_run(&home, &app, &["build"]);
    assert!(run.status.success(), "{run}");
    assert!(built_exe_path(&app, "app").exists(), "{run}");
}

/// `[build] backend` in config is an error whatever its value, names the file
/// that sets it, and stops every command that builds.
#[test]
fn test_the_backend_config_key_is_rejected_whatever_its_value() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let app = new_app(&tmp, &home);
    let config = app.join(".harbour/config.toml");
    fs::create_dir_all(app.join(".harbour")).unwrap();

    for value in ["native", "custom", "cmake", "meson", "nonesuch"] {
        fs::write(&config, format!("[build]\nbackend = \"{value}\"\n")).unwrap();

        for command in ["build", "run", "test"] {
            let run = harbour_run(&home, &app, &[command]);
            assert!(
                !run.status.success(),
                "`harbour {command}` with `backend = \"{value}\"` in config must fail, \
                 not ignore it\n{run}"
            );
            let message = run.combined();
            assert!(
                message.contains("[build] backend") && message.contains(&format!("\"{value}\"")),
                "the error must name the config key and its value\n{run}"
            );
            assert!(
                message.contains("config.toml") && message.contains("delete the `backend` line"),
                "the error must name the file and what to delete from it\n{run}"
            );
            assert_explains_the_removal(&message, &run);
            assert!(
                !built_exe_path(&app, "app").exists(),
                "nothing may be built\n{run}"
            );
        }
    }

    // Deleting the key, as the error says, is all it takes.
    fs::write(&config, "[build]\njobs = 1\n").unwrap();
    let run = harbour_run(&home, &app, &["build"]);
    assert!(run.status.success(), "{run}");
}

/// `type = "cmake"` / `type = "meson"` in a recipe are rejected at manifest
/// load, by name, with the custom recipe to write instead.
#[test]
fn test_removed_recipe_types_are_rejected_in_a_manifest() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let app = new_app(&tmp, &home);

    for removed in ["cmake", "meson"] {
        fs::write(
            app.join("Harbour.toml"),
            format!(
                "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
                 [targets.app]\nkind = \"staticlib\"\n\n\
                 [targets.app.recipe]\ntype = \"{removed}\"\n"
            ),
        )
        .unwrap();

        let run = harbour_run(&home, &app, &["build"]);
        assert!(
            !run.status.success(),
            "`type = \"{removed}\"` must fail\n{run}"
        );
        let message = run.combined();
        assert!(
            message.contains(&format!("`type = \"{removed}\"` has been removed"))
                && message.contains("type = \"custom\""),
            "the error must name the removed type and the replacement\n{run}"
        );
    }
}

/// `harbour backend` is not a command, with or without its old subcommands.
#[test]
fn test_backend_is_not_a_command() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let invocations: &[&[&str]] = &[
        &["backend"],
        &["backend", "list"],
        &["backend", "show", "native"],
        &["backend", "check", "custom"],
    ];
    for args in invocations {
        let run = harbour_run(&home, tmp.path(), args);
        assert!(!run.status.success(), "`harbour {args:?}` must fail\n{run}");
        assert!(
            run.combined().contains("unrecognized subcommand 'backend'"),
            "`harbour {args:?}` must be an unknown command\n{run}"
        );
    }
}

/// The subcommands a help text lists under `Commands:`, without `help`.
fn listed_subcommands(help: &str) -> Vec<String> {
    help.lines()
        .skip_while(|line| line.trim() != "Commands:")
        .skip(1)
        .take_while(|line| !line.trim().is_empty())
        .filter_map(|line| line.split_whitespace().next().map(str::to_string))
        .filter(|name| name != "help")
        .collect()
}

/// No help text offers a backend: not the command list, not `build`'s options
/// (where `--backend` is still parsed, hidden, only to be refused), and not
/// any other command or subcommand.
#[test]
fn test_no_help_text_mentions_a_backend() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    // Walk the whole command tree from what `--help` lists, so a command
    // added later is covered without being named here.
    let mut pending: Vec<Vec<String>> = vec![vec![]];
    let mut visited: Vec<String> = Vec::new();
    while let Some(path) = pending.pop() {
        for flag in ["--help", "-h"] {
            let mut args: Vec<&str> = path.iter().map(String::as_str).collect();
            args.push(flag);
            let run = harbour_run(&home, tmp.path(), &args);
            assert!(run.status.success(), "{run}");
            let text = run.combined();
            assert!(
                !text.to_lowercase().contains("backend"),
                "`harbour {}` must not mention a backend\n{text}",
                args.join(" ")
            );
            if flag == "--help" {
                for sub in listed_subcommands(&text) {
                    let mut child = path.clone();
                    child.push(sub);
                    pending.push(child);
                }
            }
        }
        visited.push(path.join(" "));
    }

    for expected in ["", "build", "ffi bundle", "toolchain override"] {
        assert!(
            visited.iter().any(|v| v == expected),
            "the walk must have reached `harbour {expected}`: {visited:?}"
        );
    }
}

/// Shell completions do not offer the removed command or the removed flag.
#[test]
fn test_completions_do_not_offer_a_backend() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    for shell in ["bash", "zsh", "fish", "powershell", "elvish"] {
        let run = harbour_run(&home, tmp.path(), &["completions", shell]);
        assert!(run.status.success(), "{run}");
        assert!(
            !run.combined().to_lowercase().contains("backend"),
            "`harbour completions {shell}` must not offer a backend"
        );
    }
}

/// The migration recipe documented in MANIFEST.md works as written: a custom
/// step runs a build system through `sh -c` and copies the archive to
/// `$HARBOUR_ARTIFACT_DIR`, and a dependent links against it.
///
/// The build system here is a plain `cc` + `ar` pair so the test does not need
/// cmake installed; the shape (configure, build, copy to the artifact dir) is
/// the one a cmake recipe takes.
///
/// Unix-only: the fixture uses `sh`, `cc` and `ar`.
#[test]
#[cfg(not(windows))]
fn test_migrated_custom_recipe_via_sh_is_consumable_by_a_dependent() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let lib_dir = tmp.path().join("migrated");
    fs::create_dir_all(lib_dir.join("src")).unwrap();
    fs::create_dir_all(lib_dir.join("include")).unwrap();
    fs::write(
        lib_dir.join("src/answer.c"),
        "int migrated_answer(void) { return 7; }\n",
    )
    .unwrap();
    fs::write(
        lib_dir.join("include/migrated.h"),
        "int migrated_answer(void);\n",
    )
    .unwrap();
    fs::write(
        lib_dir.join("Harbour.toml"),
        r#"[package]
name = "migrated"
version = "0.1.0"

[targets.migrated]
kind = "staticlib"

[targets.migrated.recipe]
type = "custom"

[[targets.migrated.recipe.steps]]
program = "sh"
args = [
  "-c",
  "mkdir -p build && cc -c src/answer.c -o build/answer.o && ar rcs build/libmigrated.a build/answer.o && mkdir -p \"$HARBOUR_ARTIFACT_DIR\" && cp build/libmigrated.a \"$HARBOUR_ARTIFACT_DIR/\"",
]

[targets.migrated.surface.compile.public]
include_dirs = ["include"]
"#,
    )
    .unwrap();

    let app = new_app(&tmp, &home);
    harbour(&home)
        .args(["add", "migrated", "--path", "../migrated"])
        .current_dir(&app)
        .assert()
        .success();
    fs::write(
        app.join("src/main.c"),
        "#include <stdio.h>\n#include \"migrated.h\"\n\n\
         int main(void) {\n    printf(\"%d\\n\", migrated_answer());\n    return 0;\n}\n",
    )
    .unwrap();

    let run = harbour_run(&home, &app, &["build"]);
    assert!(run.status.success(), "{run}");

    let exe = built_exe_path(&app, "app");
    let out = Command::new(&exe).output().unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "7",
        "the archive copied to $HARBOUR_ARTIFACT_DIR must reach the dependent's link line"
    );
}
