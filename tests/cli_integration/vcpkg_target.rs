//! Which vcpkg triplet a build consults while *resolving* its dependencies.
//!
//! vcpkg keeps one `installed/<triplet>` tree per triplet, so a cross build has
//! to look for a port in the target's tree, not the host's.

use std::fs;
use std::path::Path;

use assert_cmd::prelude::*;

use crate::common::*;

/// A vcpkg root with `foo` installed for `triplet` only, and no `vcpkg`
/// binary, so a port that is not already installed cannot be fetched.
fn vcpkg_root_with_foo(root: &Path, triplet: &str) {
    let share = root.join("installed").join(triplet).join("share/foo");
    fs::create_dir_all(&share).unwrap();
    fs::write(share.join("vcpkg.json"), r#"{ "version": "1.2.3" }"#).unwrap();
}

/// What `harbour build --target-triple <target>` said for a project that
/// depends on the vcpkg port `foo`, with `foo` installed only for
/// `installed_for`.
fn build_for_target(target: &str, installed_for: &str) -> (bool, String) {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let root = tmp.path().join("vcpkg-root");
    vcpkg_root_with_foo(&root, installed_for);

    harbour(&home)
        .current_dir(tmp.path())
        .args(["new", "--lib", "app"])
        .assert()
        .success();
    let project = tmp.path().join("app");
    let mut manifest = fs::read_to_string(project.join("Harbour.toml")).unwrap();
    manifest.push_str("\n[dependencies]\nfoo = { vcpkg = true }\n");
    fs::write(project.join("Harbour.toml"), manifest).unwrap();

    let out = harbour(&home)
        .current_dir(&project)
        .env("VCPKG_ROOT", &root)
        .args(["build", "--target-triple", target])
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

/// The two Linux triples map to different vcpkg triplets (`x64-linux`,
/// `arm64-linux`), so whatever the host is, at most one of them is the host's
/// own. With the port installed for `arm64-linux` only, the *target* has to
/// decide whether resolution finds it.
#[test]
fn resolution_consults_the_targets_vcpkg_triplet_not_the_hosts() {
    // Installed for the target's triplet: resolution finds it. (The build may
    // still fail afterwards for lack of a cross toolchain; that is not what is
    // under test.)
    let (_, found) = build_for_target("aarch64-unknown-linux-gnu", "arm64-linux");
    assert!(
        !found.contains("vcpkg binary not found"),
        "foo is installed for arm64-linux, the target's triplet, but resolution \
         did not find it:\n{}",
        found
    );

    // Installed for a different triplet than the target's: not found, and with
    // no vcpkg binary it cannot be installed either.
    let (ok, missing) = build_for_target("x86_64-unknown-linux-gnu", "arm64-linux");
    assert!(
        !ok && missing.contains("vcpkg binary not found"),
        "foo is installed for arm64-linux only, but the target is x64-linux; \
         resolution should have looked in the x64-linux tree and missed:\n{}",
        missing
    );
}
