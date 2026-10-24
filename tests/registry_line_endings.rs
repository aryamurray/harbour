//! A registry's patches, and the upstream trees they are applied to, must
//! reach disk byte for byte, whatever line-ending conversion git is set to.
//!
//! A shim's patch is verified by the sha256 of its exact bytes and applied with
//! `git apply`. Git for Windows converts line endings on checkout by default
//! (`core.autocrlf = true`), so on a default Windows machine every patch in
//! every registry failed verification -- found by CI, not by reasoning. These
//! tests reproduce that default on any operating system with a `~/.gitconfig`
//! in the test's own `HOME` (see `common::home_that_converts_line_endings`).

mod common;

use std::fs;

use harbour::sources::registry::generate::{ensure_patch_attributes, PatchAttributes, PATCH_RULE};
use harbour::test_support::fixtures::local_registry;
use tempfile::TempDir;

/// An upstream with `lib.h`/`lib.c` (LF line endings), committed, and the rev.
fn upstream(tmp: &TempDir, answer: u32) -> (std::path::PathBuf, String) {
    let dir = tmp.path().join("upstream-lib");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("lib.h"), "int lib_value(void);\n").unwrap();
    fs::write(
        dir.join("lib.c"),
        format!("int lib_value(void) {{ return {answer}; }}\n"),
    )
    .unwrap();
    let rev = local_registry::commit_all(&dir, "upstream").unwrap();
    (dir, rev)
}

/// Replace the registry's `.gitattributes` with one that says nothing about
/// patches and commit that, so a checkout of the registry is subject to the
/// user's line-ending conversion exactly as an unprotected registry would be.
fn drop_the_patch_rule(registry: &std::path::Path) {
    fs::write(registry.join(".gitattributes"), "# (no rule for patches)\n").unwrap();
    local_registry::commit_all(registry, "drop the rule").unwrap();
}

/// The registry is cloned without its line endings converted, so a patch keeps
/// the bytes its hash was taken from -- even with no `.gitattributes` rule to
/// protect it. Without this, `core.autocrlf = true` turns every LF in the patch
/// into CRLF and verification fails: "patch file hash mismatch".
#[test]
fn a_patch_survives_a_registry_checkout_that_converts_line_endings() {
    let tmp = TempDir::new().unwrap();
    let home = common::home_that_converts_line_endings(&tmp);
    let registry = tmp.path().join("registry");
    local_registry::init(&registry).unwrap();

    let (upstream_dir, rev) = upstream(&tmp, 42);
    let (patch, sha) = common::write_patch(
        &registry,
        "lib",
        "0.1.0",
        &common::manifest_patch_text("lib", "0.1.0"),
    );
    local_registry::add_shim(
        &registry,
        "lib",
        "0.1.0",
        &local_registry::file_url(&upstream_dir),
        &rev,
        &[(patch.as_str(), sha.as_str())],
    )
    .unwrap();
    local_registry::publish(&registry, &tmp.path().join("scratch")).unwrap();
    drop_the_patch_rule(&registry);

    let app = tmp.path().join("app");
    common::write_app(&app, "lib = \"^0.1\"", &["lib.h"], "lib_value()");
    let run = common::run_in_registry(
        &home,
        &app,
        &["build"],
        &local_registry::file_url(&registry),
    );
    assert!(run.success, "the patch must verify and apply\n{}", run.text);
    assert_eq!(common::run_exe(&app, "app"), "42");
}

/// The *upstream* tree is cloned without conversion too, so every file in it is
/// exactly what was committed.
///
/// This is asserted on the bytes of the cached tree, not on whether a patch
/// applies: modern `git apply` tolerates LF context lines against a CRLF
/// checkout, so a patch applying proves nothing here (the first version of this
/// test passed with the conversion left in). What a converted tree breaks is
/// everything else that reads those files -- a generator script with a CR at the
/// end of every line, a header whose bytes are hashed. Here the patched file
/// must come out with LF endings only.
#[test]
fn an_upstream_tree_is_checked_out_with_its_committed_bytes() {
    let tmp = TempDir::new().unwrap();
    let home = common::home_that_converts_line_endings(&tmp);
    let registry = tmp.path().join("registry");
    local_registry::init(&registry).unwrap();

    // Upstream returns 41; the patch makes it 42 *and* adds the manifest.
    let (upstream_dir, rev) = upstream(&tmp, 41);
    let patch_text = format!(
        "{}diff --git a/lib.c b/lib.c\n--- a/lib.c\n+++ b/lib.c\n@@ -1 +1 @@\n\
         -int lib_value(void) {{ return 41; }}\n+int lib_value(void) {{ return 42; }}\n",
        common::manifest_patch_text("lib", "0.1.0")
    );
    let (patch, sha) = common::write_patch(&registry, "lib", "0.1.0", &patch_text);
    local_registry::add_shim(
        &registry,
        "lib",
        "0.1.0",
        &local_registry::file_url(&upstream_dir),
        &rev,
        &[(patch.as_str(), sha.as_str())],
    )
    .unwrap();
    local_registry::publish(&registry, &tmp.path().join("scratch")).unwrap();

    let app = tmp.path().join("app");
    common::write_app(&app, "lib = \"^0.1\"", &["lib.h"], "lib_value()");
    let run = common::run_in_registry(
        &home,
        &app,
        &["build"],
        &local_registry::file_url(&registry),
    );
    assert!(run.success, "the patch must apply\n{}", run.text);
    assert_eq!(common::run_exe(&app, "app"), "42");

    // The cached, patched copy of upstream: find it under the harbour home.
    let cached: Vec<_> = common::walk_files(&home)
        .into_iter()
        .filter(|p| p.file_name().is_some_and(|n| n == "lib.c"))
        .filter(|p| !p.components().any(|c| c.as_os_str() == ".git"))
        .collect();
    assert_eq!(cached.len(), 1, "expected one cached lib.c: {cached:?}");
    assert_eq!(
        fs::read(&cached[0]).unwrap(),
        b"int lib_value(void) { return 42; }\n",
        "the upstream checkout must keep LF line endings: {}",
        cached[0].display()
    );
}

/// Publishing a registry that lacks the rule adds it, so the format is kept by
/// the tool every publish goes through instead of by maintainers remembering.
#[test]
fn publishing_adds_the_patch_rule_to_a_registry_that_lacks_it() {
    let tmp = TempDir::new().unwrap();
    let registry = tmp.path().join("registry");
    local_registry::init(&registry).unwrap();
    fs::write(registry.join(".gitattributes"), "*.c text eol=lf\n").unwrap();

    // A package that carries its own manifest needs no patch.
    let (upstream_dir, _) = upstream(&tmp, 42);
    fs::write(
        upstream_dir.join("Harbour.toml"),
        "[package]\nname = \"lib\"\nversion = \"0.1.0\"\n\n[targets.lib]\n\
         kind = \"staticlib\"\nsources = [\"lib.c\"]\n",
    )
    .unwrap();
    let rev = local_registry::commit_all(&upstream_dir, "manifest").unwrap();
    local_registry::add_shim(
        &registry,
        "lib",
        "0.1.0",
        &local_registry::file_url(&upstream_dir),
        &rev,
        &[],
    )
    .unwrap();

    local_registry::publish(&registry, &tmp.path().join("scratch")).unwrap();

    let attrs = fs::read_to_string(registry.join(".gitattributes")).unwrap();
    assert!(
        attrs.starts_with("*.c text eol=lf\n"),
        "existing rules are kept: {attrs:?}"
    );
    assert!(attrs.lines().any(|l| l == PATCH_RULE), "{attrs:?}");
    assert_eq!(
        ensure_patch_attributes(&registry).unwrap(),
        PatchAttributes::AlreadySet
    );
}

/// A registry that deliberately converts patch line endings contradicts the
/// format; publishing says so instead of rewriting a maintainer's file.
#[test]
fn publishing_refuses_a_registry_that_converts_patch_line_endings() {
    let tmp = TempDir::new().unwrap();
    let registry = tmp.path().join("registry");
    local_registry::init(&registry).unwrap();
    fs::write(registry.join(".gitattributes"), "*.patch text eol=crlf\n").unwrap();

    let err = local_registry::publish(&registry, &tmp.path().join("scratch"))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("*.patch text eol=crlf") && err.contains(PATCH_RULE),
        "{err}"
    );
    assert_eq!(
        fs::read_to_string(registry.join(".gitattributes")).unwrap(),
        "*.patch text eol=crlf\n",
        "the maintainer's file must be left as it was"
    );
}
