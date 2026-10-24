//! Features in the manifest: toggles, `dep/feature`, optional dependencies and `dep:` syntax.
#![allow(unused_imports)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::prelude::*;
use predicates::prelude::*;
use tempfile::TempDir;

use harbour::test_support::fixtures::local_registry;

use crate::common::*;

/// A single-source library, deliberately shaped like sqlite's amalgamation:
/// one `.c` file whose behavior branches entirely on whether a preprocessor
/// define is present, and that define is only supplied when a manifest
/// `[features]` toggle is enabled.
///
/// This is the strong form of the validation the change asked for: not "the
/// flag string contains -DENABLE_FTS5", but "the same source, compiled
/// twice with the feature off vs. on, produces a binary that runs
/// differently" -- so a regression that silently stops threading the
/// feature into the compile step (e.g. because `resolved_extra_compile` or
/// the `feature = "..."` predicate on `PlatformCondition` broke) fails this
/// test via a wrong *runtime* answer, not just a missing string in a
/// recorded flag list.
fn write_feature_lib(dir: &std::path::Path) {
    fs::create_dir_all(dir).unwrap();
    fs::write(
        dir.join("Harbour.toml"),
        r#"[package]
name = "sqlike"
version = "0.1.0"

[features]
fts5 = []

[targets.sqlike]
kind = "staticlib"
sources = ["src/**/*.c"]

[targets.sqlike.surface.compile.public]
include_dirs = ["include"]

[[targets.sqlike.when]]
feature = "fts5"
defines = ["ENABLE_FTS5"]
"#,
    )
    .unwrap();
    fs::create_dir_all(dir.join("include")).unwrap();
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join("include/sqlike.h"),
        r#"#ifndef SQLIKE_H
#define SQLIKE_H
int sqlike_has_fts5(void);
#endif
"#,
    )
    .unwrap();
    fs::write(
        dir.join("src/lib.c"),
        r#"#include "sqlike.h"

int sqlike_has_fts5(void) {
#ifdef ENABLE_FTS5
    return 1;
#else
    return 0;
#endif
}
"#,
    )
    .unwrap();
}

#[test]
fn test_feature_toggles_define_and_changes_binary_behavior() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    write_feature_lib(&tmp.path().join("sqlike"));

    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");

    harbour(&home)
        .args(["add", "sqlike", "--path", "../sqlike"])
        .current_dir(&app_dir)
        .assert()
        .success();

    fs::write(
        app_dir.join("src/main.c"),
        r#"#include <stdio.h>
#include "sqlike.h"

int main(void) {
    printf("%d\n", sqlike_has_fts5());
    return 0;
}
"#,
    )
    .unwrap();

    // Feature off (default): `fts5` is not in `[features]`'s implicit
    // default set (there is no `default` key), and the app didn't request
    // it, so the library must be built without ENABLE_FTS5.
    harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .success();
    let exe = built_exe_path(&app_dir, "app");
    let output = Command::new(&exe).output().unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "0",
        "feature off must compile out ENABLE_FTS5"
    );

    // Turn the feature on from the dependent and rebuild. Editing the
    // dependency line directly (rather than through `harbour add
    // --features`, which today only plumbs vcpkg feature selection) matches
    // how `[dependencies].features` is actually meant to be authored for a
    // native package -- see `DetailedDependencySpec::features`.
    let manifest_path = app_dir.join("Harbour.toml");
    let manifest = fs::read_to_string(&manifest_path).unwrap();
    let manifest = manifest.replace(
        r#"sqlike = { path = "../sqlike" }"#,
        r#"sqlike = { path = "../sqlike", features = ["fts5"] }"#,
    );
    assert_ne!(
        manifest,
        fs::read_to_string(&manifest_path).unwrap(),
        "expected `harbour add`'s generated dependency line to match the replaced pattern"
    );
    fs::write(&manifest_path, manifest).unwrap();

    // Captured for the same reason as the `dep/feature` one-hop test: this
    // fails intermittently on Windows with the pre-change output, and the
    // build log is what distinguishes "never recompiled" from "recompiled
    // but not relinked".
    let rebuild = harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .success();
    let rebuild_log = String::from_utf8_lossy(&rebuild.get_output().stderr).into_owned();

    let output = Command::new(&exe).output().unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "1",
        "feature on must define ENABLE_FTS5 and recompile the library.\n\n\
         Rebuild log:\n{rebuild_log}"
    );
}

/// A library with a `[features]` entry whose only job is to gate a
/// preprocessor define -- same shape as `write_feature_lib` above, but
/// parameterized so it can be reused for `inner`/`mid`/`leaf` roles across
/// the tests below.
fn write_relay_lib(dir: &std::path::Path, name: &str, feature: &str, define: &str, value_fn: &str) {
    fs::create_dir_all(dir).unwrap();
    fs::write(
        dir.join("Harbour.toml"),
        format!(
            r#"[package]
name = "{name}"
version = "0.1.0"

[features]
{feature} = []

[targets.{name}]
kind = "staticlib"
sources = ["src/**/*.c"]

[targets.{name}.surface.compile.public]
include_dirs = ["include"]

[[targets.{name}.when]]
feature = "{feature}"
defines = ["{define}"]
"#
        ),
    )
    .unwrap();
    fs::create_dir_all(dir.join("include")).unwrap();
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join(format!("include/{name}.h")),
        format!(
            "#ifndef {name}_H\n#define {name}_H\nint {value_fn}(void);\n#endif\n",
            name = name.to_uppercase()
        ),
    )
    .unwrap();
    fs::write(
        dir.join("src/lib.c"),
        format!(
            r#"#include "{name}.h"

int {value_fn}(void) {{
#ifdef {define}
    return 1;
#else
    return 0;
#endif
}}
"#
        ),
    )
    .unwrap();
}

/// `outer` declares `want = ["inner/deep"]`; the app requests `outer/want`
/// only -- never touching `inner` directly. `inner`'s function must return
/// the enabled value, proving the request reached across the one hop from
/// `outer`'s own `[features]` entry to `inner`'s.
///
/// `app` declares only `outer`; `inner` is reached transitively, so the test
/// also covers that the request crosses a dependency edge the root never names.
#[test]
fn test_dep_feature_propagates_one_hop_and_changes_binary_behavior() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    write_relay_lib(
        &tmp.path().join("inner"),
        "inner",
        "deep",
        "ENABLE_DEEP",
        "inner_value",
    );

    fs::create_dir_all(tmp.path().join("outer")).unwrap();
    fs::write(
        tmp.path().join("outer/Harbour.toml"),
        r#"[package]
name = "outer"
version = "0.1.0"

[features]
want = ["inner/deep"]

[targets.outer]
kind = "staticlib"
sources = ["src/**/*.c"]

[targets.outer.surface.compile.public]
include_dirs = ["include"]
"#,
    )
    .unwrap();
    fs::create_dir_all(tmp.path().join("outer/include")).unwrap();
    fs::create_dir_all(tmp.path().join("outer/src")).unwrap();
    fs::write(
        tmp.path().join("outer/include/outer.h"),
        "#ifndef OUTER_H\n#define OUTER_H\nint outer_value(void);\n#endif\n",
    )
    .unwrap();
    fs::write(
        tmp.path().join("outer/src/lib.c"),
        r#"#include "outer.h"
#include "inner.h"

int outer_value(void) {
    return inner_value();
}
"#,
    )
    .unwrap();
    harbour(&home)
        .args(["add", "inner", "--path", "../inner"])
        .current_dir(tmp.path().join("outer"))
        .assert()
        .success();

    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");
    harbour(&home)
        .args(["add", "outer", "--path", "../outer"])
        .current_dir(&app_dir)
        .assert()
        .success();

    fs::write(
        app_dir.join("src/main.c"),
        r#"#include <stdio.h>
#include "outer.h"

int main(void) {
    printf("%d\n", outer_value());
    return 0;
}
"#,
    )
    .unwrap();

    // First, without requesting `outer/want` at all: `inner` must be built
    // without ENABLE_DEEP.
    harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .success()
        .stderr(predicate::str::contains("Finished"));

    let exe = built_exe_path(&app_dir, "app");
    let output = Command::new(&exe).output().unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "0",
        "feature off (nothing requests `outer/want`): inner must not define ENABLE_DEEP"
    );

    // Now request `outer/want` only -- never `inner` directly -- and
    // rebuild *without* cleaning first. This is the fingerprint-
    // invalidation check requirement 5 in the task asked for: a change in
    // a *propagated* feature set must invalidate `inner`'s compile just as
    // surely as a directly-requested one would, since it flows into the
    // same `defines`/`cflags` that feed `CompileFingerprint::flags_hash`.
    // If propagation updated the in-memory feature set but something
    // upstream cached inner's old fingerprint, this rebuild would wrongly
    // skip recompiling `inner` and the binary would still print `0`.
    let manifest_path = app_dir.join("Harbour.toml");
    let manifest = fs::read_to_string(&manifest_path).unwrap();
    let manifest = manifest.replace(
        r#"outer = { path = "../outer" }"#,
        r#"outer = { path = "../outer", features = ["want"] }"#,
    );
    assert_ne!(manifest, fs::read_to_string(&manifest_path).unwrap());
    fs::write(&manifest_path, manifest).unwrap();

    // Snapshot the build tree across the rebuild so a failure can say
    // *which* stage went wrong, and assert each stage separately. The two
    // candidate causes need different fixes -- `inner` was never recompiled
    // (a fingerprint that failed to invalidate on a propagated feature
    // change), or it was recompiled and the executable was not relinked
    // against the new archive -- and `left: "0"` alone cannot tell them
    // apart. That ambiguity is what made the first two attempts at
    // diagnosing this Windows-only, intermittent failure useless.
    let (rebuild, diff) = rebuild_and_diff(&home, &app_dir);
    diff.assert_touched(
        "inner",
        "a change in a *propagated* feature set must invalidate inner's \
         cached object exactly as a directly requested one would",
    );
    diff.assert_touched(
        "app",
        "inner was recompiled, so the executable must be relinked against \
         the new archive",
    );

    let out = run_built_exe(&app_dir, "app");
    assert_eq!(
        out.out(),
        "1",
        "app now requests `outer/want`; `outer`'s `dep/feature` entry must have \
         propagated `deep` onto `inner` and defined ENABLE_DEEP there.\n\n\
         rebuild:\n{rebuild}\n{diff}"
    );
}

/// A chain three deep: `app -> outer -> mid -> leaf`. `app` requests only
/// `outer/want`; `outer`'s `want` requests `mid/relay`; `mid`'s `relay`
/// requests `leaf/leaf_feat`. Two hops of `dep/feature` back to back,
/// proving propagation is transitive rather than a single-hop special case.
#[test]
fn test_dep_feature_propagates_transitively_through_a_chain() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    write_relay_lib(
        &tmp.path().join("leaf"),
        "leaf",
        "leaf_feat",
        "ENABLE_LEAF",
        "leaf_value",
    );

    fs::create_dir_all(tmp.path().join("mid")).unwrap();
    fs::write(
        tmp.path().join("mid/Harbour.toml"),
        r#"[package]
name = "mid"
version = "0.1.0"

[features]
relay = ["leaf/leaf_feat"]

[targets.mid]
kind = "staticlib"
sources = ["src/**/*.c"]

[targets.mid.surface.compile.public]
include_dirs = ["include"]
"#,
    )
    .unwrap();
    fs::create_dir_all(tmp.path().join("mid/include")).unwrap();
    fs::create_dir_all(tmp.path().join("mid/src")).unwrap();
    fs::write(
        tmp.path().join("mid/include/mid.h"),
        "#ifndef MID_H\n#define MID_H\nint mid_value(void);\n#endif\n",
    )
    .unwrap();
    fs::write(
        tmp.path().join("mid/src/lib.c"),
        r#"#include "mid.h"
#include "leaf.h"

int mid_value(void) {
    return leaf_value();
}
"#,
    )
    .unwrap();
    harbour(&home)
        .args(["add", "leaf", "--path", "../leaf"])
        .current_dir(tmp.path().join("mid"))
        .assert()
        .success();

    fs::create_dir_all(tmp.path().join("outer")).unwrap();
    fs::write(
        tmp.path().join("outer/Harbour.toml"),
        r#"[package]
name = "outer"
version = "0.1.0"

[features]
want = ["mid/relay"]

[targets.outer]
kind = "staticlib"
sources = ["src/**/*.c"]

[targets.outer.surface.compile.public]
include_dirs = ["include"]
"#,
    )
    .unwrap();
    fs::create_dir_all(tmp.path().join("outer/include")).unwrap();
    fs::create_dir_all(tmp.path().join("outer/src")).unwrap();
    fs::write(
        tmp.path().join("outer/include/outer.h"),
        "#ifndef OUTER_H\n#define OUTER_H\nint outer_value(void);\n#endif\n",
    )
    .unwrap();
    fs::write(
        tmp.path().join("outer/src/lib.c"),
        r#"#include "outer.h"
#include "mid.h"

int outer_value(void) {
    return mid_value();
}
"#,
    )
    .unwrap();
    harbour(&home)
        .args(["add", "mid", "--path", "../mid"])
        .current_dir(tmp.path().join("outer"))
        .assert()
        .success();

    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");
    harbour(&home)
        .args(["add", "outer", "--path", "../outer"])
        .current_dir(&app_dir)
        .assert()
        .success();

    let manifest_path = app_dir.join("Harbour.toml");
    let manifest = fs::read_to_string(&manifest_path).unwrap();
    let manifest = manifest.replace(
        r#"outer = { path = "../outer" }"#,
        r#"outer = { path = "../outer", features = ["want"] }"#,
    );
    assert_ne!(manifest, fs::read_to_string(&manifest_path).unwrap());
    fs::write(&manifest_path, manifest).unwrap();

    fs::write(
        app_dir.join("src/main.c"),
        r#"#include <stdio.h>
#include "outer.h"

int main(void) {
    printf("%d\n", outer_value());
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
    let output = Command::new(&exe).output().unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "1",
        "app requested only `outer/want`; propagation must cross both \
         `outer -> mid` and `mid -> leaf` dep/feature hops to define \
         ENABLE_LEAF in `leaf`"
    );
}

/// Diamond dependency shape (`app -> b -> d`, `app -> relay_c -> d`), but the
/// requests on the shared tail `d` arrive via `dep/feature` rather than a
/// direct `features = [...]` entry: `b` declares `want_x = ["d/x"]` and
/// `relay_c` declares `want_y = ["d/y"]`; the app requests both.
/// `d`'s final feature set must be the union `{x, y}` -- if unification
/// broke for propagated requests specifically (as opposed to direct ones,
/// already covered by `compute_feature_sets_unifies_disjoint_dependent_requests`
/// in `surface_resolver.rs`), `d` would only ever see one of the two and
/// this test's arithmetic would come out wrong.
#[test]
fn test_dep_feature_diamond_union_via_dep_feature() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    fs::create_dir_all(tmp.path().join("d")).unwrap();
    fs::write(
        tmp.path().join("d/Harbour.toml"),
        r#"[package]
name = "d"
version = "0.1.0"

[features]
x = []
y = []

[targets.d]
kind = "staticlib"
sources = ["src/**/*.c"]

[targets.d.surface.compile.public]
include_dirs = ["include"]

[[targets.d.when]]
feature = "x"
defines = ["ENABLE_X"]

[[targets.d.when]]
feature = "y"
defines = ["ENABLE_Y"]
"#,
    )
    .unwrap();
    fs::create_dir_all(tmp.path().join("d/include")).unwrap();
    fs::create_dir_all(tmp.path().join("d/src")).unwrap();
    fs::write(
        tmp.path().join("d/include/d.h"),
        "#ifndef D_H\n#define D_H\nint d_value(void);\n#endif\n",
    )
    .unwrap();
    fs::write(
        tmp.path().join("d/src/lib.c"),
        r#"#include "d.h"

int d_value(void) {
    int v = 0;
#ifdef ENABLE_X
    v += 1;
#endif
#ifdef ENABLE_Y
    v += 10;
#endif
    return v;
}
"#,
    )
    .unwrap();

    let make_relay = |name: &str, feature: &str, dep_feature: &str, value_fn: &str| {
        let dir = tmp.path().join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("Harbour.toml"),
            format!(
                r#"[package]
name = "{name}"
version = "0.1.0"

[features]
{feature} = ["d/{dep_feature}"]

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
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(
            dir.join(format!("include/{name}.h")),
            format!(
                "#ifndef {upper}_H\n#define {upper}_H\nint {value_fn}(void);\n#endif\n",
                upper = name.to_uppercase()
            ),
        )
        .unwrap();
        fs::write(
            dir.join("src/lib.c"),
            format!(
                r#"#include "{name}.h"
#include "d.h"

int {value_fn}(void) {{
    return d_value();
}}
"#
            ),
        )
        .unwrap();
        harbour(&home)
            .args(["add", "d", "--path", "../d"])
            .current_dir(&dir)
            .assert()
            .success();
    };

    make_relay("b", "want_x", "x", "b_value");
    // Not named `c`: the archive would be `libc.a`, and since a dependency's
    // lib dir lands on `-L`, gcc's implicit `-lc` would resolve to it instead
    // of the system C library and the link would fail with undefined
    // `__libc_start_main`/`printf`.
    make_relay("relay_c", "want_y", "y", "c_value");

    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");
    // `app` names only b and c; d is reached transitively through both.
    for dep in ["b", "relay_c"] {
        harbour(&home)
            .args(["add", dep, "--path", &format!("../{dep}")])
            .current_dir(&app_dir)
            .assert()
            .success();
    }

    let manifest_path = app_dir.join("Harbour.toml");
    let manifest = fs::read_to_string(&manifest_path).unwrap();
    let manifest = manifest
        .replace(
            r#"b = { path = "../b" }"#,
            r#"b = { path = "../b", features = ["want_x"] }"#,
        )
        .replace(
            r#"relay_c = { path = "../relay_c" }"#,
            r#"relay_c = { path = "../relay_c", features = ["want_y"] }"#,
        );
    assert_ne!(manifest, fs::read_to_string(&manifest_path).unwrap());
    fs::write(&manifest_path, manifest).unwrap();

    fs::write(
        app_dir.join("src/main.c"),
        r#"#include <stdio.h>
#include "b.h"
#include "relay_c.h"

int main(void) {
    printf("%d\n", b_value() + c_value());
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
    let output = Command::new(&exe).output().unwrap();
    assert!(output.status.success());
    // If the union held, `d` is built once with {x, y}, so d_value() == 11
    // everywhere and the sum is 22. If propagated requests failed to unify
    // (e.g. only the last writer won), `d` would see only one of {x, y}
    // and this would come out as 2 (both see only x) or 20 (both see only
    // y) instead.
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "22",
        "d's feature set must be the union {{x, y}} of both dep/feature requests"
    );
}

/// `harbour add --optional` writes `optional = true`, and the manifest it
/// produces loads.
///
/// The flag used to refuse, because the key it wrote changed nothing. Now
/// that it does something, the round trip is the thing worth asserting: the
/// spelling `add` writes has to be the spelling the parser reads, and the
/// added dependency must be *absent* from the build until a feature asks for
/// it (which no feature here does).
#[test]
fn test_harbour_add_optional_writes_a_key_the_parser_reads() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let app = tmp.path().join("addopt");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(app.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();
    let manifest = "[package]\n\
                    name = \"addopt\"\n\
                    version = \"0.1.0\"\n\
                    \n\
                    [targets.addopt]\n\
                    kind = \"exe\"\n\
                    sources = [\"src/main.c\"]\n";
    fs::write(app.join("Harbour.toml"), manifest).unwrap();

    let run = harbour_run(
        &home,
        &app,
        &[
            "add",
            "zlib",
            "--version",
            "1.3.1",
            "--optional",
            "--offline",
        ],
    );
    run.success();

    let written = fs::read_to_string(app.join("Harbour.toml")).unwrap();
    assert!(
        written.contains("optional = true"),
        "`--optional` must write the key\n{written}"
    );

    // The manifest `add` just wrote has to load. `--offline` with no
    // registry cache means the dependency cannot be fetched, so a build
    // that *succeeds* is itself the assertion that the optional dependency
    // was never fetched: nothing activated `zlib`, so nothing asked its
    // source a question.
    harbour_run(&home, &app, &["build", "--offline"]).success();
    assert!(built_exe_path_in(&app, "debug", "addopt").exists());
}

/// `default-features = false` turns default features off, and a misspelled
/// key in the same table fails the build.
///
/// Two halves of one defect. `DetailedDependencySpec.default_features` had
/// no serde rename, so the hyphenated spelling -- the only one anyone
/// writes, the one Cargo uses, the one this crate's own tests use, and the
/// one `manifest.rs` lists in its "you meant the package-level table" hint
/// -- was absorbed as an unknown key and thrown away. The dependency was
/// built with its default features on, silently. The same table absorbed
/// `brnach` and `verison`, where the value being thrown away decides which
/// source is fetched.
///
/// Asserted on the compile database rather than on a parsed field, because
/// what matters is whether the *dependency's compile* changed.
#[test]
fn test_default_features_false_reaches_the_build_and_a_typo_does_not_pass() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let lib = tmp.path().join("featlib");
    fs::create_dir_all(lib.join("src")).unwrap();
    fs::write(
        lib.join("src/l.c"),
        "#ifdef WITH_EXTRA\n\
         int extra(void) { return 1; }\n\
         #endif\n\
         int l(void) { return 2; }\n",
    )
    .unwrap();
    fs::write(
        lib.join("Harbour.toml"),
        "[package]\n\
         name = \"featlib\"\n\
         version = \"1.0.0\"\n\
         \n\
         [features]\n\
         default = [\"extra\"]\n\
         extra = []\n\
         \n\
         [targets.featlib]\n\
         kind = \"staticlib\"\n\
         sources = [\"src/l.c\"]\n\
         \n\
         [[targets.featlib.when]]\n\
         feature = \"extra\"\n\
         defines = [\"WITH_EXTRA=1\"]\n",
    )
    .unwrap();

    let app = tmp.path().join("featapp");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(app.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();

    let manifest = |dep_keys: &str| {
        format!(
            "[package]\n\
             name = \"featapp\"\n\
             version = \"0.1.0\"\n\
             \n\
             [dependencies]\n\
             featlib = {{ path = \"../featlib\"{dep_keys} }}\n\
             \n\
             [targets.featapp]\n\
             kind = \"exe\"\n\
             sources = [\"src/main.c\"]\n\
             \n\
             [targets.featapp.deps]\n\
             featlib = \"featlib\"\n"
        )
    };

    let extra_is_on = |app: &std::path::Path| -> bool {
        fs::read_to_string(app.join(".harbour/compile_commands.json"))
            .unwrap()
            .contains("WITH_EXTRA")
    };

    // Say nothing: the default feature is on.
    fs::write(app.join("Harbour.toml"), manifest("")).unwrap();
    harbour_run(&home, &app, &["build"]).success();
    assert!(
        extra_is_on(&app),
        "with no opt-out the dependency's default feature must be enabled"
    );

    // Opt out with the hyphenated spelling: it must actually take effect.
    fs::remove_dir_all(app.join(".harbour")).unwrap();
    fs::write(
        app.join("Harbour.toml"),
        manifest(", default-features = false"),
    )
    .unwrap();
    let run = harbour_run(&home, &app, &["build"]).success();
    assert!(
        !extra_is_on(&app),
        "`default-features = false` must reach the build -- it used to be \
         absorbed as an unknown key and the dependency kept its defaults\n{run}"
    );

    // The underscore spelling stays accepted, so manifests written against
    // the form that worked keep working.
    fs::remove_dir_all(app.join(".harbour")).unwrap();
    fs::write(
        app.join("Harbour.toml"),
        manifest(", default_features = false"),
    )
    .unwrap();
    harbour_run(&home, &app, &["build"]).success();
    assert!(!extra_is_on(&app), "the underscore alias must still work");

    // And a key that is neither fails the build, by name.
    fs::write(
        app.join("Harbour.toml"),
        manifest(", deafult-features = false"),
    )
    .unwrap();
    let run = harbour_run(&home, &app, &["build"]);
    assert!(
        !run.status.success(),
        "a misspelled dependency key must not be absorbed\n{run}"
    );
    assert!(
        run.combined().contains("deafult-features") && run.combined().contains("featlib"),
        "the diagnostic must name the key and the dependency\n{run}"
    );
}

// Optional dependencies (issue #108)
//
// `optional = true` used to parse and change nothing: the dependency was
// resolved, fetched, built and linked regardless. The assertions below are
// deliberately about the *filesystem and the produced binary*, not about
// exit status or log lines, because "the build succeeded" is exactly what
// the broken behaviour also did:
//
//   - disabled: no archive for the dependency exists anywhere under the
//     target directory, the dependency's name appears in no compiler argv,
//     it is absent from `Harbour.lock`, and the binary takes the
//     without-it branch;
//   - a disabled *git* optional dependency points at a URL that cannot
//     resolve, so a build that succeeds proves nothing asked its source a
//     question -- and flipping the same manifest to activate it makes the
//     build fail, which is what rules out "git deps are just ignored";
//   - enabled: the archive exists, the argv carries it, and the binary
//     prints the number only the dependency can compute.
/// A dependency with a distinctive name (so a substring search over compiler
/// argv cannot collide with the consumer's own paths) exposing one function.
fn write_optional_dep(dir: &std::path::Path) {
    fs::create_dir_all(dir.join("include")).unwrap();
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join("Harbour.toml"),
        r#"[package]
name = "zzoptional"
version = "0.1.0"

[targets.zzoptional]
kind = "staticlib"
sources = ["src/lib.c"]

[targets.zzoptional.surface.compile.public]
include_dirs = ["include"]
"#,
    )
    .unwrap();
    fs::write(
        dir.join("include/zzoptional.h"),
        "#ifndef ZZOPTIONAL_H\n#define ZZOPTIONAL_H\nint zzoptional_answer(void);\n#endif\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/lib.c"),
        "#include \"zzoptional.h\"\nint zzoptional_answer(void) { return 42; }\n",
    )
    .unwrap();
}

/// The consumer. `withopt` activates the optional dependency through its
/// implicit feature, and the `when` block is what makes the *binary*
/// observably different rather than merely the link line.
fn write_optional_consumer(dir: &std::path::Path, default_features: &str) {
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join("Harbour.toml"),
        format!(
            r#"[package]
name = "optapp"
version = "0.1.0"

[dependencies]
zzoptional = {{ path = "../zzoptional", optional = true }}

[features]
default = [{default_features}]
withopt = ["zzoptional"]

[targets.optapp]
kind = "exe"
sources = ["src/main.c"]

[[targets.optapp.when]]
feature = "withopt"
defines = ["WITH_OPT=1"]
"#
        ),
    )
    .unwrap();
    fs::write(
        dir.join("src/main.c"),
        r#"#include <stdio.h>
#ifdef WITH_OPT
#include "zzoptional.h"
#endif

int main(void) {
#ifdef WITH_OPT
    printf("%d\n", zzoptional_answer());
#else
    printf("absent\n");
#endif
    return 0;
}
"#,
    )
    .unwrap();
}

/// Whether an archive for `name` exists anywhere under the target tree.
///
/// The counterpart of [`built_archive_path`], which panics when there is
/// none -- here *none* is the interesting answer.
fn archive_exists(dir: &std::path::Path, name: &str) -> bool {
    let wanted = [format!("lib{name}.a"), format!("{name}.lib")];
    snapshot_tree(&target_dir(dir)).into_keys().any(|p| {
        p.file_name()
            .map(|f| wanted.iter().any(|w| w.as_str() == f))
            .unwrap_or(false)
    })
}

/// The load-bearing test: disabled means not built, not linked, not in the
/// lockfile, and a different binary.
#[test]
fn test_optional_dependency_is_absent_from_the_build_until_a_feature_activates_it() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    write_optional_dep(&tmp.path().join("zzoptional"));

    let app = tmp.path().join("optapp");
    write_optional_consumer(&app, "");

    harbour_run(&home, &app, &["build"]).success();

    assert!(
        !archive_exists(&app, "zzoptional"),
        "nothing activated `zzoptional`, so it must not have been built.\n\
         build tree:\n{:#?}",
        snapshot_tree(&target_dir(&app))
            .into_keys()
            .collect::<Vec<_>>()
    );

    let lock = fs::read_to_string(app.join("Harbour.lock")).unwrap();
    assert!(
        !lock.contains("zzoptional"),
        "an inactive optional dependency must not be in the lockfile:\n{lock}"
    );

    let exe = built_exe_path(&app, "optapp");
    let out = Command::new(&exe).output().unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "absent",
        "with the feature off the binary must take the without-it branch"
    );

    // `--locked` is the honest way to assert the lockfile is not considered
    // stale: it refuses to run if resolution would change it. An optional
    // dependency that is absent from the graph must not make every build
    // look like it needs re-resolving.
    harbour_run(&home, &app, &["--locked", "build"]).success();

    // Now activate it, and everything flips.
    write_optional_consumer(&app, "\"withopt\"");
    harbour_run(&home, &app, &["build"]).success();

    assert!(
        archive_exists(&app, "zzoptional"),
        "`default = [\"withopt\"]` activates the implicit `zzoptional` feature, \
         so the dependency must now be built.\nbuild tree:\n{:#?}",
        snapshot_tree(&target_dir(&app))
            .into_keys()
            .collect::<Vec<_>>()
    );
    let lock = fs::read_to_string(app.join("Harbour.lock")).unwrap();
    assert!(
        lock.contains("zzoptional"),
        "an active optional dependency must be recorded in the lockfile:\n{lock}"
    );

    let out = Command::new(&exe).output().unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "42",
        "the binary must now call into the dependency"
    );

    harbour_run(&home, &app, &["--locked", "build"]).success();
}

/// The same thing asserted on the **real argv the compiler and linker were
/// handed**, which is the only place "built but not linked" and "linked but
/// not compiled against" can be told apart.
#[cfg(not(windows))]
#[test]
fn test_an_inactive_optional_dependency_reaches_no_compiler_argv() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    write_optional_dep(&tmp.path().join("zzoptional"));

    let app = tmp.path().join("optapp");
    write_optional_consumer(&app, "");

    let off_root = tmp.path().join("off");
    fs::create_dir_all(&off_root).unwrap();
    let (shim, off_records) = install_cc_recorder(&off_root);
    harbour_run_env(&home, &app, &["build"], &[("CC", shim.to_str().unwrap())]).success();

    let off: Vec<String> = recorded_argvs(&off_records).into_iter().flatten().collect();
    assert!(
        !off.is_empty(),
        "the recording shim captured nothing, so this test proves nothing"
    );
    assert!(
        !off.iter().any(|a| a.contains("zzoptional")),
        "an inactive optional dependency must not appear in any compile or link \
         argv -- not as an `-I`, not as an archive operand.\nargv:\n{off:#?}"
    );

    // Activate it and the same capture must now show it, on both sides: an
    // `-I` into its include directory, and its archive as a link operand.
    write_optional_consumer(&app, "\"withopt\"");
    let on_root = tmp.path().join("on");
    fs::create_dir_all(&on_root).unwrap();
    let (shim, on_records) = install_cc_recorder(&on_root);
    harbour_run_env(&home, &app, &["build"], &[("CC", shim.to_str().unwrap())]).success();

    let on = recorded_argvs(&on_records);
    let flat: Vec<String> = on.into_iter().flatten().collect();
    assert!(
        flat.iter()
            .any(|a| a.starts_with("-I") && a.contains("zzoptional")),
        "the consumer's compile must see the dependency's include dir.\nargv:\n{flat:#?}"
    );
    assert!(
        flat.iter()
            .any(|a| a.contains("zzoptional") && (a.ends_with(".a") || a.ends_with(".lib"))),
        "the dependency's archive must be a link operand.\nargv:\n{flat:#?}"
    );
}

/// An optional **git** dependency nobody activated is never cloned.
///
/// The URL cannot resolve, so "the build succeeded" is the assertion: if
/// anything queried the source, the build would fail. The second half rules
/// out the alternative explanation -- activating the same dependency must
/// make the build fail, proving the source really is unreachable and that
/// the first build's success came from pruning.
#[test]
fn test_an_inactive_optional_git_dependency_is_never_cloned() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let app = tmp.path().join("gitoptapp");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(app.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();
    let manifest = |default: &str| {
        format!(
            r#"[package]
name = "gitoptapp"
version = "0.1.0"

[dependencies]
zznotcloned = {{ git = "https://harbour.invalid/zznotcloned.git", optional = true }}

[features]
default = [{default}]

[targets.gitoptapp]
kind = "exe"
sources = ["src/main.c"]
"#
        )
    };

    fs::write(app.join("Harbour.toml"), manifest("")).unwrap();
    harbour_run(&home, &app, &["build"]).success();
    assert!(built_exe_path(&app, "gitoptapp").exists());

    // Nothing anywhere under the Harbour home may mention it: no clone, no
    // cache entry, no index file.
    let mentions: Vec<_> = snapshot_tree(&home)
        .into_keys()
        .filter(|p| p.to_string_lossy().contains("zznotcloned"))
        .collect();
    assert!(
        mentions.is_empty(),
        "an inactive optional git dependency left traces in the cache: {mentions:#?}"
    );

    // Activating it must fail, which is what proves the URL was never
    // reachable and the first build's success was the pruning.
    fs::write(app.join("Harbour.toml"), manifest("\"zznotcloned\"")).unwrap();
    let run = harbour_run(&home, &app, &["build"]);
    assert!(
        !run.status.success(),
        "activating an unreachable git dependency must fail; if this passes, the \
         dependency is being ignored rather than pruned\n{run}"
    );
}

/// Activation is unified across the graph, exactly as feature sets are.
///
/// `core` declares `extra` optional. `mid_on` asks for it, `mid_off` does
/// not, and both are in the same build -- so there is one `core` archive and
/// it must be the one with `extra`. Asserted through `core`'s own
/// `when feature = "extra"` define, which reaches the *binary*: a
/// per-dependent activation would give `core` no `HAVE_EXTRA` (the union
/// being computed per edge rather than per package) and the program would
/// print 0.
#[test]
fn test_optional_dependency_activation_is_unified_across_the_whole_graph() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    write_optional_dep(&tmp.path().join("zzoptional"));

    let core = tmp.path().join("core");
    fs::create_dir_all(core.join("include")).unwrap();
    fs::create_dir_all(core.join("src")).unwrap();
    fs::write(
        core.join("Harbour.toml"),
        r#"[package]
name = "core"
version = "0.1.0"

[dependencies]
zzoptional = { path = "../zzoptional", optional = true }

[targets.core]
kind = "staticlib"
sources = ["src/lib.c"]

[targets.core.surface.compile.public]
include_dirs = ["include"]

[[targets.core.when]]
feature = "zzoptional"
defines = ["HAVE_EXTRA=1"]
"#,
    )
    .unwrap();
    fs::write(
        core.join("include/core.h"),
        "#ifndef CORE_H\n#define CORE_H\nint core_extra(void);\n#endif\n",
    )
    .unwrap();
    fs::write(
        core.join("src/lib.c"),
        r#"#include "core.h"
#ifdef HAVE_EXTRA
#include "zzoptional.h"
#endif

int core_extra(void) {
#ifdef HAVE_EXTRA
    return zzoptional_answer();
#else
    return 0;
#endif
}
"#,
    )
    .unwrap();

    // Two intermediate libraries over the same `core`: one asks for the
    // optional dependency's implicit feature, the other says nothing.
    for (name, dep_line) in [
        (
            "mid_on",
            r#"core = { path = "../core", features = ["zzoptional"] }"#,
        ),
        ("mid_off", r#"core = { path = "../core" }"#),
    ] {
        let dir = tmp.path().join(name);
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::create_dir_all(dir.join("include")).unwrap();
        fs::write(
            dir.join("Harbour.toml"),
            format!(
                r#"[package]
name = "{name}"
version = "0.1.0"

[dependencies]
{dep_line}

[targets.{name}]
kind = "staticlib"
sources = ["src/lib.c"]

[targets.{name}.surface.compile.public]
include_dirs = ["include"]
"#
            ),
        )
        .unwrap();
        fs::write(
            dir.join(format!("include/{name}.h")),
            format!("int {name}_value(void);\n"),
        )
        .unwrap();
        fs::write(
            dir.join("src/lib.c"),
            format!(
                "#include \"core.h\"\n#include \"{name}.h\"\nint {name}_value(void) {{ return core_extra(); }}\n"
            ),
        )
        .unwrap();
    }

    let app = tmp.path().join("uniapp");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(
        app.join("Harbour.toml"),
        r#"[package]
name = "uniapp"
version = "0.1.0"

[dependencies]
mid_on = { path = "../mid_on" }
mid_off = { path = "../mid_off" }

[targets.uniapp]
kind = "exe"
sources = ["src/main.c"]
"#,
    )
    .unwrap();
    fs::write(
        app.join("src/main.c"),
        r#"#include <stdio.h>
#include "mid_on.h"
#include "mid_off.h"

int main(void) {
    printf("%d %d\n", mid_on_value(), mid_off_value());
    return 0;
}
"#,
    )
    .unwrap();

    harbour_run(&home, &app, &["build"]).success();

    // One `core` archive, one `zzoptional` archive -- a C graph links one
    // copy of each library, so a per-dependent activation would have to
    // produce two of one of them (or drop the dependency).
    assert!(archive_exists(&app, "zzoptional"));
    let out = Command::new(built_exe_path(&app, "uniapp"))
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "42 42",
        "`mid_off` never asked for the optional dependency, but there is only one \
         `core` in the link, so it gets the same one `mid_on` asked for"
    );
}

/// `[targets.X.deps]` naming an optional dependency that nothing activated
/// is an error, not a silent no-op.
#[test]
fn test_target_deps_naming_an_inactive_optional_dependency_is_an_error() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    write_optional_dep(&tmp.path().join("zzoptional"));

    let app = tmp.path().join("tdapp");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(app.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();
    fs::write(
        app.join("Harbour.toml"),
        r#"[package]
name = "tdapp"
version = "0.1.0"

[dependencies]
zzoptional = { path = "../zzoptional", optional = true }

[targets.tdapp]
kind = "exe"
sources = ["src/main.c"]

[targets.tdapp.deps.zzoptional]
link = "private"
"#,
    )
    .unwrap();

    let run = harbour_run(&home, &app, &["build"]);
    assert!(
        !run.status.success(),
        "naming an inactive optional dependency in `[targets.X.deps]` must fail \
         rather than be ignored\n{run}"
    );
    assert!(
        run.combined().contains("zzoptional"),
        "the diagnostic must name the dependency\n{run}"
    );
}

/// `dep:name` activates without defining a feature of that name, and the
/// suppression of the implicit feature that comes with it is visible from
/// the consumer.
#[test]
fn test_dep_colon_syntax_activates_an_optional_dependency() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    write_optional_dep(&tmp.path().join("zzoptional"));

    let app = tmp.path().join("depcolon");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(
        app.join("src/main.c"),
        "#include <stdio.h>\n#include \"zzoptional.h\"\n\
         int main(void) { printf(\"%d\\n\", zzoptional_answer()); return 0; }\n",
    )
    .unwrap();
    let manifest = r#"[package]
name = "depcolon"
version = "0.1.0"

[dependencies]
zzoptional = { path = "../zzoptional", optional = true }

[features]
default = ["tls"]
tls = ["dep:zzoptional"]

[targets.depcolon]
kind = "exe"
sources = ["src/main.c"]
"#;
    fs::write(app.join("Harbour.toml"), manifest).unwrap();

    harbour_run(&home, &app, &["build"]).success();
    assert!(archive_exists(&app, "zzoptional"));
    let out = Command::new(built_exe_path(&app, "depcolon"))
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "42");

    // ... and because some feature said `dep:zzoptional`, the dependency's
    // own name is no longer a feature: asking for it is an error rather
    // than a second way to switch it on.
    fs::write(
        app.join("Harbour.toml"),
        manifest.replace(r#"default = ["tls"]"#, r#"default = ["zzoptional"]"#),
    )
    .unwrap();
    let run = harbour_run(&home, &app, &["build"]);
    assert!(
        !run.status.success(),
        "`dep:` suppresses the implicit feature, so `zzoptional` must not be a \
         feature name here\n{run}"
    );
    assert!(
        run.combined().contains("unknown feature"),
        "and the diagnostic must say so\n{run}"
    );
}
