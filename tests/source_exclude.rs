//! A source a target excludes is not required to exist.
//!
//! Harbour refuses a target that names a source file which is not on disk: a
//! vendored or generated file that failed to ship would otherwise vanish from
//! the build silently. That check ran over the raw `sources` list and ignored
//! `exclude`, so a target could not name a file for one platform and drop it for
//! another: LuaJIT lists `src/lj_vm.S` and, under MSVC, excludes it because
//! `buildvm` writes `lj_vm.obj` instead -- and was refused for a file it had
//! explicitly dropped. Found building LuaJIT on a Windows runner.

mod common;

use std::fs;

use tempfile::TempDir;

fn build(manifest_extra: &str) -> (bool, String) {
    let tmp = TempDir::new().unwrap();
    let home = common::home(&tmp);
    let app = tmp.path().join("app");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(app.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();
    fs::write(
        app.join("Harbour.toml"),
        format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
             [targets.app]\nkind = \"exe\"\n\
             sources = [\"src/main.c\", \"src/absent.c\"]\n{manifest_extra}"
        ),
    )
    .unwrap();
    let out = common::harbour(&home)
        .current_dir(&app)
        .arg("build")
        .output()
        .unwrap();
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[test]
fn a_named_source_that_is_excluded_is_not_required_to_exist() {
    let (ok, text) = build("exclude = [\"src/absent.c\"]\n");
    assert!(ok, "an excluded entry must not be checked\n{text}");
}

#[test]
fn an_exclude_glob_covers_a_named_source_too() {
    let (ok, text) = build("exclude = [\"src/abs*.c\"]\n");
    assert!(ok, "the glob matches the entry as written\n{text}");
}

#[test]
fn a_named_source_that_is_not_excluded_is_still_required() {
    let (ok, text) = build("");
    assert!(!ok, "the missing-file check must still fire");
    assert!(
        text.contains("src/absent.c") && text.contains("do not exist"),
        "{text}"
    );
}
