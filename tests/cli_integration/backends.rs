//! Backend selection: `native` and `custom` are the only backends.
//!
//! CMake and Meson used to be dedicated backends (a `type = "cmake"` recipe,
//! `--backend=cmake`, `[build] backend = "cmake"` in config). They were removed
//! in favour of the one escape hatch, the `custom` recipe, which runs whatever
//! commands it is given -- including `cmake` or `meson`. Every old spelling is
//! rejected loudly and names that replacement; none may fall back to building
//! natively.
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

/// `--backend=cmake` and `--backend=meson` are errors that name the custom
/// recipe, and the build does not go ahead natively.
#[test]
fn test_removed_backends_are_rejected_on_the_command_line() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let app = new_app(&tmp, &home);

    for removed in ["cmake", "meson"] {
        let run = harbour_run(&home, &app, &["build", &format!("--backend={removed}")]);
        assert!(
            !run.status.success(),
            "`--backend={removed}` must fail rather than build natively\n{run}"
        );
        let message = run.combined();
        assert!(
            message.contains("has been removed") && message.contains("type = \"custom\""),
            "the error must name the custom recipe that replaces `{removed}`\n{run}"
        );
        assert!(
            !built_exe_path(&app, "app").exists(),
            "nothing may be built after a rejected backend\n{run}"
        );
    }
}

/// `[build] backend = "cmake"` in `.harbour/config.toml` used to be parsed with
/// `.ok()` and so ignored if it was not a known name; now it is an error.
#[test]
fn test_removed_backend_in_config_is_rejected() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let app = new_app(&tmp, &home);

    fs::create_dir_all(app.join(".harbour")).unwrap();
    fs::write(
        app.join(".harbour/config.toml"),
        "[build]\nbackend = \"cmake\"\n",
    )
    .unwrap();

    let run = harbour_run(&home, &app, &["build"]);
    assert!(
        !run.status.success(),
        "a removed backend in config must fail, not be ignored\n{run}"
    );
    let message = run.combined();
    assert!(
        message.contains("[build] backend") && message.contains("type = \"custom\""),
        "the error must name the config key and the replacement\n{run}"
    );
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

/// `harbour backend list` shows exactly the two backends that exist, and
/// `show`/`check` on a removed one say what replaced it.
#[test]
fn test_backend_commands_know_only_native_and_custom() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let list = harbour_run(&home, tmp.path(), &["backend", "list"]);
    assert!(list.status.success(), "{list}");
    let text = list.combined();
    assert!(text.contains("native - "), "{text}");
    assert!(text.contains("custom - "), "{text}");
    assert!(
        !text.to_lowercase().contains("cmake") && !text.to_lowercase().contains("meson"),
        "removed backends must not be listed\n{text}"
    );

    for sub in ["show", "check"] {
        let run = harbour_run(&home, tmp.path(), &["backend", sub, "meson"]);
        assert!(
            !run.status.success(),
            "`backend {sub} meson` must fail\n{run}"
        );
        assert!(
            run.combined().contains("type = \"custom\""),
            "`backend {sub} meson` must name the replacement\n{run}"
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
