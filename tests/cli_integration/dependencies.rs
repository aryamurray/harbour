//! Dependency graphs end to end: path and git dependencies, transitive, diamond, and recipes consumed by a dependent.
#![allow(unused_imports)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::prelude::*;
use predicates::prelude::*;
use tempfile::TempDir;

use harbour::test_support::fixtures::local_registry;

use crate::common::*;

#[test]
fn test_full_workflow_with_dependency() {
    // Regression coverage for the "linkplan lists the archive but the
    // actual link command doesn't" bug: the dependency exposes a real
    // *function* (not just a macro), the app *calls* it, and the test runs
    // the resulting binary and asserts on its output. A build that links
    // successfully but produces a binary that computes the wrong thing (or
    // a build that fails to link the dependency archive at all, as
    // happened before this fix) must fail this test.
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    // 1. Create a library
    harbour(&home)
        .args(["new", "myutil", "--lib"])
        .current_dir(tmp.path())
        .assert()
        .success();

    let lib_dir = tmp.path().join("myutil");

    // Update manifest to expose include dir
    fs::write(
        lib_dir.join("Harbour.toml"),
        r#"[package]
name = "myutil"
version = "0.1.0"

[targets.myutil]
kind = "staticlib"
sources = ["src/**/*.c"]

[targets.myutil.surface.compile.public]
include_dirs = ["include"]
"#,
    )
    .unwrap();

    // Add header declaring a real function, not just a macro -- the
    // original version of this test only exercised the include path, never
    // the linker, and passed even while dependency archives were silently
    // dropped from the link command.
    fs::create_dir_all(lib_dir.join("include")).unwrap();
    fs::write(
        lib_dir.join("include/myutil.h"),
        r#"#ifndef MYUTIL_H
#define MYUTIL_H
int myutil_double(int x);
#endif
"#,
    )
    .unwrap();
    fs::write(
        lib_dir.join("src/lib.c"),
        r#"#include "myutil.h"

int myutil_double(int x) {
    return x * 2;
}
"#,
    )
    .unwrap();

    // 2. Create an application that uses the library
    harbour(&home)
        .args(["new", "myapp"])
        .current_dir(tmp.path())
        .assert()
        .success();

    let app_dir = tmp.path().join("myapp");

    // 3. Add the library as a dependency
    //
    // NOTE: this uses a path dependency, so it never touches the registry
    // or the network. If this test is ever seen failing on Windows CI, that
    // is tracked as the same pre-existing, unrelated CLI exit-code defect
    // referenced in `test_add_registry_dependency` above -- do not paper
    // over it here either.
    harbour(&home)
        .args(["add", "myutil", "--path", "../myutil"])
        .current_dir(&app_dir)
        .assert()
        .success();

    // 4. Update the app to call the library's function and print the result.
    fs::write(
        app_dir.join("src/main.c"),
        r#"#include <stdio.h>
#include "myutil.h"

int main(void) {
    printf("%d\n", myutil_double(21));
    return 0;
}
"#,
    )
    .unwrap();

    // 5. Check the dependency tree
    harbour(&home)
        .args(["tree"])
        .current_dir(&app_dir)
        .assert()
        .success()
        .stdout(predicate::str::contains("myapp"))
        .stdout(predicate::str::contains("myutil"));

    // 6. Check flags show the dependency's include path
    harbour(&home)
        .args(["flags", "myapp"])
        .current_dir(&app_dir)
        .assert()
        .success()
        .stdout(predicate::str::contains("myutil"));

    // 7. Check linkplan shows the dependency
    harbour(&home)
        .args(["linkplan", "myapp"])
        .current_dir(&app_dir)
        .assert()
        .success()
        .stdout(predicate::str::contains("myutil"));

    // 8. Build the application. This must actually link `libmyutil.a` into
    // `myapp` -- not just resolve its include path.
    harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .success()
        .stderr(predicate::str::contains("Finished"));

    // 9. Verify outputs exist
    let target_dir = app_dir.join(".harbour").join("target").join("debug");
    assert!(target_dir.exists());

    // 10. Run the built binary and check its actual output. This is the
    // assertion that catches both "didn't link at all" (the binary
    // wouldn't exist / build would have failed at step 8) and "linked but
    // computed the wrong thing" (wrong output here).
    let exe = built_exe_path(&app_dir, "myapp");
    assert!(exe.exists(), "built executable not found at {exe:?}");
    let output = Command::new(&exe).output().unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "42");
}

/// Transitive dependency: `app -> libb -> liba`, where `libb` calls into
/// `liba`. This is the scenario from the bug report's "second, related
/// bug" (link order): without the fix, `linkplan` emitted `liba` before
/// `libb`, which is backwards for static linking (`liba` gets linked
/// before anything has asked it to resolve `_liba_answer`, so a
/// traditional left-to-right static linker never pulls its objects in).
///
/// `app` declares only `libb`; `liba` is reached transitively. That is the
/// point of the test as much as the linking is -- an earlier version had to
/// declare `liba` on `app` as well, because only root-declared path
/// dependencies were resolvable.
#[test]
fn test_transitive_dependency_links_and_runs() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    // liba: leaf static library.
    harbour(&home)
        .args(["new", "liba", "--lib"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let liba_dir = tmp.path().join("liba");
    fs::write(
        liba_dir.join("Harbour.toml"),
        r#"[package]
name = "liba"
version = "0.1.0"

[targets.liba]
kind = "staticlib"
sources = ["src/**/*.c"]

[targets.liba.surface.compile.public]
include_dirs = ["include"]
"#,
    )
    .unwrap();
    fs::create_dir_all(liba_dir.join("include")).unwrap();
    fs::write(
        liba_dir.join("include/liba.h"),
        r#"#ifndef LIBA_H
#define LIBA_H
int liba_answer(void);
#endif
"#,
    )
    .unwrap();
    fs::write(
        liba_dir.join("src/lib.c"),
        r#"#include "liba.h"

int liba_answer(void) {
    return 42;
}
"#,
    )
    .unwrap();

    // libb: static library that calls into liba.
    harbour(&home)
        .args(["new", "libb", "--lib"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let libb_dir = tmp.path().join("libb");
    fs::write(
        libb_dir.join("Harbour.toml"),
        r#"[package]
name = "libb"
version = "0.1.0"

[targets.libb]
kind = "staticlib"
sources = ["src/**/*.c"]

[targets.libb.surface.compile.public]
include_dirs = ["include"]
"#,
    )
    .unwrap();
    fs::create_dir_all(libb_dir.join("include")).unwrap();
    fs::write(
        libb_dir.join("include/libb.h"),
        r#"#ifndef LIBB_H
#define LIBB_H
int libb_double_answer(void);
#endif
"#,
    )
    .unwrap();
    fs::write(
        libb_dir.join("src/lib.c"),
        r#"#include "libb.h"
#include "liba.h"

int libb_double_answer(void) {
    return liba_answer() * 2;
}
"#,
    )
    .unwrap();
    harbour(&home)
        .args(["add", "liba", "--path", "../liba"])
        .current_dir(&libb_dir)
        .assert()
        .success();

    // app: depends on libb only. liba arrives transitively.
    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");
    harbour(&home)
        .args(["add", "libb", "--path", "../libb"])
        .current_dir(&app_dir)
        .assert()
        .success();
    fs::write(
        app_dir.join("src/main.c"),
        r#"#include <stdio.h>
#include "libb.h"

int main(void) {
    printf("%d\n", libb_double_answer());
    return 0;
}
"#,
    )
    .unwrap();

    harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .success()
        .stderr(predicate::str::contains("Finished"));

    let exe = built_exe_path(&app_dir, "app");
    assert!(exe.exists(), "built executable not found at {exe:?}");
    let output = Command::new(&exe).output().unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "84");
}

/// Diamond dependency shape: `app -> b -> d` and `app -> c -> d`. `d` must
/// be linked exactly once, positioned after both `b` and `c` on the link
/// line, and the computed result must be correct -- catching both
/// "dropped" (missing symbol at link time) and "duplicated" (which some
/// naive link-order fixes could produce for a diamond) failure modes.
///
/// Same shape as
/// `test_transitive_dependency_links_and_runs` above: `d` is also declared
/// directly on `app` until the resolver fix lands.
#[test]
fn test_diamond_dependency_links_once() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let make_lib = |name: &str, header_body: &str, source_body: &str| {
        harbour(&home)
            .args(["new", name, "--lib"])
            .current_dir(tmp.path())
            .assert()
            .success();
        let dir = tmp.path().join(name);
        fs::write(
            dir.join("Harbour.toml"),
            format!(
                r#"[package]
name = "{name}"
version = "0.1.0"

[targets.{name}]
kind = "staticlib"
sources = ["src/**/*.c"]

[targets.{name}.surface.compile.public]
include_dirs = ["include"]
"#
            ),
        )
        .unwrap();
        fs::create_dir_all(dir.join("include")).unwrap();
        fs::write(dir.join(format!("include/{name}.h")), header_body).unwrap();
        fs::write(dir.join("src/lib.c"), source_body).unwrap();
        dir
    };

    // d: the shared tail of the diamond.
    make_lib(
        "libd",
        "#ifndef LIBD_H\n#define LIBD_H\nint libd_value(void);\n#endif\n",
        "#include \"libd.h\"\n\nint libd_value(void) {\n    return 7;\n}\n",
    );

    // b: app -> b -> d
    let libb_dir = make_lib(
        "libb",
        "#ifndef LIBB_H\n#define LIBB_H\nint libb_via_d(void);\n#endif\n",
        "#include \"libb.h\"\n#include \"libd.h\"\n\nint libb_via_d(void) {\n    return libd_value() + 1;\n}\n",
    );
    harbour(&home)
        .args(["add", "libd", "--path", "../libd"])
        .current_dir(&libb_dir)
        .assert()
        .success();

    // c: app -> c -> d
    let libc_dir = make_lib(
        "libc",
        "#ifndef LIBC_H\n#define LIBC_H\nint libc_via_d(void);\n#endif\n",
        "#include \"libc.h\"\n#include \"libd.h\"\n\nint libc_via_d(void) {\n    return libd_value() * 10;\n}\n",
    );
    harbour(&home)
        .args(["add", "libd", "--path", "../libd"])
        .current_dir(&libc_dir)
        .assert()
        .success();

    // app depends on b and c only. d arrives transitively through both.
    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");
    for dep in ["libb", "libc"] {
        harbour(&home)
            .args(["add", dep, "--path", &format!("../{dep}")])
            .current_dir(&app_dir)
            .assert()
            .success();
    }
    fs::write(
        app_dir.join("src/main.c"),
        r#"#include <stdio.h>
#include "libb.h"
#include "libc.h"

int main(void) {
    printf("%d\n", libb_via_d() + libc_via_d());
    return 0;
}
"#,
    )
    .unwrap();

    harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .success()
        .stderr(predicate::str::contains("Finished"));

    let exe = built_exe_path(&app_dir, "app");
    assert!(exe.exists(), "built executable not found at {exe:?}");
    let output = Command::new(&exe).output().unwrap();
    assert!(output.status.success());
    // (7 + 1) + (7 * 10) = 78. If `d` were duplicated or dropped from the
    // link line, this would either fail to link or (in principle, if a
    // buggy dedup silently discarded one of the sibling libraries instead
    // of `d`) produce a different number.
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "78");
}

/// A dependency whose archive name collides with a system library must not
/// shadow it.
///
/// A package named `c` builds `libc.a`. While Harbour also put each
/// dependency's artifact directory on the linker search path, that `-L`
/// applied to the libraries the compiler driver links implicitly, so the
/// fixture's `libc.a` won over the real C library and the link died on
/// `__libc_start_main` and `printf`. Passing the archive by absolute path
/// with no matching `-L` is what fixes it. The name is the point of the
/// test, not incidental.
#[test]
fn test_dependency_named_c_does_not_shadow_libc() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let lib = tmp.path().join("c");
    fs::create_dir_all(lib.join("src")).unwrap();
    fs::create_dir_all(lib.join("include")).unwrap();
    fs::write(lib.join("include/c.h"), "int c_value(void);\n").unwrap();
    fs::write(lib.join("src/lib.c"), "int c_value(void) { return 7; }\n").unwrap();
    fs::write(
        lib.join("Harbour.toml"),
        r#"[package]
name = "c"
version = "0.1.0"

[targets.c]
kind = "staticlib"
sources = ["src/**/*.c"]

[targets.c.surface.compile.public]
include_dirs = ["include"]
"#,
    )
    .unwrap();

    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");
    harbour(&home)
        .args(["add", "c", "--path", "../c"])
        .current_dir(&app_dir)
        .assert()
        .success();

    // `printf` matters: it is resolved from the real libc, which is what the
    // fixture's `libc.a` used to displace.
    fs::write(
        app_dir.join("src/main.c"),
        r#"#include <stdio.h>
#include "c.h"

int main(void) {
    printf("%d\n", c_value());
    return 0;
}
"#,
    )
    .unwrap();

    harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .success()
        .stderr(predicate::str::contains("Finished"));

    let exe = built_exe_path(&app_dir, "app");
    let out = Command::new(&exe).output().unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "7");
}

/// A library built by a custom recipe can be consumed by a dependent.
///
/// It could not before: the dependent's link line points at
/// `deps/<pkg>-<ver>/lib/lib<target>.a` and nothing told the recipe where
/// that was, so the escape hatch only worked for a root package nobody
/// depended on. `HARBOUR_ARTIFACT_DIR` closes that gap.
///
/// Unix-only: the fixture drives `make`, `cc` and `ar`.
#[test]
#[cfg(not(windows))]
fn test_custom_recipe_library_is_consumable_by_a_dependent() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let lib_dir = tmp.path().join("foreign");
    fs::create_dir_all(lib_dir.join("src")).unwrap();
    fs::create_dir_all(lib_dir.join("include")).unwrap();
    fs::write(
        lib_dir.join("src/answer.c"),
        "int foreign_answer(void) { return 42; }\n",
    )
    .unwrap();
    fs::write(
        lib_dir.join("include/foreign.h"),
        "int foreign_answer(void);\n",
    )
    .unwrap();
    // Tabs matter to make.
    fs::write(
        lib_dir.join("Makefile"),
        "all:\n\tcc -c src/answer.c -o answer.o\n\tar rcs libforeign.a answer.o\n\
         \tmkdir -p \"$(HARBOUR_ARTIFACT_DIR)\"\n\
         \tcp libforeign.a \"$(HARBOUR_ARTIFACT_DIR)/libforeign.a\"\n",
    )
    .unwrap();
    fs::write(
        lib_dir.join("Harbour.toml"),
        r#"[package]
name = "foreign"
version = "0.1.0"

[targets.foreign]
kind = "staticlib"

[targets.foreign.recipe]
type = "custom"

[[targets.foreign.recipe.steps]]
program = "make"
args = ["all"]
cwd = "."
outputs = ["libforeign.a"]

[targets.foreign.surface.compile.public]
include_dirs = ["include"]
"#,
    )
    .unwrap();

    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");
    harbour(&home)
        .args(["add", "foreign", "--path", "../foreign"])
        .current_dir(&app_dir)
        .assert()
        .success();
    fs::write(
        app_dir.join("src/main.c"),
        r#"#include <stdio.h>
#include "foreign.h"

int main(void) {
    printf("%d\n", foreign_answer());
    return 0;
}
"#,
    )
    .unwrap();

    harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .success()
        .stderr(predicate::str::contains("Finished"));

    let exe = built_exe_path(&app_dir, "app");
    let out = Command::new(&exe).output().unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "42",
        "the recipe's archive must reach the dependent's link line"
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
    // `Url` rather than string-building: a Windows path has backslashes and a
    // drive letter, and neither survives pasting into a TOML string.
    url::Url::from_file_path(dir).unwrap().to_string()
}

/// A git dependency's own git dependency is resolved, fetched, built and
/// linked.
///
/// `app -> mid -> base`, every edge a `git` source, so `base` is reachable
/// only by cloning `mid` and reading *its* manifest. A resolver that walked
/// transitive path dependencies but not git ones would fail to find `base`.
/// Found by hand-building libpng (which needs zlib) and kept here because
/// it is the shape every real multi-package project has.
///
/// The answer is asserted from the running binary: `mid`'s function calls
/// `base`'s, so the number only comes out right if both archives were built
/// and linked, and `mid.h` includes `base.h`, so `app` only compiles if
/// `base`'s include directory reached a package two edges away.
#[test]
fn test_a_git_dependency_of_a_git_dependency_is_built_and_linked() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let base = tmp.path().join("base");
    fs::create_dir_all(base.join("include")).unwrap();
    fs::create_dir_all(base.join("src")).unwrap();
    fs::write(
        base.join("Harbour.toml"),
        r#"[package]
name = "base"
version = "1.0.0"

[targets.base]
kind = "staticlib"
sources = ["src/base.c"]

[targets.base.surface.compile.public]
include_dirs = ["include"]
"#,
    )
    .unwrap();
    fs::write(base.join("include/base.h"), "int base_value(void);\n").unwrap();
    fs::write(
        base.join("src/base.c"),
        "#include \"base.h\"\nint base_value(void) { return 40; }\n",
    )
    .unwrap();
    let base_url = git_fixture(&base);

    let mid = tmp.path().join("mid");
    fs::create_dir_all(mid.join("include")).unwrap();
    fs::create_dir_all(mid.join("src")).unwrap();
    fs::write(
        mid.join("Harbour.toml"),
        format!(
            r#"[package]
name = "mid"
version = "1.0.0"

[dependencies]
base = {{ git = "{base_url}", tag = "v1" }}

[targets.mid]
kind = "staticlib"
sources = ["src/mid.c"]

[targets.mid.surface.compile.public]
include_dirs = ["include"]
"#
        ),
    )
    .unwrap();
    fs::write(
        mid.join("include/mid.h"),
        "#include \"base.h\"\nint mid_value(void);\n",
    )
    .unwrap();
    fs::write(
        mid.join("src/mid.c"),
        "#include \"mid.h\"\nint mid_value(void) { return base_value() + 2; }\n",
    )
    .unwrap();
    let mid_url = git_fixture(&mid);

    let app = tmp.path().join("app");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(
        app.join("Harbour.toml"),
        format!(
            r#"[package]
name = "app"
version = "0.1.0"

[dependencies]
mid = {{ git = "{mid_url}", tag = "v1" }}

[targets.app]
kind = "exe"
sources = ["src/main.c"]
"#
        ),
    )
    .unwrap();
    fs::write(
        app.join("src/main.c"),
        "#include <stdio.h>\n#include \"mid.h\"\n\
         int main(void) { printf(\"%d\\n\", mid_value()); return 0; }\n",
    )
    .unwrap();

    harbour_run(&home, &app, &["build"]).success();

    let output = Command::new(built_exe_path(&app, "app")).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "42",
        "the binary must link both git dependencies in the chain"
    );
}
