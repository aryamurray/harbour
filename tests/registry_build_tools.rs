//! `build_tools` carried by a **registry entry**: the situation OpenSSL is in.
//!
//! The package is a tarball with no manifest, its entry's patch adds one that
//! declares the tools its generators need, and the consumer is an app that never
//! mentions them. A machine without the tool must be told so before the
//! generator runs, naming the dependency.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use harbour::test_support::fixtures::local_registry;
use tempfile::TempDir;

/// A tool no machine has.
const MISSING: &str = "harbour-no-such-tool-7c1e";

/// The patch for an upstream with no manifest: adds one that declares
/// `build_tools` and runs a `sh` generator leaving `generated/ran.txt`.
fn manifest_patch(build_tools: &str) -> String {
    let manifest = format!(
        "[package]\nname = \"lib\"\nversion = \"0.1.0\"\nbuild_tools = [{build_tools}]\n\n\
         [targets.lib]\nkind = \"staticlib\"\nsources = [\"lib.c\"]\n\n\
         [targets.lib.surface.compile.public]\ninclude_dirs = [\".\"]\n\n\
         [[targets.lib.prebuild]]\nprogram = \"sh\"\n\
         args = [\"-c\", \"mkdir -p generated && echo ran > generated/ran.txt\"]\n\
         outputs = [\"generated/ran.txt\"]\n"
    );
    let added: String = manifest.lines().map(|l| format!("+{l}\n")).collect();
    format!(
        "diff --git a/Harbour.toml b/Harbour.toml\nnew file mode 100644\n--- /dev/null\n\
         +++ b/Harbour.toml\n@@ -0,0 +1,{} @@\n{added}",
        manifest.lines().count()
    )
}

/// A registry with `lib` 0.1.0 (tarball source, patch = `manifest_patch`),
/// returning its path.
fn registry(tmp: &TempDir, build_tools: &str) -> PathBuf {
    let tarball = tmp.path().join("lib-1.0.tar.gz");
    let sha = local_registry::write_tarball(
        &tarball,
        "lib-1.0",
        &[
            ("lib.h", "int lib_value(void);\n"),
            ("lib.c", "int lib_value(void) { return 7; }\n"),
        ],
    )
    .unwrap();

    let registry = tmp.path().join("registry");
    local_registry::init(&registry).unwrap();
    let (patch, patch_sha) =
        common::write_patch(&registry, "lib", "0.1.0", &manifest_patch(build_tools));
    local_registry::add_tarball_shim(
        &registry,
        "lib",
        "0.1.0",
        url::Url::from_file_path(&tarball).unwrap().as_str(),
        &sha,
        Some("lib-1.0"),
        &[(patch.as_str(), patch_sha.as_str())],
    )
    .unwrap();
    local_registry::publish(&registry, &tmp.path().join("scratch")).unwrap();
    registry
}

fn generated_files(home: &Path, registry: &Path) -> Vec<PathBuf> {
    // The cache must exist or "no file" says nothing about the cache, only about
    // where we looked (on Windows it is not under the isolated home).
    let cache = common::registry_source_cache(home, &local_registry::file_url(registry))
        .expect("the build must have created this registry's source cache");
    common::walk_files(&cache)
        .into_iter()
        .filter(|p| p.file_name().is_some_and(|n| n == "ran.txt"))
        .collect()
}

fn build_app(home: &Path, tmp: &TempDir, registry: &Path) -> common::Run {
    let app = tmp.path().join("app");
    common::write_app(&app, "lib = \"^0.1\"", &["lib.h"], "lib_value()");
    common::run_in_registry(home, &app, &["build"], &local_registry::file_url(registry))
}

#[test]
#[cfg(unix)]
fn a_registry_dependencys_missing_tool_is_reported_before_its_generator_runs() {
    let tmp = TempDir::new().unwrap();
    let home = common::home(&tmp);
    let registry = registry(&tmp, &format!("\"{MISSING}\""));

    let run = build_app(&home, &tmp, &registry);
    assert!(!run.success, "{}", run.text);
    assert!(
        run.text.contains("package `lib`") && run.text.contains(MISSING),
        "the registry dependency, not the app, must be named\n{}",
        run.text
    );
    assert!(
        generated_files(&home, &registry).is_empty(),
        "the generator must not have run in the cached tree"
    );
}

/// The control that makes the test above mean something: the same entry with a
/// tool that is present runs the generator, and the search finds its output.
#[test]
#[cfg(unix)]
fn the_same_entry_with_a_present_tool_builds_and_its_generator_ran() {
    let tmp = TempDir::new().unwrap();
    let home = common::home(&tmp);
    let registry = registry(&tmp, "\"sh\"");

    let run = build_app(&home, &tmp, &registry);
    assert!(run.success, "{}", run.text);
    assert_eq!(
        generated_files(&home, &registry).len(),
        1,
        "the generator's output must be in the cached tree (and the search must find it)"
    );
    assert_eq!(common::run_exe(&tmp.path().join("app"), "app"), "7");
}
