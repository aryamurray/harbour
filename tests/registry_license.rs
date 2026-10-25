//! The license rules are enforced when a registry is *published*
//! (`harbour registry index`), not when it is read: a package with a shim
//! written before the rules stays usable, and publishing says how to fix it.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use harbour::test_support::fixtures::local_registry;
use tempfile::TempDir;

/// A published registry with one library, `lib`, whose shim is then rewritten
/// by `edit_shim`. The tier-1 index is generated *before* the edit, so it is as
/// a registry published under older rules would be.
fn registry_with_edited_shim(tmp: &TempDir, edit_shim: impl Fn(&str) -> String) -> PathBuf {
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

    let (patch, sha) = common::write_patch(
        &registry,
        "lib",
        "0.1.0",
        &common::manifest_patch_text("lib", "0.1.0"),
    );
    let shim_dir = local_registry::add_shim(
        &registry,
        "lib",
        "0.1.0",
        &local_registry::file_url(&upstream),
        &rev,
        &[(patch.as_str(), sha.as_str())],
    )
    .unwrap();
    local_registry::publish(&registry, &tmp.path().join("scratch")).unwrap();

    let shim = shim_dir.join("0.1.0.toml");
    let text = fs::read_to_string(&shim).unwrap();
    fs::write(&shim, edit_shim(&text)).unwrap();
    local_registry::commit_all(&registry, "edit the shim").unwrap();
    registry
}

fn index(home: &Path, registry: &Path) -> common::Run {
    let out = common::harbour(home)
        .current_dir(registry)
        .args(["registry", "index"])
        .arg(registry)
        .output()
        .unwrap();
    common::Run {
        success: out.status.success(),
        text: format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    }
}

#[test]
fn publishing_a_shim_without_a_license_fails_and_says_how_to_fix_it() {
    let tmp = TempDir::new().unwrap();
    let home = common::home(&tmp);
    let registry = registry_with_edited_shim(&tmp, |t| {
        t.replace("[metadata]\nlicense = \"MIT\"\n", "")
    });

    let run = index(&home, &registry);
    assert!(!run.success, "indexing must refuse\n{}", run.text);
    assert!(run.text.contains("lib"), "names the shim\n{}", run.text);
    assert!(run.text.contains("declares no license"), "{}", run.text);
    assert!(run.text.contains("LicenseRef-"), "{}", run.text);
}

#[test]
fn an_old_style_license_resolves_for_a_consumer_but_cannot_be_published() {
    let tmp = TempDir::new().unwrap();
    let home = common::home(&tmp);
    let registry = registry_with_edited_shim(&tmp, |t| {
        t.replace("license = \"MIT\"", "license = \"GPL-2.0\"")
    });
    let registry_url = local_registry::file_url(&registry);

    // A consumer still gets the package, with a warning.
    let app = tmp.path().join("app");
    common::write_app(&app, "lib = \"^0.1\"", &["lib.h"], "lib_value()");
    let run = common::run_in_registry(&home, &app, &["build"], &registry_url);
    assert!(run.success, "reading must stay lenient\n{}", run.text);
    assert_eq!(common::run_exe(&app, "app"), "42");
    assert!(run.text.contains("GPL-2.0"), "warns\n{}", run.text);

    // Publishing is where it is refused, with the replacement.
    let run = index(&home, &registry);
    assert!(!run.success, "indexing must refuse\n{}", run.text);
    assert!(
        run.text.contains("use `GPL-2.0-only` instead of `GPL-2.0`"),
        "{}",
        run.text
    );
}
