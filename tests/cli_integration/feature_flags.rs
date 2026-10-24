//! `--features`, `--no-default-features` and `--all-features` on the command line.
#![allow(unused_imports)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::prelude::*;
use predicates::prelude::*;
use tempfile::TempDir;

use harbour::test_support::fixtures::local_registry;

use crate::common::*;

/// An `app` with features `alpha` (default) and `beta`, an optional dependency
/// `extra` (whose implicit feature is `extra`), a `when feature = "beta"`
/// define, and a generator that dumps its environment so a test can see exactly
/// what it was told.
///
/// Returns `(home, app)`.
fn write_feature_app(tmp: &TempDir) -> (PathBuf, PathBuf) {
    let home = harbour_home(tmp);

    let extra = tmp.path().join("extra");
    fs::create_dir_all(extra.join("src")).unwrap();
    fs::write(
        extra.join("src/extra.c"),
        "int extra_fn(void) { return 1; }\n",
    )
    .unwrap();
    fs::write(
        extra.join("Harbour.toml"),
        "[package]\nname = \"extra\"\nversion = \"0.1.0\"\n\n\
         [targets.extra]\nkind = \"staticlib\"\nsources = [\"src/extra.c\"]\n",
    )
    .unwrap();

    let app = tmp.path().join("app");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(app.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();
    let prebuild = write_env_dumping_generator(&app, "");
    fs::write(
        app.join("Harbour.toml"),
        format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
             [dependencies]\nextra = {{ path = \"../extra\", optional = true }}\n\n\
             [features]\ndefault = [\"alpha\"]\nalpha = []\nbeta = []\n\n\
             [targets.app]\nkind = \"exe\"\nsources = [\"src/main.c\"]\n\n\
             [[targets.app.when]]\nfeature = \"beta\"\ndefines = [\"WITH_BETA\"]\n\n\
             {prebuild}"
        ),
    )
    .unwrap();
    (home, app)
}

/// What the generator was told (`HARBOUR_FEATURES`) after building `app` with
/// `args`.
fn features_the_generator_saw(home: &Path, app: &Path, args: &[&str]) -> String {
    let mut full = vec!["build"];
    full.extend_from_slice(args);
    harbour_run(home, app, &full).success();
    let dump = fs::read_to_string(app.join("generated/env.txt")).unwrap();
    env_dump_get(&dump, "HARBOUR_FEATURES").expect("HARBOUR_FEATURES is always set")
}

fn extra_was_built(app: &Path) -> bool {
    let archive = if cfg!(windows) {
        "extra.lib"
    } else {
        "libextra.a"
    };
    walk_files(&app.join(".harbour"))
        .iter()
        .any(|p| p.file_name().is_some_and(|n| n == archive))
}

#[test]
fn test_without_feature_flags_a_build_is_what_it_always_was() {
    let tmp = temp_dir();
    let (home, app) = write_feature_app(&tmp);
    assert_eq!(
        features_the_generator_saw(&home, &app, &[]),
        "alpha,default"
    );
    assert!(
        !extra_was_built(&app),
        "an optional dependency nothing activated is not built"
    );
}

#[test]
fn test_features_flag_adds_to_the_defaults() {
    let tmp = temp_dir();
    let (home, app) = write_feature_app(&tmp);
    assert_eq!(
        features_the_generator_saw(&home, &app, &["--features", "beta"]),
        "alpha,beta,default"
    );
    // `-F` is the short spelling and the list is comma-separated.
    assert_eq!(
        features_the_generator_saw(&home, &app, &["-F", "beta,alpha"]),
        "alpha,beta,default"
    );
}

#[test]
fn test_no_default_features_drops_the_default_feature_set() {
    let tmp = temp_dir();
    let (home, app) = write_feature_app(&tmp);
    assert_eq!(
        features_the_generator_saw(&home, &app, &["--no-default-features"]),
        ""
    );
    assert_eq!(
        features_the_generator_saw(
            &home,
            &app,
            &["--no-default-features", "--features", "beta"]
        ),
        "beta"
    );
}

#[test]
fn test_all_features_enables_every_declared_feature_including_an_optional_dependency() {
    let tmp = temp_dir();
    let (home, app) = write_feature_app(&tmp);
    assert_eq!(
        features_the_generator_saw(&home, &app, &["--all-features"]),
        "alpha,beta,default,extra"
    );
    assert!(
        extra_was_built(&app),
        "--all-features activates the optional dependency"
    );
}

/// A feature flag reaches `when feature = "..."` blocks too: they read the same
/// set the generator is told.
#[test]
fn test_a_feature_flag_reaches_conditional_blocks_as_well_as_generators() {
    let tmp = temp_dir();
    let (home, app) = write_feature_app(&tmp);

    let off = harbour_run(&home, &app, &["flags", "app", "--compile"]);
    assert!(off.status.success(), "{off}");
    assert!(
        !off.to_string().contains("WITH_BETA"),
        "beta is off by default\n{off}"
    );

    let on = harbour_run(
        &home,
        &app,
        &["flags", "app", "--compile", "--features", "beta"],
    );
    assert!(on.status.success(), "{on}");
    assert!(
        on.to_string().contains("WITH_BETA"),
        "`harbour flags` must report the flag the build would pass\n{on}"
    );

    // And the build agrees with what `flags` printed.
    assert!(features_the_generator_saw(&home, &app, &["--features", "beta"]).contains("beta"));
}

#[test]
fn test_an_unknown_feature_is_an_error_naming_the_package_and_what_it_declares() {
    let tmp = temp_dir();
    let (home, app) = write_feature_app(&tmp);
    let run = harbour_run(&home, &app, &["build", "--features", "nope"]);
    assert!(!run.status.success());
    let text = run.to_string();
    assert!(
        text.contains("feature `nope` is not declared by package `app`")
            && text.contains("alpha, beta, default, extra"),
        "expected the error to name the package and list its features\n{text}"
    );
}

#[test]
fn test_the_qualified_package_slash_feature_form_is_refused_with_the_way_to_say_it() {
    let tmp = temp_dir();
    let (home, app) = write_feature_app(&tmp);
    let run = harbour_run(&home, &app, &["build", "--features", "app/beta"]);
    assert!(!run.status.success());
    assert!(
        run.to_string().contains("qualified") && run.to_string().contains("-p"),
        "expected a pointer to `-p`\n{run}"
    );
}

/// The point of the lockfile hash: a selection that activates an optional
/// dependency changes the *graph*, so it must not find the lockfile fresh and
/// build the graph without the dependency it asked for -- in either direction.
#[test]
fn test_a_feature_flag_that_activates_an_optional_dependency_relocks_in_both_directions() {
    let tmp = temp_dir();
    let (home, app) = write_feature_app(&tmp);
    let lock_names_extra = |app: &Path| {
        fs::read_to_string(app.join("Harbour.lock"))
            .map(|l| l.contains("name = \"extra\""))
            .unwrap_or(false)
    };

    harbour_run(&home, &app, &["build"]).success();
    assert!(
        !lock_names_extra(&app) && !extra_was_built(&app),
        "starts without `extra`"
    );

    harbour_run(&home, &app, &["build", "--features", "extra"]).success();
    assert!(
        lock_names_extra(&app),
        "`--features extra` must put `extra` in the graph"
    );
    assert!(extra_was_built(&app), "and build it");

    harbour_run(&home, &app, &["build"]).success();
    assert!(
        !lock_names_extra(&app),
        "dropping the flag must re-resolve, not keep serving the last selection's graph"
    );
}

#[test]
fn test_tree_shows_the_graph_the_selected_features_produce() {
    let tmp = temp_dir();
    let (home, app) = write_feature_app(&tmp);
    let plain = harbour_run(&home, &app, &["tree"]);
    assert!(plain.status.success(), "{plain}");
    assert!(!plain.to_string().contains("extra"), "{plain}");

    let with = harbour_run(&home, &app, &["tree", "--features", "extra"]);
    assert!(with.status.success(), "{with}");
    assert!(
        with.to_string().contains("extra"),
        "`tree --features extra` must show it\n{with}"
    );
}

/// In a workspace the flags speak for every selected package, and a package
/// that does not declare a requested feature simply does not get it (an error
/// only when *no* selected package declares it), as in Cargo.
#[test]
fn test_features_apply_to_each_selected_package_that_declares_them() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let ws = tmp.path().join("ws");
    fs::create_dir_all(&ws).unwrap();
    fs::write(
        ws.join("Harbour.toml"),
        "[workspace]\nmembers = [\"a\", \"b\"]\n",
    )
    .unwrap();

    for (name, features) in [("a", "beta = []\nonly_a = []\n"), ("b", "beta = []\n")] {
        let dir = ws.join(name);
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();
        let prebuild = write_env_dumping_generator(&dir, "")
            .replace("targets.app", &format!("targets.{name}"));
        fs::write(
            dir.join("Harbour.toml"),
            format!(
                "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n\n[features]\n{features}\n\
                 [targets.{name}]\nkind = \"exe\"\nsources = [\"src/main.c\"]\n\n{prebuild}"
            ),
        )
        .unwrap();
    }
    let saw = |name: &str| {
        let dump = fs::read_to_string(ws.join(name).join("generated/env.txt")).unwrap();
        env_dump_get(&dump, "HARBOUR_FEATURES").unwrap()
    };

    harbour_run(&home, &ws, &["build", "--features", "beta,only_a"]).success();
    assert_eq!(saw("a"), "beta,only_a", "`a` declares both");
    assert_eq!(
        saw("b"),
        "beta",
        "`b` declares only `beta`, and is not an error for it"
    );

    // Narrowed with -p, the flags speak only for that package.
    harbour_run(&home, &ws, &["build", "-p", "b", "--features", "beta"]).success();
    assert_eq!(saw("b"), "beta");

    let none = harbour_run(&home, &ws, &["build", "--features", "nowhere"]);
    assert!(!none.status.success());
    assert!(
        none.to_string().contains("any selected package")
            && none.to_string().contains("package `a` declares")
            && none.to_string().contains("package `b` declares"),
        "an error only when no selected package declares it, naming each\n{none}"
    );
}

/// `--locked` compares against the selection the lockfile was made with: the
/// same flags pass, different ones are "lockfile would change", never a quiet
/// build of the wrong graph.
#[test]
fn test_locked_builds_respect_the_feature_selection_the_lockfile_was_made_with() {
    let tmp = temp_dir();
    let (home, app) = write_feature_app(&tmp);

    harbour_run(&home, &app, &["build", "--features", "extra"]).success();
    harbour_run(&home, &app, &["build", "--locked", "--features", "extra"]).success();

    let plain = harbour_run(&home, &app, &["build", "--locked"]);
    assert!(
        !plain.status.success(),
        "a different selection must not pass --locked\n{plain}"
    );
    assert!(
        plain.to_string().contains("lockfile would change"),
        "expected the usual --locked message\n{plain}"
    );
}
