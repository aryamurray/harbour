//! A target depending on a sibling target in its own package (`local = true`).
#![allow(unused_imports)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::prelude::*;
use predicates::prelude::*;
use tempfile::TempDir;

use harbour::test_support::fixtures::local_registry;

use crate::common::*;

/// Lay out a package with a static library `core` (declared *after* the
/// program that uses it, so build order cannot be accidentally right) and an
/// executable `tool` whose `deps` table is `tool_deps`.
fn write_local_dep_package(root: &Path, tool_deps: &str) {
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/core.h"),
        "#ifndef CORE_H\n#define CORE_H\nint core_value(void);\n#endif\n",
    )
    .unwrap();
    fs::write(
        root.join("src/core.c"),
        "int core_value(void) { return 41; }\n",
    )
    .unwrap();
    fs::write(
        root.join("src/tool.c"),
        "#include <stdio.h>\n#include \"core.h\"\n\
         int main(void) { printf(\"%d\\n\", core_value() + 1); return 0; }\n",
    )
    .unwrap();
    fs::write(
        root.join("Harbour.toml"),
        format!(
            "[package]\nname = \"multi\"\nversion = \"0.1.0\"\n\n\
             [targets.tool]\nkind = \"exe\"\nsources = [\"src/tool.c\"]\n\n\
             [targets.tool.deps]\n{tool_deps}\n\n\
             [targets.core]\nkind = \"staticlib\"\nsources = [\"src/core.c\"]\n\n\
             [targets.core.surface.compile.public]\ninclude_dirs = [\"src\"]\n"
        ),
    )
    .unwrap();
}

/// An executable links a static library from its own package, finds its
/// public headers, and builds even though the library is declared second.
///
/// Before this, `[targets.X.deps]` named only *packages*: `core = "core"` was
/// "dependency `core` not found, add it to [dependencies]", so a package with
/// a library and a program over it had to be split into two packages.
#[test]
fn test_a_target_can_depend_on_a_sibling_target_in_its_own_package() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let root = tmp.path().join("multi");
    write_local_dep_package(&root, "core = { local = true }");

    harbour_run(&home, &root, &["build"]).success();

    let output = Command::new(built_exe_path(&root, "tool"))
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "42",
        "the program must link the library from its own package"
    );
}

/// A mistake in a local dependency is named, not reported as a missing
/// package: the old message told the author to add a *package*.
#[test]
fn test_a_local_dependency_on_a_target_that_does_not_exist_is_refused() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let root = tmp.path().join("multi");
    write_local_dep_package(&root, "nope = { local = true }");

    let run = harbour_run(&home, &root, &["build"]);
    assert!(!run.status.success());
    let text = run.to_string();
    assert!(
        text.contains("nope") && text.contains("no target") && !text.contains("[dependencies]"),
        "expected an error about the missing sibling target, not about [dependencies]\n{text}"
    );
}

#[test]
fn test_a_local_dependency_on_an_executable_is_refused() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let root = tmp.path().join("multi");
    // `tool` depending on itself would be a cycle; use a second program.
    write_local_dep_package(&root, "other = { local = true }");
    let manifest = fs::read_to_string(root.join("Harbour.toml")).unwrap();
    fs::write(
        root.join("Harbour.toml"),
        format!("{manifest}\n[targets.other]\nkind = \"exe\"\nsources = [\"src/core.c\"]\n"),
    )
    .unwrap();

    let run = harbour_run(&home, &root, &["build"]);
    assert!(!run.status.success());
    assert!(
        run.to_string().contains("executable"),
        "an executable cannot be linked into another target\n{run}"
    );
}

/// Local dependencies form a graph, and a cycle in it has no build order. The
/// error names every target on the loop.
#[test]
fn test_a_cycle_between_sibling_targets_is_refused_and_named() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let root = tmp.path().join("multi");
    write_local_dep_package(&root, "core = { local = true }");
    let manifest = fs::read_to_string(root.join("Harbour.toml")).unwrap();
    fs::write(
        root.join("Harbour.toml"),
        manifest.replace(
            "[targets.core]\nkind = \"staticlib\"\nsources = [\"src/core.c\"]\n",
            "[targets.core]\nkind = \"staticlib\"\nsources = [\"src/core.c\"]\n\n\
             [targets.core.deps]\nhelper = { local = true }\n\n\
             [targets.helper]\nkind = \"staticlib\"\nsources = [\"src/core.c\"]\n\n\
             [targets.helper.deps]\ncore = { local = true }\n",
        ),
    )
    .unwrap();

    let run = harbour_run(&home, &root, &["build"]);
    assert!(!run.status.success());
    let text = run.to_string();
    assert!(
        text.contains("cycle") && text.contains("core") && text.contains("helper"),
        "the error must name the targets on the loop\n{text}"
    );
}

/// A package that is itself a dependency can have targets that depend on their
/// siblings, and its consumer links all of it.
///
/// `app -> multi:extra -> multi:core`. `extra` is a static library, so it does
/// not link `core` itself; whoever links `extra` needs `core`'s archive and
/// headers as well. A fold that handled a target's *own* siblings but not those
/// of a dependency's target would build fine and fail at the consumer's link
/// with an undefined symbol, which is the failure this test is shaped to catch.
#[test]
fn test_a_consumer_links_the_siblings_of_a_dependency_targets_siblings() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let multi = tmp.path().join("multi");
    fs::create_dir_all(multi.join("src")).unwrap();
    fs::write(
        multi.join("src/core.h"),
        "#ifndef CORE_H\n#define CORE_H\nint core_value(void);\n#endif\n",
    )
    .unwrap();
    fs::write(
        multi.join("src/core.c"),
        "int core_value(void) { return 41; }\n",
    )
    .unwrap();
    fs::write(
        multi.join("src/extra.h"),
        "#include \"core.h\"\nint extra_value(void);\n",
    )
    .unwrap();
    fs::write(
        multi.join("src/extra.c"),
        "#include \"extra.h\"\nint extra_value(void) { return core_value() + 1; }\n",
    )
    .unwrap();
    fs::write(
        multi.join("Harbour.toml"),
        "[package]\nname = \"multi\"\nversion = \"0.1.0\"\ndefault_target = \"extra\"\n\n\
         [targets.extra]\nkind = \"staticlib\"\nsources = [\"src/extra.c\"]\n\n\
         [targets.extra.surface.compile.public]\ninclude_dirs = [\"src\"]\n\n\
         [targets.extra.deps]\ncore = { local = true }\n\n\
         [targets.core]\nkind = \"staticlib\"\nsources = [\"src/core.c\"]\n\n\
         [targets.core.surface.compile.public]\ninclude_dirs = [\"src\"]\n",
    )
    .unwrap();

    let app = tmp.path().join("app");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(
        app.join("Harbour.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\nmulti = { path = \"../multi\" }\n\n\
         [targets.app]\nkind = \"exe\"\nsources = [\"src/main.c\"]\n",
    )
    .unwrap();
    fs::write(
        app.join("src/main.c"),
        "#include <stdio.h>\n#include \"extra.h\"\n\
         int main(void) { printf(\"%d\\n\", extra_value()); return 0; }\n",
    )
    .unwrap();

    harbour_run(&home, &app, &["build"]).success();

    let output = Command::new(built_exe_path(&app, "app")).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "42",
        "the consumer must link `core` as well as `extra`, and find both headers"
    );
}

/// Naming only the program with `--target` still builds the sibling library it
/// links against, and `harbour flags` reports that archive on the program's
/// link line.
///
/// The second half guards against the same fact being computed two ways: the
/// plan and `harbour flags` both read the surface fold and both need the
/// package's artifact directory, and a copy of that layout in each is how a
/// dependent once looked for its dependency's archive in the wrong place.
#[test]
fn test_selecting_one_target_builds_its_sibling_dependencies_and_flags_agrees() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let root = tmp.path().join("multi");
    write_local_dep_package(&root, "core = { local = true }");

    harbour_run(&home, &root, &["build", "--target", "tool"]).success();
    assert!(
        built_exe_path(&root, "tool").exists(),
        "the selected program must be built"
    );
    // MSVC names static libraries `core.lib`; everything else `libcore.a`.
    let core_archive = if cfg!(windows) {
        "core.lib"
    } else {
        "libcore.a"
    };
    let archive = root.join(".harbour/target/debug/lib").join(core_archive);
    assert!(
        archive.exists(),
        "`--target tool` must build the sibling library `core` that `tool` links, \
         expected {}",
        archive.display()
    );

    let flags = harbour_run(&home, &root, &["flags", "tool", "--link"]);
    assert!(flags.status.success(), "{flags}");
    assert!(
        flags.to_string().contains(core_archive),
        "`harbour flags` must show the sibling archive on the link line\n{flags}"
    );
}

/// A local entry never lends its settings to a package of the same name.
///
/// `app` depends on a *package* called `core` and also has a *target* called
/// `core`, and `tool`'s `deps` entry for `core` is the local one, marked
/// `private`. Private means "do not take this dependency's surface", so if the
/// package lookup read the local entry, the package's header would disappear
/// and `tool` would fail to compile. It must still see and link the package,
/// and must not see the local sibling it declined.
#[test]
fn test_a_local_dependency_does_not_affect_a_package_of_the_same_name() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let pkg = tmp.path().join("corepkg");
    fs::create_dir_all(pkg.join("include")).unwrap();
    fs::create_dir_all(pkg.join("src")).unwrap();
    fs::write(pkg.join("include/corepkg.h"), "int pkg_value(void);\n").unwrap();
    fs::write(
        pkg.join("src/pkg.c"),
        "int pkg_value(void) { return 42; }\n",
    )
    .unwrap();
    fs::write(
        pkg.join("Harbour.toml"),
        "[package]\nname = \"core\"\nversion = \"0.1.0\"\n\n\
         [targets.core]\nkind = \"staticlib\"\nsources = [\"src/pkg.c\"]\n\n\
         [targets.core.surface.compile.public]\ninclude_dirs = [\"include\"]\n",
    )
    .unwrap();

    let app = tmp.path().join("app");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(app.join("src/localcore.h"), "int local_value(void);\n").unwrap();
    fs::write(
        app.join("src/local.c"),
        "int local_value(void) { return 7; }\n",
    )
    .unwrap();
    // `localcore.h` is deliberately not included: the local sibling was declined.
    fs::write(
        app.join("src/tool.c"),
        "#include <stdio.h>\n#include \"corepkg.h\"\n\
         int main(void) { printf(\"%d\\n\", pkg_value()); return 0; }\n",
    )
    .unwrap();
    fs::write(
        app.join("Harbour.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\ncore = { path = \"../corepkg\" }\n\n\
         [targets.tool]\nkind = \"exe\"\nsources = [\"src/tool.c\"]\n\n\
         [targets.tool.deps]\ncore = { local = true, compile = \"private\", link = \"private\" }\n\n\
         [targets.core]\nkind = \"staticlib\"\nsources = [\"src/local.c\"]\n\n\
         [targets.core.surface.compile.public]\ninclude_dirs = [\"src\"]\n",
    )
    .unwrap();

    harbour_run(&home, &app, &["build"]).success();

    let output = Command::new(built_exe_path(&app, "tool")).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "42",
        "`tool` must link the *package* `core`; the local sibling's `private` must not leak to it"
    );
}

/// In a workspace with several roots, `harbour flags` prints the sibling
/// archive where the build actually wrote it.
///
/// Several roots write to `output_dir/<pkg>/` instead of `output_dir/`, so a
/// command that assumed a single root printed `debug/lib/libcore.a` for an
/// archive that was at `debug/multi/lib/libcore.a`. The assertion compares the
/// printed path with a file the build really produced, rather than with a
/// path this test computed, so the two cannot agree by sharing a mistake.
#[test]
fn test_flags_shows_the_sibling_archive_where_a_multi_root_workspace_wrote_it() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let ws = tmp.path().join("ws");
    fs::create_dir_all(&ws).unwrap();
    fs::write(
        ws.join("Harbour.toml"),
        "[workspace]\nmembers = [\"a\", \"b\"]\n",
    )
    .unwrap();

    write_local_dep_package(&ws.join("a"), "core = { local = true }");
    fs::create_dir_all(ws.join("b/src")).unwrap();
    fs::write(ws.join("b/src/b.c"), "int b_value(void) { return 1; }\n").unwrap();
    fs::write(
        ws.join("b/Harbour.toml"),
        "[package]\nname = \"other\"\nversion = \"0.1.0\"\n\n\
         [targets.other]\nkind = \"staticlib\"\nsources = [\"src/b.c\"]\n",
    )
    .unwrap();

    harbour_run(&home, &ws, &["build"]).success();

    let core_archive = if cfg!(windows) {
        "core.lib"
    } else {
        "libcore.a"
    };
    let written: Vec<PathBuf> = walk_files(&ws.join(".harbour"))
        .into_iter()
        .filter(|p| p.file_name().is_some_and(|n| n == core_archive))
        .collect();
    assert_eq!(
        written.len(),
        1,
        "expected exactly one {core_archive}: {written:?}"
    );
    let sep = std::path::MAIN_SEPARATOR;
    let tail = format!("multi{sep}lib{sep}{core_archive}");
    assert!(
        written[0].to_string_lossy().ends_with(&tail),
        "a multi-root build writes under the package's own directory, got {}",
        written[0].display()
    );

    let flags = harbour_run(&home, &ws.join("a"), &["flags", "tool", "--link"]);
    assert!(flags.status.success(), "{flags}");
    assert!(
        flags.to_string().contains(&tail),
        "`harbour flags` must print the archive where the build wrote it ({tail})\n{flags}"
    );
}
