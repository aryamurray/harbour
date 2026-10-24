//! Packages resolved from a registry, `harbour search`, shim patches.
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
fn test_add_registry_dependency() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    // Build a real, local, git-backed registry fixture that is guaranteed
    // not to contain "somepkg" (or any other package). This lets us
    // exercise harbour's real registry-lookup code path without any
    // network access or dependence on the contents of the real,
    // network-hosted default registry.
    let registry_dir = tmp.path().join("fixture-registry");
    local_registry::init(&registry_dir).expect("failed to init fixture registry");
    let registry_url = local_registry::file_url(&registry_dir);

    harbour(&home)
        .args(["new", "addtest"])
        .current_dir(tmp.path())
        .assert()
        .success();

    let project_dir = tmp.path().join("addtest");

    // Adding without --path or --git should error if not found and vcpkg is not configured.
    //
    // NOTE: on Windows, `harbour add <unknown-package>` is known to exit 0
    // instead of non-zero here (a real, pre-existing CLI defect unrelated
    // to registry hermeticity -- see project tracking for the concurrent
    // investigation). This test intentionally does NOT `#[cfg]`-gate or
    // `#[ignore]` around that: it should keep failing on Windows until the
    // underlying exit-code bug is fixed. Do not "fix" this test by loosening
    // the `.failure()` assertion.
    harbour(&home)
        .env("HARBOUR_TEST_REGISTRY_URL", &registry_url)
        .args(["add", "somepkg"])
        .current_dir(&project_dir)
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found in registries"))
        .stderr(predicate::str::contains("vcpkg is not configured"));
}

/// Write one package into a registry directory the way the resolver reads it:
/// a tier-1 `index/<letter>/<name>.idx` listing every version, and (for the
/// versions given a category) a tier-2 shim `index/<letter>/<name>/<version>.toml`.
///
/// Uses the library's own `write_index_file`, so the layout under test is the
/// one `harbour add` reads rather than a second description of it.
fn write_registry_package(
    registry: &Path,
    name: &str,
    versions: &[(&str, bool)],
    category: Option<&str>,
) {
    use harbour::sources::registry::index::{
        write_index_file, IndexRecord, CURRENT_FORMAT_VERSION,
    };

    let letter = &name[..1];
    let records: Vec<IndexRecord> = versions
        .iter()
        .map(|(version, yanked)| IndexRecord {
            format_version: CURRENT_FORMAT_VERSION,
            name: name.to_string(),
            version: version.to_string(),
            yanked: *yanked,
            deps: Vec::new(),
            checksum: None,
            shim: format!("{letter}/{name}/{version}.toml"),
        })
        .collect();
    write_index_file(
        &registry
            .join("index")
            .join(letter)
            .join(format!("{name}.idx")),
        &records,
    )
    .unwrap();

    if let Some(category) = category {
        for (version, _) in versions {
            let dir = registry.join("index").join(letter).join(name);
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                dir.join(format!("{version}.toml")),
                format!(
                    "[package]\nname = \"{name}\"\nversion = \"{version}\"\n\n\
                     [metadata]\ncategory = \"{category}\"\n"
                ),
            )
            .unwrap();
        }
    }
}

fn search_output(home: &Path, registry: &Path, query: &str) -> String {
    let out = harbour(home)
        .args(["search", query, "--registry-path"])
        .arg(registry)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "search must succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// `harbour search` reads the registry's real two-tier layout.
///
/// It used to walk `index/<letter>/<package>/` directories -- the tier-2 shim
/// directories only -- so a package whose shim directory was absent, or a
/// registry that carries just the tier-1 `.idx` files, printed "No packages
/// found" for everything. Here `libpng` and `curl` have only a tier-1 file.
#[test]
fn test_search_finds_packages_listed_in_the_tier_one_index() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let registry = tmp.path().join("registry");
    write_registry_package(
        &registry,
        "zlib",
        &[("1.2.13", false), ("1.3.1", false)],
        Some("compression"),
    );
    write_registry_package(&registry, "libpng", &[("1.6.50", false)], None);
    write_registry_package(&registry, "curl", &[("8.22.0", false)], None);

    let all = search_output(&home, &registry, "l");
    assert!(all.contains("Found 3 packages"), "{all}");
    // Sorted by name, latest available version, category when the shim has one.
    let curl = all.find("curl v8.22.0").expect("curl listed");
    let libpng = all.find("libpng v1.6.50").expect("libpng listed");
    let zlib = all
        .find("zlib v1.3.1 [compression]")
        .expect("zlib at its latest version");
    assert!(curl < libpng && libpng < zlib, "sorted by name: {all}");

    // A query matches anywhere in the name, not only at its start: `lib` is in
    // both `libpng` and `zlib`.
    let substring = search_output(&home, &registry, "lib");
    assert!(
        substring.contains("Found 2 packages matching 'lib'"),
        "{substring}"
    );
    assert!(
        substring.contains("libpng v1.6.50") && substring.contains("zlib v1.3.1"),
        "{substring}"
    );

    let narrow = search_output(&home, &registry, "png");
    assert!(
        narrow.contains("Found 1 package matching 'png'"),
        "{narrow}"
    );
    assert!(
        narrow.contains("libpng v1.6.50") && !narrow.contains("zlib"),
        "{narrow}"
    );
}

#[test]
fn test_search_reports_no_match_and_hides_nothing_about_yanked_packages() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let registry = tmp.path().join("registry");
    write_registry_package(&registry, "zlib", &[("1.3.1", false)], None);
    // The newest version is yanked: search offers the newest one a new
    // resolution would use, not the yanked one.
    write_registry_package(
        &registry,
        "sqlite",
        &[("3.44.0", false), ("3.45.0", true)],
        None,
    );
    // Every version yanked: still shown, flagged, so a package never looks
    // like it does not exist.
    write_registry_package(&registry, "oldlib", &[("0.1.0", true)], None);

    let none = search_output(&home, &registry, "nonexistent");
    assert!(
        none.contains("No packages found matching 'nonexistent'"),
        "{none}"
    );

    let sqlite = search_output(&home, &registry, "sqlite");
    assert!(
        sqlite.contains("sqlite v3.44.0") && !sqlite.contains("3.45.0"),
        "{sqlite}"
    );

    let oldlib = search_output(&home, &registry, "oldlib");
    assert!(
        oldlib.contains("oldlib v0.1.0") && oldlib.contains("yanked"),
        "{oldlib}"
    );
}

//
// Every earlier test of the registry path asked whether a package was *found*.
// None built a package from one. These do, with small real C libraries in a
// real git-backed registry (`local_registry`), offline.
/// One package for a fixture registry: a static library whose public headers
/// live beside its source, with `deps_toml` and `extra_toml` spliced into its
/// manifest. Returns the directory it was written to.
fn write_fixture_lib(
    root: &Path,
    name: &str,
    deps_toml: &str,
    extra_toml: &str,
    files: &[(&str, &str)],
) -> PathBuf {
    let dir = root.join(format!("src-{name}"));
    fs::create_dir_all(&dir).unwrap();
    for (file, contents) in files {
        fs::write(dir.join(file), contents).unwrap();
    }
    fs::write(
        dir.join("Harbour.toml"),
        format!(
            "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n\n[dependencies]\n{deps_toml}\n\n\
             {extra_toml}\n\
             [targets.{name}]\nkind = \"staticlib\"\nsources = [\"{name}.c\"]\n\n\
             [targets.{name}.surface.compile.public]\ninclude_dirs = [\".\"]\n"
        ),
    )
    .unwrap();
    dir
}

/// Publish `name` 0.1.0 into `registry`: commit its source tree as a git
/// repository and add a shim pinning that commit.
fn publish_package(registry: &Path, name: &str, source: &Path) {
    local_registry::add_package(registry, name, "0.1.0", source).unwrap();
}

/// An application that depends on `deps_toml` and calls `call` (a C expression
/// of type `int`), printing the result.
fn write_registry_app(app: &Path, deps_toml: &str, headers: &[&str], call: &str) {
    fs::create_dir_all(app.join("src")).unwrap();
    let includes: String = headers
        .iter()
        .map(|h| format!("#include \"{h}\"\n"))
        .collect();
    fs::write(
        app.join("src/main.c"),
        format!(
            "#include <stdio.h>\n{includes}int main(void) {{ printf(\"%d\\n\", {call}); return 0; }}\n"
        ),
    )
    .unwrap();
    fs::write(
        app.join("Harbour.toml"),
        format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\n{deps_toml}\n\n\
             [targets.app]\nkind = \"exe\"\nsources = [\"src/main.c\"]\n"
        ),
    )
    .unwrap();
}

fn build_in_registry(home: &Path, app: &Path, registry_url: &str) -> RunLog {
    harbour_run_env(
        home,
        app,
        &["build"],
        &[("HARBOUR_TEST_REGISTRY_URL", registry_url)],
    )
}

fn run_app(app: &Path) -> String {
    let out = Command::new(built_exe_path(app, "app")).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// How many `[[package]]` entries of the lockfile carry this name.
fn lock_entries(app: &Path, name: &str) -> usize {
    fs::read_to_string(app.join("Harbour.lock"))
        .unwrap()
        .matches(&format!("name = \"{name}\""))
        .count()
}

/// `libbase.a` or `base.lib`, whichever this platform's toolchain writes.
fn archive_count(app: &Path, lib: &str) -> usize {
    let unix = format!("lib{lib}.a");
    let msvc = format!("{lib}.lib");
    walk_files(&app.join(".harbour"))
        .into_iter()
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n == unix.as_str() || n == msvc.as_str())
        })
        .count()
}

/// A registry package's dependency on another package in the **same**
/// registry resolves and links, written the natural way (`base = "^0.1"`).
///
/// `mid` depends on `base` with no `registry = ...` key. That used to resolve
/// against the built-in *public* registry whatever registry `mid` was
/// published in, so on any other registry -- a private one, this fixture -- the
/// build failed with "no version of base" before it compiled anything. Nested
/// *git* dependencies were tested; nested *registry* ones never were.
#[test]
fn test_a_registry_package_depends_on_a_package_in_the_same_registry() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let registry = tmp.path().join("registry");
    local_registry::init(&registry).unwrap();

    let base = write_fixture_lib(
        tmp.path(),
        "base",
        "",
        "",
        &[
            ("base.h", "int base_value(void);\n"),
            ("base.c", "int base_value(void) { return 40; }\n"),
        ],
    );
    publish_package(&registry, "base", &base);
    let mid = write_fixture_lib(
        tmp.path(),
        "mid",
        "base = \"^0.1\"",
        "",
        &[
            ("mid.h", "#include \"base.h\"\nint mid_value(void);\n"),
            (
                "mid.c",
                "#include \"mid.h\"\nint mid_value(void) { return base_value() + 1; }\n",
            ),
        ],
    );
    publish_package(&registry, "mid", &mid);
    local_registry::publish(&registry, &tmp.path().join("scratch")).unwrap();

    let app = tmp.path().join("app");
    write_registry_app(&app, "mid = \"^0.1\"", &["mid.h"], "mid_value()");
    build_in_registry(&home, &app, &local_registry::file_url(&registry)).success();

    assert_eq!(
        run_app(&app),
        "41",
        "the program must link both registry packages"
    );
    let lock = fs::read_to_string(app.join("Harbour.lock")).unwrap();
    assert!(
        lock.contains("dependencies = [\"base 0.1.0\"]"),
        "`mid` must depend on `base` in the lockfile\n{lock}"
    );
    assert_eq!(lock.matches("source = \"registry+").count(), 2, "{lock}");
}

/// A package reached twice -- directly and through another registry package --
/// is one package: one lockfile entry and one archive on the link line.
///
/// This is FreeType's shape (zlib directly and through libpng). Two copies of
/// a C library in one link are duplicate symbols at best and two diverging
/// copies of its state at worst.
#[test]
fn test_a_registry_package_reached_twice_is_built_and_linked_once() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let registry = tmp.path().join("registry");
    local_registry::init(&registry).unwrap();

    for (name, deps, files) in [
        (
            "base",
            "",
            vec![
                ("base.h", "int base_value(void);\n"),
                ("base.c", "int base_value(void) { return 40; }\n"),
            ],
        ),
        (
            "mid",
            "base = \"^0.1\"",
            vec![
                ("mid.h", "#include \"base.h\"\nint mid_value(void);\n"),
                ("mid.c", "#include \"mid.h\"\nint mid_value(void) { return base_value() + 1; }\n"),
            ],
        ),
        (
            "top",
            "base = \"^0.1\"\nmid = \"^0.1\"",
            vec![
                ("top.h", "#include \"base.h\"\n#include \"mid.h\"\nint top_value(void);\n"),
                (
                    "top.c",
                    "#include \"top.h\"\nint top_value(void) { return base_value() + mid_value(); }\n",
                ),
            ],
        ),
    ] {
        let dir = write_fixture_lib(tmp.path(), name, deps, "", &files);
        publish_package(&registry, name, &dir);
    }
    local_registry::publish(&registry, &tmp.path().join("scratch")).unwrap();

    let app = tmp.path().join("app");
    write_registry_app(&app, "top = \"^0.1\"", &["top.h"], "top_value()");
    build_in_registry(&home, &app, &local_registry::file_url(&registry)).success();

    assert_eq!(run_app(&app), "81");
    assert_eq!(
        lock_entries(&app, "base"),
        1,
        "one lockfile entry for the shared package"
    );
    assert_eq!(
        archive_count(&app, "base"),
        1,
        "one archive, so one copy in the link"
    );
}

/// An optional registry dependency nobody activated is never fetched, and
/// activating it fetches it.
///
/// `extra`'s source URL is unreachable *after* the index was built, so "the
/// build succeeded" can only mean it was never touched. The second half rules
/// out the other explanation: with the feature on the same build must fail on
/// that very package, proving the first success came from pruning.
#[test]
fn test_an_inactive_optional_registry_dependency_is_never_fetched() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let registry = tmp.path().join("registry");
    local_registry::init(&registry).unwrap();

    let extra = write_fixture_lib(
        tmp.path(),
        "extra",
        "",
        "",
        &[
            ("extra.h", "int extra_value(void);\n"),
            ("extra.c", "int extra_value(void) { return 5; }\n"),
        ],
    );
    publish_package(&registry, "extra", &extra);
    let opttop = write_fixture_lib(
        tmp.path(),
        "opttop",
        "extra = { version = \"^0.1\", optional = true }",
        "[features]\ndefault = []\n",
        &[
            ("opttop.h", "int opttop_value(void);\n"),
            ("opttop.c", "int opttop_value(void) { return 7; }\n"),
        ],
    );
    publish_package(&registry, "opttop", &opttop);
    local_registry::publish(&registry, &tmp.path().join("scratch")).unwrap();

    // Now make `extra` unfetchable. The index (which records it as a dependency
    // of `opttop`) is unchanged; only its source location is gone.
    let shim = registry.join("index/e/extra/0.1.0.toml");
    let text = fs::read_to_string(&shim).unwrap();
    let rev = text
        .lines()
        .find_map(|l| l.strip_prefix("rev = \""))
        .unwrap()
        .trim_end_matches('"')
        .to_string();
    fs::write(
        &shim,
        format!(
            "[package]\nname = \"extra\"\nversion = \"0.1.0\"\n\n\
             [source.git]\nurl = \"https://harbour.invalid/extra.git\"\nrev = \"{rev}\"\n"
        ),
    )
    .unwrap();
    local_registry::commit_all(&registry, "extra's source has moved").unwrap();
    let url = local_registry::file_url(&registry);

    let off = tmp.path().join("off");
    write_registry_app(&off, "opttop = \"^0.1\"", &["opttop.h"], "opttop_value()");
    build_in_registry(&home, &off, &url).success();
    assert_eq!(run_app(&off), "7");
    assert_eq!(
        lock_entries(&off, "extra"),
        0,
        "an inactive optional dependency is not locked"
    );

    let on = tmp.path().join("on");
    write_registry_app(
        &on,
        "opttop = { version = \"^0.1\", features = [\"extra\"] }",
        &["opttop.h"],
        "opttop_value()",
    );
    let run = build_in_registry(&home, &on, &url);
    assert!(
        !run.status.success(),
        "activating it must fetch it, and it cannot be fetched\n{run}"
    );
    assert!(
        run.to_string().contains("extra"),
        "the failure should name the dependency\n{run}"
    );
}

/// A feature a registry package asks of its dependency reaches the build.
///
/// `mid` depends on `base` with `features = ["extra"]`, and `base` prints 1 only
/// if it was compiled with `extra`. A regression pin, not a bug report: the
/// tier-1 index records a dependency's name, requirement and optionality but
/// *not* its features, and this passes anyway because the build re-reads each
/// fetched package's own manifest to unify features. (Recording them in the index
/// was tried and mutation-checked: removing it changed nothing, so it was not
/// kept. See the design note for what that means for "resolution is
/// metadata-only".)
#[test]
fn test_a_feature_a_registry_package_asks_of_its_dependency_survives_the_index() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let registry = tmp.path().join("registry");
    local_registry::init(&registry).unwrap();

    let base = write_fixture_lib(
        tmp.path(),
        "base",
        "",
        "[features]\nextra = []\n\n[[targets.base.when]]\nfeature = \"extra\"\ndefines = [\"HAVE_EXTRA\"]\n",
        &[
            ("base.h", "int base_value(void);\n"),
            (
                "base.c",
                "int base_value(void) {\n#ifdef HAVE_EXTRA\n    return 1;\n#else\n    return 0;\n#endif\n}\n",
            ),
        ],
    );
    publish_package(&registry, "base", &base);
    let mid = write_fixture_lib(
        tmp.path(),
        "mid",
        "base = { version = \"^0.1\", features = [\"extra\"] }",
        "",
        &[
            ("mid.h", "#include \"base.h\"\nint mid_value(void);\n"),
            (
                "mid.c",
                "#include \"mid.h\"\nint mid_value(void) { return base_value(); }\n",
            ),
        ],
    );
    publish_package(&registry, "mid", &mid);
    local_registry::publish(&registry, &tmp.path().join("scratch")).unwrap();

    let app = tmp.path().join("app");
    write_registry_app(&app, "mid = \"^0.1\"", &["mid.h"], "mid_value()");
    build_in_registry(&home, &app, &local_registry::file_url(&registry)).success();
    assert_eq!(
        run_app(&app),
        "1",
        "`base` must have been built with its `extra` feature"
    );
}

/// A shim can carry the package's `Harbour.toml` as a patch, which is how a
/// registry entry is made for an upstream that has no manifest of its own.
///
/// Every real C library is in that position. The source repository here has
/// only `.c` and `.h` files; the shim's patch adds the manifest, and the
/// package builds and runs from it.
#[test]
fn test_a_shim_patch_can_add_the_manifest_an_upstream_lacks() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let registry = tmp.path().join("registry");
    local_registry::init(&registry).unwrap();

    let upstream = tmp.path().join("upstream-lib");
    fs::create_dir_all(&upstream).unwrap();
    fs::write(upstream.join("lib.h"), "int lib_value(void);\n").unwrap();
    fs::write(
        upstream.join("lib.c"),
        "int lib_value(void) { return 42; }\n",
    )
    .unwrap();
    let rev = local_registry::commit_all(&upstream, "upstream").unwrap();

    let (patch_name, patch_sha) = write_manifest_patch(&registry, "lib", "0.1.0");
    local_registry::add_shim(
        &registry,
        "lib",
        "0.1.0",
        &local_registry::file_url(&upstream),
        &rev,
        &[(patch_name.as_str(), patch_sha.as_str())],
    )
    .unwrap();
    local_registry::publish(&registry, &tmp.path().join("scratch")).unwrap();

    let app = tmp.path().join("app");
    write_registry_app(&app, "lib = \"^0.1\"", &["lib.h"], "lib_value()");
    build_in_registry(&home, &app, &local_registry::file_url(&registry)).success();
    assert_eq!(run_app(&app), "42");
}

/// A patch that does not match its recorded hash is reported as that.
///
/// It used to be swallowed by the C++-constraint pass, after the source had
/// already been cloned into the cache. The next load found the clone, served it
/// unpatched, and failed with "has no Harbour.toml and no shim surface
/// override": a true statement about the wrong thing, pointing away from the
/// patch.
#[test]
fn test_a_patch_that_fails_verification_says_so_instead_of_poisoning_the_cache() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let registry = tmp.path().join("registry");
    local_registry::init(&registry).unwrap();

    let upstream = tmp.path().join("upstream-lib");
    fs::create_dir_all(&upstream).unwrap();
    fs::write(upstream.join("lib.h"), "int lib_value(void);\n").unwrap();
    fs::write(
        upstream.join("lib.c"),
        "int lib_value(void) { return 42; }\n",
    )
    .unwrap();
    let rev = local_registry::commit_all(&upstream, "upstream").unwrap();

    let (patch_name, _real_sha) = write_manifest_patch(&registry, "lib", "0.1.0");
    // A well-formed hash that is not this patch's.
    let wrong_sha = "0".repeat(64);
    local_registry::add_shim(
        &registry,
        "lib",
        "0.1.0",
        &local_registry::file_url(&upstream),
        &rev,
        &[(patch_name.as_str(), wrong_sha.as_str())],
    )
    .unwrap();
    // Not `publish`: indexing would fetch the source and hit the same error,
    // which is correct but not what is under test. Write the index by hand.
    let idx = registry.join("index/l/lib.idx");
    fs::create_dir_all(idx.parent().unwrap()).unwrap();
    fs::write(
        &idx,
        "{\"format_version\":1,\"name\":\"lib\",\"version\":\"0.1.0\",\"yanked\":false,\
         \"deps\":[],\"checksum\":null,\"shim\":\"l/lib/0.1.0.toml\"}\n",
    )
    .unwrap();
    local_registry::commit_all(&registry, "publish").unwrap();

    let app = tmp.path().join("app");
    write_registry_app(&app, "lib = \"^0.1\"", &["lib.h"], "lib_value()");
    let url = local_registry::file_url(&registry);
    for attempt in 1..=2 {
        let run = build_in_registry(&home, &app, &url);
        assert!(!run.status.success(), "attempt {attempt} must fail\n{run}");
        let text = run.to_string();
        assert!(
            text.contains("hash mismatch"),
            "attempt {attempt} must report the patch, not a missing manifest\n{text}"
        );
    }
}

/// Write `index/<l>/<name>/<version>.patch`, a patch that adds a one-library
/// `Harbour.toml`, and return its file name and sha256.
fn write_manifest_patch(registry: &Path, name: &str, version: &str) -> (String, String) {
    let manifest = format!(
        "[package]\nname = \"{name}\"\nversion = \"{version}\"\n\n\
         [targets.{name}]\nkind = \"staticlib\"\nsources = [\"{name}.c\"]\n\n\
         [targets.{name}.surface.compile.public]\ninclude_dirs = [\".\"]\n"
    );
    let added: String = manifest.lines().map(|l| format!("+{l}\n")).collect();
    let patch = format!(
        "diff --git a/Harbour.toml b/Harbour.toml\nnew file mode 100644\n--- /dev/null\n\
         +++ b/Harbour.toml\n@@ -0,0 +1,{} @@\n{added}",
        manifest.lines().count()
    );
    let dir = registry
        .join("index")
        .join(name.chars().next().unwrap().to_string())
        .join(name);
    fs::create_dir_all(&dir).unwrap();
    let file = format!("{version}.patch");
    fs::write(dir.join(&file), &patch).unwrap();
    (file, harbour::util::hash::sha256_str(&patch))
}
