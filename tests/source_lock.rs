//! Harbour.lock records which source files each fetched package compiles, and a
//! change to that set is a warning (or, under `--locked`, an error) rather than
//! a different library.
//!
//! The registry here is built in the test and served over `file://`, so nothing
//! touches the network. Its package names its sources with a glob and an
//! `exclude`, which is what makes the file set something a tarball can change.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use harbour::test_support::fixtures::local_registry;
use tempfile::TempDir;

fn file_url(path: &Path) -> String {
    url::Url::from_file_path(path).unwrap().to_string()
}

/// A patch adding a manifest whose sources are `*.c` minus `skip_*.c`.
fn glob_manifest_patch(version: &str) -> String {
    let manifest = format!(
        "[package]\nname = \"lib\"\nversion = \"{version}\"\n\n\
         [targets.lib]\nkind = \"staticlib\"\nsources = [\"*.c\"]\nexclude = [\"skip_*.c\"]\n\n\
         [targets.lib.surface.compile.public]\ninclude_dirs = [\".\"]\n"
    );
    let added: String = manifest.lines().map(|l| format!("+{l}\n")).collect();
    format!(
        "diff --git a/Harbour.toml b/Harbour.toml\nnew file mode 100644\n--- /dev/null\n\
         +++ b/Harbour.toml\n@@ -0,0 +1,{} @@\n{added}",
        manifest.lines().count()
    )
}

/// Publish `lib` `version` from a tarball holding `lib.c`, an excluded
/// `skip_me.c` and whatever `extra` adds.
fn publish_lib(tmp: &TempDir, registry: &Path, version: &str, extra: &[(&str, &str)]) {
    let tarball = tmp.path().join(format!("lib-{version}.tar.gz"));
    let mut files = vec![
        ("lib.h", "int lib_value(void);\n"),
        ("lib.c", "int lib_value(void) { return 42; }\n"),
        ("skip_me.c", "int skipped(void) { return 0; }\n"),
    ];
    files.extend_from_slice(extra);
    let top = format!("lib-{version}");
    let sha = local_registry::write_tarball(&tarball, &top, &files).unwrap();
    let (patch, patch_sha) =
        common::write_patch(registry, "lib", version, &glob_manifest_patch(version));
    local_registry::add_tarball_shim(
        registry,
        "lib",
        version,
        &file_url(&tarball),
        &sha,
        Some(&top),
        &[(patch.as_str(), patch_sha.as_str())],
    )
    .unwrap();
    local_registry::publish(registry, &tmp.path().join("scratch")).unwrap();
}

struct World {
    tmp: TempDir,
    home: PathBuf,
    registry: PathBuf,
    app: PathBuf,
}

impl World {
    /// A registry with `lib` 0.1.0 and an app that depends on it.
    fn new() -> World {
        let tmp = TempDir::new().unwrap();
        let home = common::home(&tmp);
        let registry = tmp.path().join("registry");
        local_registry::init(&registry).unwrap();
        publish_lib(&tmp, &registry, "0.1.0", &[]);
        let app = tmp.path().join("app");
        common::write_app(&app, "lib = \"^0.1\"", &["lib.h"], "lib_value()");
        World {
            tmp,
            home,
            registry,
            app,
        }
    }

    fn harbour(&self, args: &[&str]) -> common::Run {
        common::run_in_registry(&self.home, &self.app, args, &file_url(&self.registry))
    }

    fn lock_path(&self) -> PathBuf {
        self.app.join("Harbour.lock")
    }

    fn lock(&self) -> String {
        fs::read_to_string(self.lock_path()).unwrap()
    }
}

#[test]
fn a_fetched_package_is_recorded_and_the_users_own_package_is_not() {
    let w = World::new();
    let run = w.harbour(&["build"]);
    assert!(run.success, "{}", run.text);
    let lock = w.lock();
    assert!(lock.contains("[package.sources]"), "{lock}");
    assert!(lock.contains("\"lib.c\""), "{lock}");
    assert!(
        !lock.contains("skip_me.c"),
        "an excluded file is not recorded:\n{lock}"
    );
    assert_eq!(
        lock.matches("[package.sources]").count(),
        1,
        "only the fetched package is recorded, not the app:\n{lock}"
    );
    assert!(
        !run.text.contains("sources changed"),
        "recording is not drift\n{}",
        run.text
    );
}

#[test]
fn an_unchanged_rebuild_leaves_the_lockfile_untouched() {
    let w = World::new();
    assert!(w.harbour(&["build"]).success);
    let before = fs::read(w.lock_path()).unwrap();
    let run = w.harbour(&["build"]);
    assert!(run.success, "{}", run.text);
    assert!(!run.text.contains("sources changed"), "{}", run.text);
    assert_eq!(fs::read(w.lock_path()).unwrap(), before);
}

#[test]
fn a_changed_file_set_warns_and_updates_the_lock() {
    let w = World::new();
    assert!(w.harbour(&["build"]).success);
    // The tree now holds a file the lock does not know and lacks one it does.
    let tampered = w.lock().replace("\"lib.c\"", "\"old.c\"");
    fs::write(w.lock_path(), tampered).unwrap();

    let run = w.harbour(&["build"]);
    assert!(run.success, "drift warns, it does not fail\n{}", run.text);
    assert!(
        run.text
            .contains("lib 0.1.0 target lib: sources changed: +1 -1 (+lib.c, -old.c)"),
        "{}",
        run.text
    );
    assert!(run.text.contains("Harbour.lock updated"), "{}", run.text);
    let lock = w.lock();
    assert!(
        lock.contains("\"lib.c\"") && !lock.contains("old.c"),
        "{lock}"
    );
}

#[test]
fn locked_refuses_a_changed_file_set_and_leaves_the_lock_alone() {
    let w = World::new();
    assert!(w.harbour(&["build"]).success);
    let tampered = w.lock().replace("\"lib.c\"", "\"old.c\"");
    fs::write(w.lock_path(), &tampered).unwrap();

    let run = w.harbour(&["build", "--locked"]);
    assert!(!run.success, "{}", run.text);
    assert!(
        run.text.contains("lib 0.1.0 target lib") && run.text.contains("+lib.c"),
        "{}",
        run.text
    );
    assert!(run.text.contains("without `--locked`"), "{}", run.text);
    assert_eq!(w.lock(), tampered, "--locked must not rewrite the lock");
}

#[test]
fn an_old_lock_without_the_section_still_builds_locked_and_gains_it_otherwise() {
    let w = World::new();
    assert!(w.harbour(&["build"]).success);
    // Strip the section, as a lock written before it existed would lack it.
    let lock = w.lock();
    let (before_section, after_section) = lock.split_once("[package.sources]").unwrap();
    let after_array = &after_section[after_section.find("]\n").unwrap() + 2..];
    let old_style = format!("{before_section}{after_array}");
    fs::write(w.lock_path(), &old_style).unwrap();

    let locked = w.harbour(&["build", "--locked"]);
    assert!(locked.success, "{}", locked.text);
    assert_eq!(w.lock(), old_style, "--locked records nothing");

    let run = w.harbour(&["build"]);
    assert!(run.success, "{}", run.text);
    assert!(w.lock().contains("[package.sources]"));
    assert!(!run.text.contains("sources changed"), "{}", run.text);
}

#[test]
fn moving_to_a_new_version_reports_what_it_did_to_the_file_set() {
    let w = World::new();
    assert!(w.harbour(&["build"]).success);
    publish_lib(
        &w.tmp,
        &w.registry,
        "0.2.0",
        &[("extra.c", "int extra(void) { return 1; }\n")],
    );
    fs::write(
        w.app.join("Harbour.toml"),
        fs::read_to_string(w.app.join("Harbour.toml"))
            .unwrap()
            .replace("^0.1", "^0.2"),
    )
    .unwrap();

    let run = w.harbour(&["build"]);
    assert!(run.success, "{}", run.text);
    assert!(
        run.text
            .contains("lib 0.1.0 -> 0.2.0, target lib: source list changed (+1 -0 (+extra.c))"),
        "{}",
        run.text
    );
    let lock = w.lock();
    assert!(lock.contains("\"extra.c\""), "{lock}");
    assert!(
        !lock.contains("sources_from"),
        "the marker is gone once compared:\n{lock}"
    );
}
