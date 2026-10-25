//! A generator that cannot write into its package directory says why.
//!
//! Generators write their outputs into the package root, which for a package
//! from a registry is the shared source cache. Found by making that cache tree
//! read-only: the build died with a bare `generated/config.c: Permission
//! denied` from the shell and no hint that a build writes into a source cache.

#![cfg(unix)]

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;

/// Running as root ignores directory permissions, which would make this test
/// pass for the wrong reason (nothing is unwritable) or fail for it.
fn running_as_root(dir: &std::path::Path) -> bool {
    let probe = dir.join(".root-probe");
    let writable = fs::write(&probe, b"").is_ok();
    let _ = fs::remove_file(&probe);
    writable
}

#[test]
fn a_generator_in_a_read_only_package_directory_names_the_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let home = common::home(&tmp);
    let pkg = tmp.path().join("pkg");
    fs::create_dir_all(pkg.join("src")).unwrap();
    fs::write(pkg.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();
    let manifest = |generator: &str| {
        format!(
            "[package]\nname = \"pkg\"\nversion = \"0.1.0\"\n\n\
             [targets.pkg]\nkind = \"exe\"\nsources = [\"src/main.c\"]\n{generator}"
        )
    };

    // Build once with no generator, so the lockfile and the build directory exist:
    // a read-only root otherwise fails at the lockfile, before any generator runs.
    fs::write(pkg.join("Harbour.toml"), manifest("")).unwrap();
    let first = common::harbour(&home)
        .arg("build")
        .current_dir(&pkg)
        .output()
        .unwrap();
    assert!(first.status.success(), "setup build failed");

    // Now add the generator by rewriting the existing manifest in place (allowed in
    // a read-only directory), and make the directory read-only.
    fs::write(
        pkg.join("Harbour.toml"),
        manifest(
            "\n[[targets.pkg.prebuild]]\nprogram = \"sh\"\n\
             args = [\"-c\", \"mkdir -p generated && echo x > generated/out.txt\"]\n\
             outputs = [\"generated/out.txt\"]\n",
        ),
    )
    .unwrap();
    fs::set_permissions(&pkg, fs::Permissions::from_mode(0o555)).unwrap();
    let skip = running_as_root(&pkg);
    let out = common::harbour(&home)
        .arg("build")
        .current_dir(&pkg)
        .output()
        .unwrap();
    // Let the temp directory be cleaned up.
    fs::set_permissions(&pkg, fs::Permissions::from_mode(0o755)).unwrap();

    if skip {
        eprintln!("skipping: directory permissions do not bind this user (running as root)");
        return;
    }
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success(),
        "the generator cannot write; the build must fail\n{text}"
    );
    assert!(
        text.contains("is not writable") && text.contains("shared source cache"),
        "the error must say the package directory is not writable and why that matters\n{text}"
    );
    assert!(
        text.contains(&pkg.display().to_string()),
        "the error must name the directory\n{text}"
    );
}

/// A generator that fails for any other reason gets no such note: the note is a
/// diagnosis, and a wrong diagnosis is worse than none.
#[test]
fn a_generator_that_fails_in_a_writable_directory_gets_no_note() {
    let tmp = tempfile::tempdir().unwrap();
    let home = common::home(&tmp);
    let pkg = tmp.path().join("pkg");
    fs::create_dir_all(pkg.join("src")).unwrap();
    fs::write(pkg.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();
    fs::write(
        pkg.join("Harbour.toml"),
        "[package]\nname = \"pkg\"\nversion = \"0.1.0\"\n\n\
         [targets.pkg]\nkind = \"exe\"\nsources = [\"src/main.c\"]\n\n\
         [[targets.pkg.prebuild]]\nprogram = \"sh\"\nargs = [\"-c\", \"echo broken >&2; exit 3\"]\n\
         outputs = [\"generated/out.txt\"]\n",
    )
    .unwrap();

    let out = common::harbour(&home)
        .arg("build")
        .current_dir(&pkg)
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success());
    assert!(
        text.contains("broken"),
        "the generator's own message must still show\n{text}"
    );
    assert!(
        !text.contains("is not writable"),
        "no note when the directory is writable\n{text}"
    );
}
