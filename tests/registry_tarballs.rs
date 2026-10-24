//! Registry packages whose source is a **tarball**, with patches.
//!
//! Every real C library publishes release tarballs, and none of them has a
//! `Harbour.toml`; a registry entry for one carries its manifest as a patch.
//! Patches used to be refused on tarball sources ("patches can only be applied
//! to git sources"), which left most of the real packages unable to become
//! registry entries at all. Tarballs here are built in the test and served
//! through `file://` URLs, so nothing touches the network.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use harbour::test_support::fixtures::local_registry;
use tempfile::TempDir;

const LIB_H: &str = "int lib_value(void);\n";

fn lib_c(answer: u32) -> String {
    format!("int lib_value(void) {{ return {answer}; }}\n")
}

/// A tarball `lib-1.0.tar.gz` of an upstream with no manifest (upstream returns
/// `answer`), and its sha256.
fn tarball(tmp: &TempDir, answer: u32) -> (PathBuf, String) {
    let path = tmp.path().join("lib-1.0.tar.gz");
    let sha = local_registry::write_tarball(
        &path,
        "lib-1.0",
        &[("lib.h", LIB_H), ("lib.c", &lib_c(answer))],
    )
    .unwrap();
    (path, sha)
}

fn file_url(path: &Path) -> String {
    url::Url::from_file_path(path).unwrap().to_string()
}

/// A registry holding `lib` 0.1.0, whose source is `tarball_path` and whose
/// patch is `patch_text`, with the patch's recorded hash replaced by
/// `claimed_patch_sha` when given (to provoke a mismatch).
fn registry_with_tarball_lib(
    tmp: &TempDir,
    tarball_path: &Path,
    tarball_sha: &str,
    patch_text: &str,
    claimed_patch_sha: Option<&str>,
) -> PathBuf {
    let registry = tmp.path().join("registry");
    local_registry::init(&registry).unwrap();
    let (patch, sha) = common::write_patch(&registry, "lib", "0.1.0", patch_text);
    let sha = claimed_patch_sha.map(str::to_string).unwrap_or(sha);
    local_registry::add_tarball_shim(
        &registry,
        "lib",
        "0.1.0",
        &file_url(tarball_path),
        tarball_sha,
        Some("lib-1.0"),
        &[(patch.as_str(), sha.as_str())],
    )
    .unwrap();
    registry
}

/// Assert that the harbour under test left no extracted tree for `lib` in this
/// registry's source cache.
///
/// The directory for the registry must *exist* -- a failed fetch creates it and
/// takes only the package's own directory away again -- which is what keeps this
/// from passing vacuously: a search of the wrong place (the cache is not under
/// the isolated home on Windows) finds no files either. Only then is "no
/// `lib.c`" a statement about the cache rather than about where we looked.
fn assert_no_tree_left_for_lib(home: &Path, registry_url: &str) {
    let cache = common::registry_source_cache(home, registry_url)
        .expect("the failed build must have created this registry's source cache directory");
    let leftovers: Vec<_> = common::walk_files(&cache)
        .into_iter()
        .filter(|p| p.file_name().is_some_and(|n| n == "lib.c"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "a failed patch left a tree behind in {}: {leftovers:?}",
        cache.display()
    );
}

/// The patch an entry for a manifest-less upstream carries: it adds the manifest.
fn manifest_patch() -> String {
    common::manifest_patch_text("lib", "0.1.0")
}

/// A tarball source with a patch that adds `Harbour.toml` builds and runs.
#[test]
fn a_patch_can_add_the_manifest_a_tarball_lacks() {
    let tmp = TempDir::new().unwrap();
    let home = common::home(&tmp);
    let (tar_path, tar_sha) = tarball(&tmp, 42);
    let registry = registry_with_tarball_lib(&tmp, &tar_path, &tar_sha, &manifest_patch(), None);
    local_registry::publish(&registry, &tmp.path().join("scratch")).unwrap();

    let app = tmp.path().join("app");
    common::write_app(&app, "lib = \"^0.1\"", &["lib.h"], "lib_value()");
    let run = common::run_in_registry(
        &home,
        &app,
        &["build"],
        &local_registry::file_url(&registry),
    );
    assert!(
        run.success,
        "a tarball package with a manifest patch must build\n{}",
        run.text
    );
    assert_eq!(common::run_exe(&app, "app"), "42");
}

/// A patch may also *modify* a file the tarball contains, not only add one.
#[test]
fn a_patch_can_modify_a_file_in_the_tarball() {
    let tmp = TempDir::new().unwrap();
    let home = common::home(&tmp);
    let (tar_path, tar_sha) = tarball(&tmp, 41);
    let patch = format!(
        "{}diff --git a/lib.c b/lib.c\n--- a/lib.c\n+++ b/lib.c\n@@ -1 +1 @@\n\
         -int lib_value(void) {{ return 41; }}\n+int lib_value(void) {{ return 42; }}\n",
        manifest_patch()
    );
    let registry = registry_with_tarball_lib(&tmp, &tar_path, &tar_sha, &patch, None);
    local_registry::publish(&registry, &tmp.path().join("scratch")).unwrap();

    let app = tmp.path().join("app");
    common::write_app(&app, "lib = \"^0.1\"", &["lib.h"], "lib_value()");
    let run = common::run_in_registry(
        &home,
        &app,
        &["build"],
        &local_registry::file_url(&registry),
    );
    assert!(run.success, "{}", run.text);
    assert_eq!(
        common::run_exe(&app, "app"),
        "42",
        "the modified file must be what is built"
    );
}

/// A bad patch on a tarball source is reported *as the patch*, twice running,
/// and leaves no half-prepared tree in the cache -- the same contract as for
/// git sources (#188): the cache used to serve the unpatched tree on the next
/// load and fail with a misleading "no Harbour.toml".
#[test]
fn a_failed_patch_on_a_tarball_is_reported_twice_and_leaves_nothing_cached() {
    let tmp = TempDir::new().unwrap();
    let home = common::home(&tmp);
    let (tar_path, tar_sha) = tarball(&tmp, 42);
    // A well-formed hash that is not this patch's.
    let wrong = "0".repeat(64);
    let registry =
        registry_with_tarball_lib(&tmp, &tar_path, &tar_sha, &manifest_patch(), Some(&wrong));
    // Not `publish`: indexing would fetch and patch and hit the same error,
    // which is correct but not what is under test.
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
    common::write_app(&app, "lib = \"^0.1\"", &["lib.h"], "lib_value()");
    let url = local_registry::file_url(&registry);
    for attempt in 1..=2 {
        let run = common::run_in_registry(&home, &app, &["build"], &url);
        assert!(!run.success, "attempt {attempt} must fail\n{}", run.text);
        assert!(
            run.text.contains("hash mismatch"),
            "attempt {attempt} must report the patch, not a missing manifest\n{}",
            run.text
        );
    }
    assert_no_tree_left_for_lib(&home, &url);
}

/// A patch that does not *apply* (as opposed to one whose hash is wrong) is
/// reported as that, with git's explanation, and also leaves nothing behind.
#[test]
fn a_patch_that_does_not_apply_to_the_tarball_is_reported_as_such() {
    let tmp = TempDir::new().unwrap();
    let home = common::home(&tmp);
    let (tar_path, tar_sha) = tarball(&tmp, 41);
    // Context that is not in the file.
    let patch = format!(
        "{}diff --git a/lib.c b/lib.c\n--- a/lib.c\n+++ b/lib.c\n@@ -1 +1 @@\n\
         -int lib_value(void) {{ return 7; }}\n+int lib_value(void) {{ return 42; }}\n",
        manifest_patch()
    );
    let registry = registry_with_tarball_lib(&tmp, &tar_path, &tar_sha, &patch, None);
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
    common::write_app(&app, "lib = \"^0.1\"", &["lib.h"], "lib_value()");
    let run = common::run_in_registry(
        &home,
        &app,
        &["build"],
        &local_registry::file_url(&registry),
    );
    assert!(!run.success);
    assert!(run.text.contains("will not apply cleanly"), "{}", run.text);
    assert_no_tree_left_for_lib(&home, &local_registry::file_url(&registry));
}

/// The tarball's own sha256 is still checked, before any patch is applied.
#[test]
fn a_tarball_that_does_not_match_its_pin_is_refused() {
    let tmp = TempDir::new().unwrap();
    let home = common::home(&tmp);
    let (tar_path, _real) = tarball(&tmp, 42);
    let wrong = "1".repeat(64);
    let registry = registry_with_tarball_lib(&tmp, &tar_path, &wrong, &manifest_patch(), None);
    let idx = registry.join("index/l/lib.idx");
    fs::create_dir_all(idx.parent().unwrap()).unwrap();
    fs::write(
        &idx,
        format!(
            "{{\"format_version\":1,\"name\":\"lib\",\"version\":\"0.1.0\",\"yanked\":false,\
             \"deps\":[],\"checksum\":\"{wrong}\",\"shim\":\"l/lib/0.1.0.toml\"}}\n"
        ),
    )
    .unwrap();
    local_registry::commit_all(&registry, "publish").unwrap();

    let app = tmp.path().join("app");
    common::write_app(&app, "lib = \"^0.1\"", &["lib.h"], "lib_value()");
    let run = common::run_in_registry(
        &home,
        &app,
        &["build"],
        &local_registry::file_url(&registry),
    );
    assert!(!run.success);
    assert!(run.text.contains("tarball hash mismatch"), "{}", run.text);
}

/// Editing a patch (and its recorded hash) without bumping the version must not
/// be served from the cache: a tarball package's patches are part of its
/// identity, exactly as a git package's are (#188, G3). The second build uses a
/// fresh app so no lockfile pins the old source; only the source cache can make
/// it stale.
#[test]
fn editing_a_tarball_packages_patch_is_not_served_from_the_cache() {
    let tmp = TempDir::new().unwrap();
    let home = common::home(&tmp);
    let (tar_path, tar_sha) = tarball(&tmp, 41);
    let patch_to = |answer: u32| {
        format!(
            "{}diff --git a/lib.c b/lib.c\n--- a/lib.c\n+++ b/lib.c\n@@ -1 +1 @@\n\
             -int lib_value(void) {{ return 41; }}\n+int lib_value(void) {{ return {answer}; }}\n",
            manifest_patch()
        )
    };

    let registry = registry_with_tarball_lib(&tmp, &tar_path, &tar_sha, &patch_to(42), None);
    local_registry::publish(&registry, &tmp.path().join("scratch")).unwrap();
    let url = local_registry::file_url(&registry);

    let first = tmp.path().join("app-first");
    common::write_app(&first, "lib = \"^0.1\"", &["lib.h"], "lib_value()");
    assert!(common::run_in_registry(&home, &first, &["build"], &url).success);
    assert_eq!(common::run_exe(&first, "app"), "42");

    // Same version, new patch.
    let (patch, sha) = common::write_patch(&registry, "lib", "0.1.0", &patch_to(43));
    local_registry::add_tarball_shim(
        &registry,
        "lib",
        "0.1.0",
        &file_url(&tar_path),
        &tar_sha,
        Some("lib-1.0"),
        &[(patch.as_str(), sha.as_str())],
    )
    .unwrap();
    local_registry::publish(&registry, &tmp.path().join("scratch2")).unwrap();

    let second = tmp.path().join("app-second");
    common::write_app(&second, "lib = \"^0.1\"", &["lib.h"], "lib_value()");
    let run = common::run_in_registry(&home, &second, &["build"], &url);
    assert!(run.success, "{}", run.text);
    assert_eq!(
        common::run_exe(&second, "app"),
        "43",
        "the edited patch must be what is built, not the cached tree from the old one"
    );
}

/// A tarball's extracted tree is patched in place even when the cache happens to
/// live inside some other git repository (a project's own `.harbour`, a home
/// directory under version control). `git apply` run inside a repository takes a
/// patch's paths relative to that repository's *root*; without a ceiling on
/// repository discovery it would look for `Harbour.toml` next to the enclosing
/// repository's top level and find nothing to patch.
#[test]
fn a_tarball_patch_applies_when_the_cache_is_inside_another_git_repository() {
    let tmp = TempDir::new().unwrap();
    // Everything below -- the harbour home and so its source cache -- is now
    // inside a repository that has nothing to do with the package.
    git2::Repository::init(tmp.path()).unwrap();
    let home = common::home(&tmp);
    let (tar_path, tar_sha) = tarball(&tmp, 42);
    let registry = registry_with_tarball_lib(&tmp, &tar_path, &tar_sha, &manifest_patch(), None);
    local_registry::publish(&registry, &tmp.path().join("scratch")).unwrap();

    let app = tmp.path().join("app");
    common::write_app(&app, "lib = \"^0.1\"", &["lib.h"], "lib_value()");
    let run = common::run_in_registry(
        &home,
        &app,
        &["build"],
        &local_registry::file_url(&registry),
    );
    assert!(run.success, "{}", run.text);
    assert_eq!(common::run_exe(&app, "app"), "42");
}
