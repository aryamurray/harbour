//! Workspaces: `-p`, members as dependencies, inherited path dependencies.
#![allow(unused_imports)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::prelude::*;
use predicates::prelude::*;
use tempfile::TempDir;

use harbour::test_support::fixtures::local_registry;

use crate::common::*;

/// `optional` in `[workspace.dependencies]` fails the build, naming the
/// entry and where the key belongs instead.
///
/// Optionality is not a property of the dependency; it is a property of the
/// relationship between one package and it, and it only means anything
/// alongside that package's `[features]` table -- which is the member's.
/// Inheriting it would have given one field two readers that disagree:
/// `resolve_dependency` has workspace context and would prune the
/// dependency, while `surface_resolver::optional_dependency_names` reads the
/// member's raw spec, where `{ workspace = true }` says nothing about
/// optionality -- so the member's `[features]` could not switch back on the
/// dependency the workspace had made optional.
#[test]
fn test_optional_in_workspace_dependencies_is_refused() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let root = tmp.path().join("wsopt");
    fs::create_dir_all(root.join("member/src")).unwrap();
    fs::write(
        root.join("member/src/main.c"),
        "int main(void) { return 0; }\n",
    )
    .unwrap();
    fs::write(
        root.join("member/Harbour.toml"),
        r#"[package]
name = "member"
version = "0.1.0"

[targets.member]
kind = "exe"
sources = ["src/main.c"]
"#,
    )
    .unwrap();
    fs::write(
        root.join("Harbour.toml"),
        r#"[workspace]
members = ["member"]

[workspace.dependencies]
zzws = { version = "1.0", optional = true }
"#,
    )
    .unwrap();

    let run = harbour_run(&home, &root, &["build"]);
    assert!(
        !run.status.success(),
        "`optional` in `[workspace.dependencies]` must fail the build\n{run}"
    );
    let message = run.combined();
    assert!(
        message.contains("`optional` cannot be set here"),
        "the diagnostic must say the key is in the wrong table\n{run}"
    );
    assert!(message.contains("zzws"), "and name the entry\n{run}");
    assert!(
        message.contains("workspace = true, optional = true"),
        "and show where it goes instead\n{run}"
    );

    // Without the key, the same workspace builds.
    fs::write(
        root.join("Harbour.toml"),
        r#"[workspace]
members = ["member"]

[workspace.dependencies]
zzws = { version = "1.0" }
"#,
    )
    .unwrap();
    harbour_run(&home, &root, &["build"]).success();
}

// `workspace = true` with a `path` (#133)
//
// Three compounding bugs, all of which had to go for the feature to have any
// working spelling at all:
//
// 1. the workspace-level `path` was anchored at the *member* directory, so
//    `path = "vendored"` in the root's manifest meant `<root>/app/vendored`;
// 2. inheritance then failed anyway, because `Package::summary` went through
//    a second, context-free route from `DependencySpec` to `Dependency`
//    that had never heard of `[workspace.dependencies]`;
// 3. and running from inside the member did not find the parent workspace,
//    so the table one directory up was invisible rather than mis-anchored.
//
// These two tests run the produced binary and assert what it prints. That is
// the only thing that distinguishes "resolved, compiled, archived and
// linked" from "the build reported success": a fix that resolved the
// dependency without linking it would still exit zero and produce an `app`
// that does not run.
/// Write the #133 fixture: a virtual workspace whose only member inherits a
/// path dependency on a package that is *not* itself a member.
///
/// `vendored` is deliberately outside `members`, because when it is a member
/// the local-first rule matches it first and the workspace entry is never
/// consulted -- a different case, now refused outright (see
/// `validate_workspace_deps_do_not_name_members`).
fn write_workspace_path_inheritance_fixture(root: &Path) {
    fs::create_dir_all(root.join("app").join("src")).unwrap();
    fs::create_dir_all(root.join("vendored").join("src")).unwrap();

    fs::write(
        root.join("Harbour.toml"),
        r#"[workspace]
members = ["app"]

[workspace.dependencies]
vendored = { path = "vendored" }
"#,
    )
    .unwrap();

    fs::write(
        root.join("app").join("Harbour.toml"),
        r#"[package]
name = "app"
version = "0.1.0"

[dependencies]
vendored = { workspace = true }

[targets.app]
kind = "bin"
sources = ["src/**/*.c"]

[targets.app.deps]
vendored = "vendored"
"#,
    )
    .unwrap();

    fs::write(
        root.join("vendored").join("Harbour.toml"),
        r#"[package]
name = "vendored"
version = "0.1.0"

[targets.vendored]
kind = "staticlib"
sources = ["src/**/*.c"]
"#,
    )
    .unwrap();

    fs::write(
        root.join("vendored").join("src").join("v.c"),
        "int vendored_value(void) { return 133; }\n",
    )
    .unwrap();

    fs::write(
        root.join("app").join("src").join("main.c"),
        "#include <stdio.h>\nint vendored_value(void);\n\
         int main(void) { printf(\"%d\\n\", vendored_value()); return 0; }\n",
    )
    .unwrap();
}

/// Bugs 1 and 2, from the workspace root.
///
/// `path = "vendored"` is the only spelling that makes sense from the
/// manifest it is written in, and it is the spelling that used to fail with
/// "path does not exist: .../app/vendored". The one that got *past* that
/// check, `path = "../vendored"`, then hit "dependency `vendored` must
/// specify `path`, `git`, `registry`, `vcpkg`, or `version`" -- so there was
/// no working spelling at all.
#[test]
fn test_workspace_inherited_path_dependency_links_and_runs() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let root = tmp.path().join("ws");
    write_workspace_path_inheritance_fixture(&root);

    build_ok(&home, &root);

    // The archive has to exist on disk; a resolve that succeeded without
    // building the dependency would leave the link to fail later, or
    // silently drop it.
    let lib = target_dir(&root)
        .join("debug")
        .join("deps")
        .join("vendored-0.1.0")
        .join("lib");
    assert!(
        lib.exists(),
        "expected the inherited dependency's archive under {}, but the \
         directory is not there",
        lib.display()
    );

    let run = run_built_exe(&root, "app");
    assert_eq!(
        run.out(),
        "133",
        "the inherited dependency must actually be linked in, not merely \
         resolved\n{run}"
    );
}

/// Bug 3: the same fixture, built from inside the member directory.
///
/// Cargo walks up to the workspace root; Harbour did not, so
/// `[workspace.dependencies]` was invisible and the error blamed the
/// member's manifest for something declared one directory up.
///
/// The second assertion pins the mechanism rather than the outcome: the
/// artifacts must land in the *workspace root's* target directory, because
/// that is what proves the workspace was found, rather than the member
/// being built as a standalone project that happened to resolve. Paths are
/// built with `Path::join`, never with embedded separators.
#[test]
fn test_workspace_inherited_path_dependency_resolves_from_a_member_directory() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let root = tmp.path().join("ws");
    write_workspace_path_inheritance_fixture(&root);

    let app = root.join("app");
    let run = harbour_run(&home, &app, &["build"]).success();

    assert!(
        !app.join(".harbour").exists(),
        "building from a member must use the workspace root's target \
         directory, not create one inside the member\n{run}"
    );

    let exe = run_built_exe(&root, "app");
    assert_eq!(
        exe.out(),
        "133",
        "a build started from the member directory must produce the same \
         linked binary as one started from the root\n{exe}"
    );
}

/// A two-member workspace whose members share nothing: `app` prints `app`,
/// `other` prints `other`, and neither depends on the other.
///
/// The independence is the point. A member that is also a dependency was
/// always built, which is why this bug survived: every workspace in the test
/// suite had one member.
fn write_two_member_workspace(root: &std::path::Path) {
    fs::write(
        root.join("Harbour.toml"),
        "[workspace]\nmembers = [\"app\", \"other\"]\n",
    )
    .unwrap();
    for name in ["app", "other"] {
        let dir = root.join(name);
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(
            dir.join("Harbour.toml"),
            format!(
                "[package]\n\
                 name = \"{name}\"\n\
                 version = \"0.1.0\"\n\
                 \n\
                 [targets.{name}]\n\
                 kind = \"bin\"\n\
                 sources = [\"src/*.c\"]\n"
            ),
        )
        .unwrap();
        fs::write(
            dir.join("src/main.c"),
            format!("#include <stdio.h>\nint main(void) {{ puts(\"{name}\"); return 0; }}\n"),
        )
        .unwrap();
    }
}

/// Run a binary from a build tree, asserting it exists and returning what it
/// printed. `subdir` is the per-root output directory a multi-root build
/// uses (`""` for a single root, which writes straight to `target/debug`).
fn run_workspace_exe(root: &std::path::Path, subdir: &str, name: &str) -> String {
    let mut path = root.join(".harbour/target/debug");
    if !subdir.is_empty() {
        path = path.join(subdir);
    }
    let mut exe = path.join("bin").join(name);
    if !exe.exists() {
        exe = path.join("bin").join(format!("{name}.exe"));
    }
    assert!(
        exe.exists(),
        "expected a built executable at {}; the build reported success \
         without producing it",
        exe.display()
    );
    let out = Command::new(&exe).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Whether any executable named `name` exists anywhere under the build tree.
///
/// Searched over the whole tree rather than at one path because the point is
/// "this package was not built at all", and a package can legitimately land
/// in `deps/` or in a per-root subdirectory.
fn workspace_built_anything_named(root: &std::path::Path, name: &str) -> bool {
    fn walk(dir: &std::path::Path, name: &str) -> bool {
        let Ok(entries) = fs::read_dir(dir) else {
            return false;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if walk(&path, name) {
                    return true;
                }
            } else if let Some(file) = path.file_name().and_then(|f| f.to_str()) {
                if file == name || file == format!("{name}.exe") {
                    return true;
                }
            }
        }
        false
    }
    walk(&root.join(".harbour"), name)
}

#[test]
fn build_dash_p_builds_the_package_it_names() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let root = tmp.path().join("ws");
    fs::create_dir_all(&root).unwrap();
    write_two_member_workspace(&root);

    harbour_run(&home, &root, &["build", "-p", "other"]).success();

    // One root, so it writes straight to `target/debug/bin`.
    assert_eq!(
        run_workspace_exe(&root, "", "other"),
        "other",
        "`-p other` must build and link `other`"
    );

    // And the other member must not have been built. This is the assertion
    // that fails against the old behaviour: it built `app` and nothing else,
    // while announcing `other`.
    assert!(
        !workspace_built_anything_named(&root, "app"),
        "`-p other` must not build `app`; nothing in the build tree may be \
         an artifact of a package that was not selected"
    );
}

#[test]
fn build_without_dash_p_builds_every_member() {
    // The other half of #143: a two-member workspace produced an artifact
    // for the first-declared member only, and the second was never planned,
    // compiled or linked.
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let root = tmp.path().join("ws");
    fs::create_dir_all(&root).unwrap();
    write_two_member_workspace(&root);

    harbour_run(&home, &root, &["build"]).success();

    // Several roots, so each gets its own output directory -- the layout
    // `BuildPlan::with_root_packages` has always used for multiple roots,
    // and the reason it exists is that two members may name a target the
    // same thing.
    assert_eq!(run_workspace_exe(&root, "app", "app"), "app");
    assert_eq!(run_workspace_exe(&root, "other", "other"), "other");
}

#[test]
fn build_dash_p_does_not_build_the_other_members_dependencies() {
    // Selection has to reach the *graph*, not just the target list: `other`
    // depends on `only_other`, `app` does not, and `-p app` must build
    // neither. A plan that walked every package in the resolve would build
    // both, which is what made the naive fix ("pass the names to
    // BuildPlan") insufficient once the resolve contained all members.
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let root = tmp.path().join("ws");
    fs::create_dir_all(&root).unwrap();
    write_two_member_workspace(&root);

    let dep = root.join("only_other");
    fs::create_dir_all(dep.join("src")).unwrap();
    fs::write(
        dep.join("Harbour.toml"),
        "[package]\n\
         name = \"only_other\"\n\
         version = \"0.1.0\"\n\
         \n\
         [targets.only_other]\n\
         kind = \"staticlib\"\n\
         sources = [\"src/*.c\"]\n",
    )
    .unwrap();
    fs::write(
        dep.join("src/lib.c"),
        "int only_other_value(void) { return 7; }\n",
    )
    .unwrap();
    fs::write(
        root.join("other/Harbour.toml"),
        "[package]\n\
         name = \"other\"\n\
         version = \"0.1.0\"\n\
         \n\
         [dependencies]\n\
         only_other = { path = \"../only_other\" }\n\
         \n\
         [targets.other]\n\
         kind = \"bin\"\n\
         sources = [\"src/*.c\"]\n",
    )
    .unwrap();

    harbour_run(&home, &root, &["build", "-p", "app"]).success();

    assert_eq!(run_workspace_exe(&root, "", "app"), "app");
    assert!(
        !workspace_built_anything_named(&root, "libonly_other.a"),
        "`-p app` must not build a dependency only `other` needs"
    );
    assert!(
        !workspace_built_anything_named(&root, "other"),
        "`-p app` must not build `other`"
    );
}

#[test]
fn a_member_another_member_depends_on_is_built_as_that_dependency() {
    // A regression guard for the fix above rather than for the original bug,
    // and it earned its place by failing: once every member became a root,
    // this workspace stopped linking.
    //
    // `app` and `lib` are both members and `app` depends on `lib`. A root's
    // artifacts go to `output_dir/<pkg>/`, while a dependent looks for its
    // dependency in `deps/<name>-<version>/lib` -- so with both as roots the
    // archive was written to the first path and the link went looking in the
    // second:
    //
    //   error: linking failed for .../debug/app/bin/app
    //   clang: error: no such file or directory:
    //          '.../debug/deps/lib-0.1.0/lib/liblib.a'
    //
    // So a selected member that another selected member depends on is built
    // as that dependency, which also means this workspace's artifact layout
    // is exactly what it was before.
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let root = tmp.path().join("ws");
    fs::create_dir_all(root.join("app/src")).unwrap();
    fs::create_dir_all(root.join("lib/src")).unwrap();
    fs::create_dir_all(root.join("lib/include")).unwrap();

    fs::write(
        root.join("Harbour.toml"),
        "[workspace]\nmembers = [\"app\", \"lib\"]\n",
    )
    .unwrap();
    fs::write(
        root.join("app/Harbour.toml"),
        "[package]\n\
         name = \"app\"\n\
         version = \"0.1.0\"\n\
         \n\
         [dependencies]\n\
         lib = { path = \"../lib\" }\n\
         \n\
         [targets.app]\n\
         kind = \"bin\"\n\
         sources = [\"src/*.c\"]\n",
    )
    .unwrap();
    fs::write(
        root.join("lib/Harbour.toml"),
        "[package]\n\
         name = \"lib\"\n\
         version = \"0.1.0\"\n\
         \n\
         [targets.lib]\n\
         kind = \"staticlib\"\n\
         sources = [\"src/*.c\"]\n\
         public_headers = [\"include/*.h\"]\n\
         \n\
         [targets.lib.public]\n\
         include_dirs = [\"include\"]\n",
    )
    .unwrap();
    fs::write(root.join("lib/include/lib.h"), "int lib_answer(void);\n").unwrap();
    fs::write(
        root.join("lib/src/lib.c"),
        "int lib_answer(void){return 41;}\n",
    )
    .unwrap();
    fs::write(
        root.join("app/src/main.c"),
        "#include <stdio.h>\n\
         #include \"lib.h\"\n\
         int main(void) { printf(\"%d\\n\", lib_answer() + 1); return 0; }\n",
    )
    .unwrap();

    harbour_run(&home, &root, &["build"]).success();

    // One root, so the same path this workspace produced before.
    assert_eq!(
        run_workspace_exe(&root, "", "app"),
        "42",
        "the binary must link against the member it depends on"
    );
}
