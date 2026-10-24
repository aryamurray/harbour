//! Resolver errors as a user sees them: each one provoked from a real manifest
//! and read from the binary's stderr.
//!
//! Kept in a file of its own, not appended to `cli_integration.rs`: that file is
//! 12,000 lines and every PR that appended to its end collided with the next.
//! stderr is a pipe here, so the output is the plain form: no ANSI, no unicode.

use std::fs;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

/// `harbour`, isolated from the developer's home directory and cache.
fn harbour(home: &Path) -> Command {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("harbour"));
    cmd.env("HOME", home)
        .env("XDG_CACHE_HOME", home.join("xdg-cache"))
        .env("XDG_CONFIG_HOME", home.join("xdg-config"))
        .env("XDG_DATA_HOME", home.join("xdg-data"))
        .env("APPDATA", home.join("AppData/Roaming"))
        .env("LOCALAPPDATA", home.join("AppData/Local"))
        .env_remove("NO_COLOR")
        .env_remove("VCPKG_ROOT")
        .env_remove("HARBOUR_TEST_REGISTRY_URL");
    cmd
}

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// A static library package `name` with one source, and `extra` appended to its
/// manifest (dependencies, features).
fn lib(root: &Path, name: &str, version: &str, extra: &str) {
    write(
        root,
        &format!("{name}/Harbour.toml"),
        &format!(
            "[package]\nname = \"{name}\"\nversion = \"{version}\"\n\n{extra}\n\
             [targets.{name}]\nkind = \"staticlib\"\nsources = [\"src/{name}.c\"]\n"
        ),
    );
    write(
        root,
        &format!("{name}/src/{name}.c"),
        &format!("int {name}_fn(void) {{ return 1; }}\n"),
    );
}

/// An executable `app` depending on `deps` (the body of its `[dependencies]`).
fn app(root: &Path, deps: &str) {
    write(
        root,
        "app/Harbour.toml",
        &format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\n{deps}\n\n\
             [targets.app]\nkind = \"exe\"\nsources = [\"src/main.c\"]\n"
        ),
    );
    write(root, "app/src/main.c", "int main(void) { return 0; }\n");
}

/// Build `app` and return what stderr said *about the error*. The build must fail.
///
/// The progress log lines (`INFO ...`, `WARN ...`) are left out: tracing writes
/// them with colour whether or not stderr is a terminal, which is its own
/// inconsistency with the renderer's rules and not what these tests are about.
fn build_fails(root: &Path, home: &Path) -> String {
    let out = harbour(home)
        .current_dir(root.join("app"))
        .arg("build")
        .output()
        .unwrap();
    assert!(!out.status.success(), "the build was expected to fail");
    let text: String = String::from_utf8_lossy(&out.stderr)
        .lines()
        .filter(|l| !(l.contains(" INFO") || l.contains(" WARN")))
        .map(|l| format!("{l}\n"))
        .collect();
    assert!(!text.contains('\x1b'), "a pipe gets no ANSI: {text:?}");
    text
}

#[test]
fn a_cycle_through_optional_features_names_the_loop_the_edges_and_the_way_out() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    lib(
        tmp.path(),
        "a",
        "0.1.0",
        "[dependencies]\nb = { path = \"../b\", optional = true }\n\n[features]\ndefault = [\"b\"]\n",
    );
    lib(
        tmp.path(),
        "b",
        "0.1.0",
        "[dependencies]\na = { path = \"../a\", optional = true }\n\n[features]\ndefault = [\"a\"]\n",
    );
    app(tmp.path(), "a = { path = \"../a\" }");

    let text = build_fails(tmp.path(), &home);
    assert!(text.contains("harbour::resolver::cycle"), "code: {text}");
    assert!(
        text.contains("cycle detected in dependency graph: a -> b -> a")
            || text.contains("cycle detected in dependency graph: b -> a -> b"),
        "the whole loop, not one edge: {text}"
    );
    assert!(
        text.contains("a -> b  (optional dependency, enabled by feature `b` of `a`)"),
        "{text}"
    );
    assert!(
        text.contains("b -> a  (optional dependency, enabled by feature `a` of `b`)"),
        "{text}"
    );
    assert!(
        text.contains("help: this loop exists only because of optional dependencies"),
        "help is its own line: {text}"
    );
    assert!(
        text.contains("turning off any one of `b` of `a` or `a` of `b` breaks it")
            || text.contains("turning off any one of `a` of `b` or `b` of `a` breaks it"),
        "{text}"
    );
}

#[test]
fn a_cycle_of_required_dependencies_says_the_manifests_are_cyclic() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    lib(
        tmp.path(),
        "a",
        "0.1.0",
        "[dependencies]\nb = { path = \"../b\" }\n",
    );
    lib(
        tmp.path(),
        "b",
        "0.1.0",
        "[dependencies]\na = { path = \"../a\" }\n",
    );
    app(tmp.path(), "a = { path = \"../a\" }");

    let text = build_fails(tmp.path(), &home);
    assert!(text.contains("harbour::resolver::cycle"), "{text}");
    assert!(
        text.contains("a -> b  (required dependency)"),
        "required edges are labelled: {text}"
    );
    assert!(
        text.contains("no edge of this loop is optional, so the manifests themselves are cyclic"),
        "{text}"
    );
    assert!(
        !text.contains("turning off"),
        "no feature advice when no edge is optional: {text}"
    );
}

#[test]
fn two_sources_for_one_package_get_a_code_and_the_reason() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    lib(tmp.path(), "s1", "1.0.0", "");
    // Two directories, one package name: two different sources.
    write(
        tmp.path(),
        "s2/Harbour.toml",
        "[package]\nname = \"shared\"\nversion = \"1.0.0\"\n\n[targets.shared]\nkind = \"staticlib\"\nsources = [\"src/s.c\"]\n",
    );
    write(tmp.path(), "s2/src/s.c", "int s(void) { return 1; }\n");
    write(
        tmp.path(),
        "s1/Harbour.toml",
        "[package]\nname = \"shared\"\nversion = \"1.0.0\"\n\n[targets.shared]\nkind = \"staticlib\"\nsources = [\"src/s.c\"]\n",
    );
    write(tmp.path(), "s1/src/s.c", "int s(void) { return 1; }\n");
    lib(
        tmp.path(),
        "a",
        "0.1.0",
        "[dependencies]\nshared = { path = \"../s1\" }\n",
    );
    lib(
        tmp.path(),
        "b",
        "0.1.0",
        "[dependencies]\nshared = { path = \"../s2\" }\n",
    );
    app(
        tmp.path(),
        "a = { path = \"../a\" }\nb = { path = \"../b\" }",
    );

    let text = build_fails(tmp.path(), &home);
    assert!(
        text.contains("harbour::resolver::duplicate_source"),
        "{text}"
    );
    assert!(
        text.contains("package `shared` is available from more than one source"),
        "{text}"
    );
    assert!(
        text.contains("help: Harbour links one copy of each package"),
        "{text}"
    );
}

#[test]
fn an_unsatisfiable_requirement_keeps_the_solvers_reasoning_and_adds_what_to_do() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    lib(tmp.path(), "dep", "1.0.0", "");
    app(
        tmp.path(),
        "dep = { path = \"../dep\", version = \"^2.0\" }",
    );

    let text = build_fails(tmp.path(), &home);
    assert!(text.contains("harbour::resolver::no_solution"), "{text}");
    assert!(text.contains("dependency resolution failed:"), "{text}");
    assert!(
        text.contains("Because there is no version of dep"),
        "the solver's own chain of reasoning is kept: {text}"
    );
    assert!(
        text.contains("help: every package is built once"),
        "and the help says what to do about it: {text}"
    );
}

#[test]
fn a_source_that_cannot_be_read_mid_resolution_is_a_message_not_a_debug_dump() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    // `.invalid` never resolves, so this fails without touching the network. The
    // dependency is *transitive*, so it is reached while the solver is running.
    lib(
        tmp.path(),
        "a",
        "0.1.0",
        "[dependencies]\nghost = { git = \"https://harbour.invalid/ghost.git\" }\n",
    );
    app(tmp.path(), "a = { path = \"../a\" }");

    let text = build_fails(tmp.path(), &home);
    assert!(text.contains("harbour::resolver::failed"), "{text}");
    assert!(
        text.contains("dependency resolution failed: Choosing a version for ghost failed"),
        "{text}"
    );
    assert!(
        text.contains("failed to fetch package `ghost`"),
        "the cause is stated: {text}"
    );
    for debug_dump in ["ErrorChoosingVersion", "PubGrubPackage", "ResolverError("] {
        assert!(
            !text.contains(debug_dump),
            "no `{{:?}}` of solver internals (`{debug_dump}`): {text}"
        );
    }
}
