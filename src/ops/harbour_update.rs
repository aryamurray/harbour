//! Implementation of `harbour update`.

use std::collections::BTreeMap;

use anyhow::Result;

use crate::core::Workspace;
use crate::ops::lockfile::load_lockfile;
use crate::ops::resolve::update_resolve;
use crate::resolver::Resolve;
use crate::sources::SourceCache;

/// Options for update command.
#[derive(Debug, Clone, Default)]
pub struct UpdateOptions {
    /// Specific packages to update (empty = all). Every other locked
    /// package keeps its version unless one of these forces it to move.
    pub packages: Vec<String>,

    /// Perform aggressive update (ignore lockfile completely)
    pub aggressive: bool,

    /// Dry run - show what would be updated without changing lockfile
    pub dry_run: bool,
}

/// One difference between the lockfile before an update and the resolve after.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageChange {
    Added {
        name: String,
        version: String,
    },
    Updated {
        name: String,
        from: String,
        to: String,
    },
    Removed {
        name: String,
        version: String,
    },
}

/// What an update resolved, and how that differs from the lockfile it replaced.
#[derive(Debug)]
pub struct UpdateReport {
    pub resolve: Resolve,
    /// Sorted by package name. Empty when the lockfile was already current.
    pub changes: Vec<PackageChange>,
}

/// Update the lockfile by re-resolving dependencies.
///
/// This is the ONLY command that modifies the lockfile.
pub fn update(
    ws: &Workspace,
    source_cache: &mut SourceCache,
    opts: &UpdateOptions,
) -> Result<UpdateReport> {
    if opts.dry_run {
        tracing::info!("Dry run - lockfile will not be modified");
    }

    // An unreadable or incompatible lockfile is replaced wholesale, so every
    // package in the new resolve counts as added.
    let before = load_lockfile(&ws.lockfile_path()).ok().flatten();
    let resolve = update_resolve(ws, source_cache, opts.dry_run, &opts.packages)?;
    let mut changes = changes_between(before.as_ref(), &resolve);
    // The workspace's own packages are not dependencies being updated.
    let is_own = |name: &str| {
        ws.member(name).is_some() || (!ws.is_virtual() && ws.root_package().name().as_str() == name)
    };
    changes.retain(|c| !is_own(change_name(c)));

    Ok(UpdateReport { resolve, changes })
}

/// The packages added, removed or moved to another version between `before`
/// (no lockfile: `None`) and `after`. A package is identified by its name and
/// source, so the same name from a different source is a removal plus an
/// addition.
pub fn changes_between(before: Option<&Resolve>, after: &Resolve) -> Vec<PackageChange> {
    fn versions(resolve: &Resolve) -> BTreeMap<(String, String), String> {
        resolve
            .packages()
            .map(|(id, _)| {
                (
                    (id.name().to_string(), id.source_id().url().to_string()),
                    id.version().to_string(),
                )
            })
            .collect()
    }
    let old = before.map(versions).unwrap_or_default();
    let new = versions(after);

    let mut changes = Vec::new();
    for (key, to) in &new {
        match old.get(key) {
            None => changes.push(PackageChange::Added {
                name: key.0.clone(),
                version: to.clone(),
            }),
            Some(from) if from != to => changes.push(PackageChange::Updated {
                name: key.0.clone(),
                from: from.clone(),
                to: to.clone(),
            }),
            Some(_) => {}
        }
    }
    for (key, version) in &old {
        if !new.contains_key(key) {
            changes.push(PackageChange::Removed {
                name: key.0.clone(),
                version: version.clone(),
            });
        }
    }
    changes.sort_by(|a, b| change_name(a).cmp(change_name(b)));
    changes
}

fn change_name(change: &PackageChange) -> &str {
    match change {
        PackageChange::Added { name, .. }
        | PackageChange::Updated { name, .. }
        | PackageChange::Removed { name, .. } => name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::GlobalContext;
    use tempfile::TempDir;

    fn create_test_workspace(dir: &std::path::Path) {
        std::fs::write(
            dir.join("Harbor.toml"),
            r#"
[package]
name = "test"
version = "1.0.0"

[targets.test]
kind = "exe"
sources = ["src/**/*.c"]
"#,
        )
        .unwrap();

        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/main.c"), "int main() { return 0; }").unwrap();
    }

    #[test]
    fn test_update() {
        let tmp = TempDir::new().unwrap();
        create_test_workspace(tmp.path());

        let ctx = GlobalContext::with_cwd_isolated(tmp.path().to_path_buf()).unwrap();
        let ws = Workspace::new(&tmp.path().join("Harbor.toml"), &ctx).unwrap();

        let mut cache = SourceCache::new(tmp.path().join("cache"));
        let opts = UpdateOptions::default();

        let report = update(&ws, &mut cache, &opts).unwrap();
        assert!(!report.resolve.is_empty());
        // The workspace's own package is never reported as a change.
        assert!(report.changes.is_empty(), "{:?}", report.changes);
        assert!(ws.lockfile_path().exists());

        let again = update(&ws, &mut cache, &opts).unwrap();
        assert!(again.changes.is_empty(), "{:?}", again.changes);
    }

    fn resolve_of(packages: &[(&str, &str)]) -> Resolve {
        let source = crate::core::SourceId::for_registry(
            &url::Url::parse("https://example.com/registry").unwrap(),
        )
        .unwrap();
        let mut resolve = Resolve::new();
        for (name, version) in packages {
            let id = crate::core::PackageId::new(*name, version.parse().unwrap(), source);
            resolve.add_package(id, crate::core::Summary::new(id, vec![], None));
        }
        resolve
    }

    #[test]
    fn changes_between_reports_added_updated_and_removed_by_name() {
        let before = resolve_of(&[("zlib", "1.3.0"), ("lz4", "1.10.0"), ("cjson", "1.7.18")]);
        let after = resolve_of(&[("zlib", "1.3.1"), ("cjson", "1.7.18"), ("zstd", "1.5.7")]);
        assert_eq!(
            changes_between(Some(&before), &after),
            [
                PackageChange::Removed {
                    name: "lz4".into(),
                    version: "1.10.0".into()
                },
                PackageChange::Updated {
                    name: "zlib".into(),
                    from: "1.3.0".into(),
                    to: "1.3.1".into()
                },
                PackageChange::Added {
                    name: "zstd".into(),
                    version: "1.5.7".into()
                },
            ]
        );
        assert!(changes_between(Some(&after), &after).is_empty());
    }
}
