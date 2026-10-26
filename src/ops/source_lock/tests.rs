use std::collections::BTreeMap;

use semver::Version;
use url::Url;

use super::*;
use crate::core::{PackageId, SourceId};

fn registry_pkg(name: &str, version: &str) -> PackageId {
    let url = Url::parse("https://example.com/reg.git").unwrap();
    PackageId::new(
        name,
        Version::parse(version).unwrap(),
        SourceId::for_registry(&url).unwrap(),
    )
}

fn list(pkg: PackageId, target: &str, files: &[&str]) -> SourceList {
    SourceList {
        package: pkg,
        target: target.to_string(),
        files: files.iter().map(|f| f.to_string()).collect(),
    }
}

fn entry(pkg: &PackageId, sources: &[(&str, &[&str])], from: Option<&str>) -> LockedPackage {
    LockedPackage {
        name: pkg.name().to_string(),
        version: pkg.version().to_string(),
        source: LockedPackage::source_string(pkg),
        checksum: None,
        dependencies: vec![],
        registry_provenance: None,
        sources: sources
            .iter()
            .map(|(t, fs)| (t.to_string(), fs.iter().map(|f| f.to_string()).collect()))
            .collect::<BTreeMap<_, _>>(),
        sources_from: from.map(str::to_string),
    }
}

fn lock_of(entries: Vec<LockedPackage>) -> Lockfile {
    Lockfile {
        version: 1,
        root_manifest_hash: Some("h".into()),
        member_manifest_hashes: vec![],
        packages: entries,
    }
}

#[test]
fn a_target_with_no_record_is_recorded_without_a_warning() {
    let z = registry_pkg("zlib", "1.3.1");
    let mut lock = lock_of(vec![entry(&z, &[], None)]);
    let r = reconcile(&mut lock, &[list(z, "z", &["a.c", "b.c"])], false);
    assert!(r.changed);
    assert!(r.drift.is_empty() && r.moved.is_empty());
    assert_eq!(lock.packages[0].sources["z"], ["a.c", "b.c"]);
}

#[test]
fn the_same_list_changes_nothing() {
    let z = registry_pkg("zlib", "1.3.1");
    let mut lock = lock_of(vec![entry(&z, &[("z", &["a.c", "b.c"])], None)]);
    let r = reconcile(&mut lock, &[list(z, "z", &["a.c", "b.c"])], false);
    assert_eq!(r, Reconciled::default());
}

#[test]
fn a_changed_list_for_the_same_pinned_source_is_drift_and_is_updated() {
    let z = registry_pkg("zlib", "1.3.1");
    let mut lock = lock_of(vec![entry(&z, &[("z", &["a.c"])], None)]);
    let r = reconcile(
        &mut lock,
        &[list(z, "z", &["a.c", "new.c", "other.c"])],
        false,
    );
    assert!(r.changed);
    assert_eq!(
        r.drift,
        ["zlib 1.3.1 target z: sources changed: +2 -0 (+new.c, +other.c)"]
    );
    assert_eq!(lock.packages[0].sources["z"], ["a.c", "new.c", "other.c"]);
}

#[test]
fn drift_names_removed_files_and_truncates_long_lists() {
    let z = registry_pkg("zlib", "1.3.1");
    let mut lock = lock_of(vec![entry(&z, &[("z", &["a.c", "b.c"])], None)]);
    let r = reconcile(
        &mut lock,
        &[list(z, "z", &["n1.c", "n2.c", "n3.c", "n4.c", "n5.c"])],
        false,
    );
    assert_eq!(
        r.drift,
        ["zlib 1.3.1 target z: sources changed: +5 -2 (+n1.c, +n2.c, +n3.c, +2 more, -a.c, -b.c)"]
    );
}

#[test]
fn locked_reports_drift_and_leaves_the_lockfile_alone() {
    let z = registry_pkg("zlib", "1.3.1");
    let mut lock = lock_of(vec![entry(&z, &[("z", &["a.c"])], None)]);
    let r = reconcile(&mut lock, &[list(z, "z", &["a.c", "b.c"])], true);
    assert!(!r.changed);
    assert_eq!(r.drift.len(), 1);
    assert_eq!(lock.packages[0].sources["z"], ["a.c"]);
}

#[test]
fn locked_does_not_record_a_missing_list() {
    let z = registry_pkg("zlib", "1.3.1");
    let mut lock = lock_of(vec![entry(&z, &[], None)]);
    let r = reconcile(&mut lock, &[list(z, "z", &["a.c"])], true);
    assert_eq!(r, Reconciled::default());
    assert!(lock.packages[0].sources.is_empty());
}

#[test]
fn a_moved_package_reports_what_the_move_did_and_takes_the_new_list() {
    let z = registry_pkg("zlib", "1.3.2");
    let mut lock = lock_of(vec![entry(&z, &[("z", &["a.c", "gone.c"])], Some("1.3.1"))]);
    let r = reconcile(&mut lock, &[list(z, "z", &["a.c", "new.c"])], false);
    assert!(r.changed);
    assert!(r.drift.is_empty());
    assert_eq!(
        r.moved,
        ["zlib 1.3.1 -> 1.3.2, target z: source list changed (+1 -1 (+new.c, -gone.c))"]
    );
    assert_eq!(lock.packages[0].sources["z"], ["a.c", "new.c"]);
    assert_eq!(lock.packages[0].sources_from, None);
}

#[test]
fn a_moved_package_with_an_unchanged_list_is_silent_but_clears_the_marker() {
    let z = registry_pkg("zlib", "1.3.2");
    let mut lock = lock_of(vec![entry(&z, &[("z", &["a.c"])], Some("1.3.1"))]);
    let r = reconcile(&mut lock, &[list(z, "z", &["a.c"])], false);
    assert!(r.changed && r.moved.is_empty() && r.drift.is_empty());
    assert_eq!(lock.packages[0].sources_from, None);
}

#[test]
fn a_moved_package_drops_targets_it_no_longer_has() {
    let z = registry_pkg("zlib", "1.3.2");
    let mut lock = lock_of(vec![entry(
        &z,
        &[("z", &["a.c"]), ("old", &["o.c"])],
        Some("1.3.1"),
    )]);
    reconcile(&mut lock, &[list(z, "z", &["a.c"])], false);
    assert!(!lock.packages[0].sources.contains_key("old"));
}

#[test]
fn a_package_not_in_the_lockfile_is_ignored() {
    let z = registry_pkg("zlib", "1.3.1");
    let other = registry_pkg("other", "1.0.0");
    let mut lock = lock_of(vec![entry(&other, &[], None)]);
    let r = reconcile(&mut lock, &[list(z, "z", &["a.c"])], false);
    assert_eq!(r, Reconciled::default());
}

#[test]
fn carry_keeps_lists_for_an_unchanged_package() {
    let z = registry_pkg("zlib", "1.3.1");
    let old = lock_of(vec![entry(&z, &[("z", &["a.c"])], None)]);
    let mut new = lock_of(vec![entry(&z, &[], None)]);
    new.carry_sources_from(&old);
    assert_eq!(new.packages[0].sources["z"], ["a.c"]);
    assert_eq!(new.packages[0].sources_from, None);
}

#[test]
fn carry_marks_lists_of_a_package_that_moved_with_the_old_version() {
    let old_z = registry_pkg("zlib", "1.3.1");
    let new_z = registry_pkg("zlib", "1.3.2");
    let old = lock_of(vec![entry(&old_z, &[("z", &["a.c"])], None)]);
    let mut new = lock_of(vec![entry(&new_z, &[], None)]);
    new.carry_sources_from(&old);
    assert_eq!(new.packages[0].sources["z"], ["a.c"]);
    assert_eq!(new.packages[0].sources_from.as_deref(), Some("1.3.1"));
}

#[test]
fn carry_keeps_the_original_marker_across_a_second_move() {
    let a = registry_pkg("zlib", "1.3.2");
    let b = registry_pkg("zlib", "1.3.3");
    let old = lock_of(vec![entry(&a, &[("z", &["a.c"])], Some("1.3.1"))]);
    let mut new = lock_of(vec![entry(&b, &[], None)]);
    new.carry_sources_from(&old);
    assert_eq!(new.packages[0].sources_from.as_deref(), Some("1.3.1"));
}

#[test]
fn carry_does_not_guess_between_several_old_versions() {
    let a = registry_pkg("zlib", "1.3.0");
    let b = registry_pkg("zlib", "1.3.1");
    let c = registry_pkg("zlib", "1.3.2");
    let old = lock_of(vec![
        entry(&a, &[("z", &["a.c"])], None),
        entry(&b, &[("z", &["b.c"])], None),
    ]);
    let mut new = lock_of(vec![entry(&c, &[], None)]);
    new.carry_sources_from(&old);
    assert!(new.packages[0].sources.is_empty());
}

#[test]
fn an_old_lockfile_without_the_section_loads_and_gains_it_on_the_next_write() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("Harbour.lock");
    std::fs::write(
        &path,
        "version = 1\nroot_manifest_hash = \"h\"\n\n[[package]]\nname = \"zlib\"\nversion = \"1.3.1\"\nsource = \"registry+https://example.com/reg.git\"\n",
    )
    .unwrap();
    let mut lock = Lockfile::load(&path).unwrap();
    assert!(lock.packages[0].sources.is_empty());
    lock.packages[0]
        .sources
        .insert("z".into(), vec!["a.c".into(), "b.c".into()]);
    lock.save(&path).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("[package.sources]"), "{text}");
    assert!(text.contains("z = ["), "{text}");
    assert_eq!(
        Lockfile::load(&path).unwrap().packages[0].sources["z"],
        ["a.c", "b.c"]
    );
}

#[test]
fn the_written_section_is_sorted_by_target_whatever_the_insertion_order() {
    let z = registry_pkg("zlib", "1.3.1");
    let mut e = entry(&z, &[], None);
    e.sources.insert("zz".into(), vec!["z.c".into()]);
    e.sources.insert("aa".into(), vec!["a.c".into()]);
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("Harbour.lock");
    lock_of(vec![e]).save(&path).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(
        text.find("aa = ").unwrap() < text.find("zz = ").unwrap(),
        "{text}"
    );
}

#[test]
fn check_and_record_leaves_the_file_untouched_when_nothing_changed() {
    let z = registry_pkg("zlib", "1.3.1");
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("Harbour.lock");
    lock_of(vec![entry(&z, &[("z", &["a.c"])], None)])
        .save(&path)
        .unwrap();
    let before = std::fs::read(&path).unwrap();
    let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
    check_and_record(&path, &[list(z, "z", &["a.c"])], false).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), mtime);
}

#[test]
fn check_and_record_locked_refuses_drift_and_does_not_write() {
    let z = registry_pkg("zlib", "1.3.1");
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("Harbour.lock");
    lock_of(vec![entry(&z, &[("z", &["a.c"])], None)])
        .save(&path)
        .unwrap();
    let before = std::fs::read(&path).unwrap();
    let err = check_and_record(&path, &[list(z, "z", &["a.c", "b.c"])], true)
        .unwrap_err()
        .to_string();
    assert!(err.contains("zlib 1.3.1 target z"), "{err}");
    assert!(err.contains("+b.c"), "{err}");
    assert!(err.contains("--locked"), "{err}");
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn check_and_record_rewrites_the_lockfile_on_drift() {
    let z = registry_pkg("zlib", "1.3.1");
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("Harbour.lock");
    lock_of(vec![entry(&z, &[("z", &["a.c"])], None)])
        .save(&path)
        .unwrap();
    check_and_record(&path, &[list(z, "z", &["a.c", "b.c"])], false).unwrap();
    assert_eq!(
        Lockfile::load(&path).unwrap().packages[0].sources["z"],
        ["a.c", "b.c"]
    );
}
