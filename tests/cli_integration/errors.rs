//! Error rendering and dependency-cycle messages.
#![allow(unused_imports)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::prelude::*;
use predicates::prelude::*;
use tempfile::TempDir;

use harbour::test_support::fixtures::local_registry;

use crate::common::*;

/// Two library packages shaped like FreeType and HarfBuzz: each has an
/// *optional* dependency on the other, switched on by a feature of the same
/// name. Neither is cyclic on its own; the cycle exists only if both features
/// are on.
fn write_optional_pair(root: &Path) {
    for (dir, name, other) in [
        ("ft", "freetype", "harfbuzz"),
        ("hb", "harfbuzz", "freetype"),
    ] {
        let pkg = root.join(dir);
        fs::create_dir_all(pkg.join("src")).unwrap();
        fs::write(
            pkg.join(format!("src/{name}.c")),
            format!("int {name}_value(void) {{ return 1; }}\n"),
        )
        .unwrap();
        fs::write(
            pkg.join("Harbour.toml"),
            format!(
                "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n\n\
                 [dependencies]\n{other} = {{ path = \"../{}\", optional = true }}\n\n\
                 [targets.{name}]\nkind = \"staticlib\"\nsources = [\"src/{name}.c\"]\n",
                if dir == "ft" { "hb" } else { "ft" }
            ),
        )
        .unwrap();
    }
}

fn write_pair_consumer(root: &Path, freetype_features: &str, harfbuzz_features: &str) -> PathBuf {
    let app = root.join("app");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(app.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();
    fs::write(
        app.join("Harbour.toml"),
        format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
             [dependencies]\n\
             freetype = {{ path = \"../ft\"{freetype_features} }}\n\
             harfbuzz = {{ path = \"../hb\"{harfbuzz_features} }}\n\n\
             [targets.app]\nkind = \"exe\"\nsources = [\"src/main.c\"]\n"
        ),
    )
    .unwrap();
    app
}

/// With both optional features on, the error names the whole loop, says each
/// edge is optional and which feature enabled it, and says turning one off
/// breaks the cycle. It used to name one edge (`freetype -> harfbuzz`) and say
/// nothing about why.
#[test]
fn test_a_cycle_through_optional_features_names_the_loop_and_the_features() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    write_optional_pair(tmp.path());
    let app = write_pair_consumer(
        tmp.path(),
        ", features = [\"harfbuzz\"]",
        ", features = [\"freetype\"]",
    );

    let run = harbour_run(&home, &app, &["build"]);
    assert!(!run.status.success());
    let text = run.to_string();
    assert!(
        text.contains("cycle detected in dependency graph: freetype -> harfbuzz -> freetype"),
        "the whole loop, closed, not one edge\n{text}"
    );
    assert!(
        text.contains(
            "freetype -> harfbuzz  (optional dependency, enabled by feature `harfbuzz` of `freetype`)"
        ),
        "first edge, with the feature that enabled it\n{text}"
    );
    assert!(
        text.contains(
            "harfbuzz -> freetype  (optional dependency, enabled by feature `freetype` of `harfbuzz`)"
        ),
        "second edge, with the feature that enabled it\n{text}"
    );
    assert!(
        text.contains(
            "turning off any one of `harfbuzz` of `freetype` or `freetype` of `harfbuzz`"
        ),
        "the hint must name the features to turn off\n{text}"
    );
}

/// Only one direction on is not a cycle, and still builds: the error is about
/// the loop, not about optional dependencies.
#[test]
fn test_one_direction_of_the_optional_pair_still_builds() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    write_optional_pair(tmp.path());
    let app = write_pair_consumer(tmp.path(), "", ", features = [\"freetype\"]");

    harbour_run(&home, &app, &["build"]).success();
}

/// A cycle with no optional edge says so, and does not tell the author to turn
/// a feature off: there is none to turn off.
#[test]
fn test_a_cycle_of_required_dependencies_says_the_manifests_are_cyclic() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    for (dir, name, other, other_dir) in
        [("a", "pkg-a", "pkg-b", "b"), ("b", "pkg-b", "pkg-a", "a")]
    {
        let pkg = tmp.path().join(dir);
        fs::create_dir_all(pkg.join("src")).unwrap();
        fs::write(pkg.join("src/lib.c"), "int f(void) { return 1; }\n").unwrap();
        fs::write(
            pkg.join("Harbour.toml"),
            format!(
                "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n\n\
                 [dependencies]\n{other} = {{ path = \"../{other_dir}\" }}\n\n\
                 [targets.{name}]\nkind = \"staticlib\"\nsources = [\"src/lib.c\"]\n"
            ),
        )
        .unwrap();
    }

    let run = harbour_run(&home, &tmp.path().join("a"), &["build"]);
    assert!(!run.status.success());
    let text = run.to_string();
    assert!(
        text.contains("cycle detected in dependency graph: pkg-a -> pkg-b -> pkg-a"),
        "{text}"
    );
    assert!(
        text.contains("pkg-a -> pkg-b  (required dependency)"),
        "{text}"
    );
    assert!(
        text.contains("pkg-b -> pkg-a  (required dependency)"),
        "{text}"
    );
    assert!(
        text.contains("no edge of this loop is optional, so the manifests themselves are cyclic"),
        "{text}"
    );
    assert!(
        !text.contains("turning off"),
        "there is no feature to turn off in a plain cycle\n{text}"
    );
}

/// One required edge and one optional one: only the optional one can be turned
/// off, so only it is named.
#[test]
fn test_a_mixed_cycle_names_only_the_optional_edge_to_turn_off() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    write_optional_pair(tmp.path());
    // freetype now *requires* harfbuzz; harfbuzz still has the optional way back.
    let manifest = fs::read_to_string(tmp.path().join("ft/Harbour.toml")).unwrap();
    fs::write(
        tmp.path().join("ft/Harbour.toml"),
        manifest.replace(", optional = true", ""),
    )
    .unwrap();
    let app = write_pair_consumer(tmp.path(), "", ", features = [\"freetype\"]");

    let run = harbour_run(&home, &app, &["build"]);
    assert!(!run.status.success());
    let text = run.to_string();
    assert!(
        text.contains("freetype -> harfbuzz  (required dependency)"),
        "{text}"
    );
    assert!(
        text.contains(
            "harfbuzz -> freetype  (optional dependency, enabled by feature `freetype` of `harfbuzz`)"
        ),
        "{text}"
    );
    assert!(
        text.contains("turning off any one of `freetype` of `harfbuzz` breaks it"),
        "only the optional edge can be turned off\n{text}"
    );
}

/// A project whose manifest misspells `sources`, and the stderr of building it.
fn misspelled_key_stderr(
    home: &Path,
    tmp: &TempDir,
    extra_args: &[&str],
    envs: &[(&str, &str)],
) -> String {
    let app = tmp.path().join("app");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(app.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();
    fs::write(
        app.join("Harbour.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[targets.app]\nkind = \"exe\"\nsorces = [\"src/main.c\"]\n",
    )
    .unwrap();
    let mut cmd = harbour(home);
    cmd.current_dir(&app)
        .arg("build")
        .args(extra_args)
        .env_remove("NO_COLOR");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    assert!(
        !out.status.success(),
        "a misspelled key must fail the build"
    );
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// A manifest parse error underlines the key the parser blamed, names the file
/// relative to where you are, and -- piped, as in CI and in every test -- is
/// plain text: no colour, no box-drawing.
///
/// Before, it printed an absolute path and a single `^` under the first
/// character of the line, whatever the parser had actually blamed.
#[test]
fn test_a_manifest_error_underlines_the_key_and_is_plain_when_piped() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let stderr = misspelled_key_stderr(&home, &tmp, &[], &[]);

    assert!(stderr.contains("unknown field `sorces`"), "{stderr}");
    assert!(stderr.contains("Harbour.toml:7:1"), "location: {stderr}");
    assert!(
        !stderr.contains(&tmp.path().display().to_string()),
        "the path is shown relative to the current directory, not absolute: {stderr}"
    );
    assert!(
        stderr.contains("sorces = [\"src/main.c\"]"),
        "the source line: {stderr}"
    );

    // As wide as the key (`sorces`), not a lone caret.
    let underline = stderr
        .lines()
        .find(|l| l.contains('^'))
        .expect("an underline");
    let width = underline.chars().filter(|c| matches!(c, '^' | '|')).count();
    assert_eq!(width, "sorces".len(), "the key is underlined: {stderr}");

    assert!(!stderr.contains('\x1b'), "no ANSI when piped: {stderr:?}");
    assert!(
        !stderr
            .chars()
            .any(|c| ('\u{2500}'..='\u{257f}').contains(&c)),
        "no box-drawing when piped: {stderr}"
    );
    assert!(
        stderr.contains("expected one of"),
        "the valid keys are offered: {stderr}"
    );
}

/// `--color always` is an explicit request and wins over the pipe.
#[test]
fn test_color_always_colours_a_piped_error() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let stderr = misspelled_key_stderr(&home, &tmp, &["--color", "always"], &[]);
    assert!(stderr.contains("\x1b["), "colour was requested: {stderr:?}");
}

/// A cause chain is listed, not flattened into `a: b: c`.
#[test]
fn test_an_error_with_causes_lists_them() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let app = tmp.path().join("app");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(app.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();
    fs::write(
        app.join("Harbour.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nnope = { path = \"../does-not-exist\" }\n\n\
         [targets.app]\nkind = \"exe\"\nsources = [\"src/main.c\"]\n",
    )
    .unwrap();
    let out = harbour(&home)
        .current_dir(&app)
        .arg("build")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(stderr.contains("error: "), "{stderr}");
    assert!(
        stderr.contains("does-not-exist"),
        "the cause names what is missing: {stderr}"
    );
}

/// On a terminal, errors are drawn with unicode and colour -- unless `NO_COLOR`
/// is set, which turns the colour off and leaves the drawing.
///
/// Needs a pseudo-terminal, which `script` provides; skipped where it is not
/// available or spelled differently.
#[test]
#[cfg(unix)]
fn test_no_color_is_honoured_on_a_terminal() {
    if Command::new("script").arg("--version").output().is_err() && !cfg!(target_os = "macos") {
        eprintln!("skipping: no `script` to provide a terminal");
        return;
    }
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let app = tmp.path().join("app");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(app.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();
    fs::write(
        app.join("Harbour.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[targets.app]\nkind = \"exe\"\nsorces = [\"src/main.c\"]\n",
    )
    .unwrap();
    let bin = assert_cmd::cargo::cargo_bin!("harbour").to_path_buf();

    let on_a_terminal = |no_color: bool| -> Option<String> {
        let mut cmd = Command::new("script");
        if cfg!(target_os = "macos") {
            cmd.args(["-q", "/dev/null"]).arg(&bin).arg("build");
        } else {
            cmd.args(["-qec", &format!("{} build", bin.display()), "/dev/null"]);
        }
        cmd.current_dir(&app)
            .env("HOME", &home)
            .env("XDG_CACHE_HOME", home.join("xdg-cache"))
            .env("TERM", "xterm-256color")
            .env_remove("CI");
        if no_color {
            cmd.env("NO_COLOR", "1");
        } else {
            cmd.env_remove("NO_COLOR");
        }
        let out = cmd.output().ok()?;
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    };

    let (Some(coloured), Some(plain)) = (on_a_terminal(false), on_a_terminal(true)) else {
        eprintln!("skipping: could not run under a terminal");
        return;
    };
    if !coloured.contains("sorces") {
        eprintln!("skipping: the harness did not provide a terminal: {coloured:?}");
        return;
    }

    assert!(
        coloured.contains("\x1b["),
        "a terminal gets colour: {coloured:?}"
    );
    assert!(
        coloured
            .chars()
            .any(|c| ('\u{2500}'..='\u{257f}').contains(&c)),
        "and unicode drawing: {coloured}"
    );
    assert!(
        !plain.contains("\x1b["),
        "NO_COLOR removes the colour on a terminal: {plain:?}"
    );
    assert!(
        plain
            .chars()
            .any(|c| ('\u{2500}'..='\u{257f}').contains(&c)),
        "but keeps the unicode drawing: {plain}"
    );
}
