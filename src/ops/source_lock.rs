//! The lockfile's record of which source files a fetched package compiles.
//!
//! A shim names a target's sources with globs, and a glob is a promise about a
//! tree Harbour does not own. A version bump that adds a file, a patch that
//! stops applying, or an `exclude` that grew too greedy all change what is
//! compiled while every checksum still matches. Harbour.lock records the
//! expanded list per target, so such a change is a visible lock diff and a
//! warning instead of a different library.
//!
//! Only fetched (git and registry) packages are recorded. A workspace member or
//! path dependency is the user's own tree, where every new `.c` file would be a
//! lock change. The recording is a function of the package alone -- see
//! [`crate::builder::plan::SourceList`] -- so it is the same on every machine.

use std::path::Path;

use anyhow::{bail, Result};

use crate::builder::plan::SourceList;
use crate::resolver::encode::{LockedPackage, Lockfile};

/// How many file names a message lists per side before saying "and N more".
const NAMES_SHOWN: usize = 3;

/// What comparing a build's source lists with the lockfile found.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Reconciled {
    /// The lockfile now differs from what was loaded and should be written.
    pub changed: bool,
    /// Warnings: a pinned package's file set changed.
    pub drift: Vec<String>,
    /// Notes: a package moved to a new version or source and its file set
    /// changed with it.
    pub moved: Vec<String>,
}

/// Compare `observed` with the lists in `lockfile` and update it.
///
/// - No recorded list for a target: recorded, silently. Not drift.
/// - Same list: nothing.
/// - A different list for a package whose version and source are the ones
///   locked: drift. Reported, and the lockfile takes the new list.
/// - A different list for a package a re-resolution moved (`sources_from` is
///   set): reported in `moved`, and the lockfile takes the new list.
///
/// With `locked`, nothing is recorded and nothing is updated: drift is returned
/// for the caller to refuse, and a lockfile with no record stays valid as it
/// is (run a build without `--locked` once to record it).
pub fn reconcile(lockfile: &mut Lockfile, observed: &[SourceList], locked: bool) -> Reconciled {
    let mut out = Reconciled::default();

    // Group by package so a moved package's stale targets can be dropped once.
    let mut packages: Vec<&crate::core::PackageId> = Vec::new();
    for o in observed {
        if !packages.contains(&&o.package) {
            packages.push(&o.package);
        }
    }

    for pkg_id in packages {
        let name = pkg_id.name().to_string();
        let version = pkg_id.version().to_string();
        let source = LockedPackage::source_string(pkg_id);
        let Some(entry) = lockfile
            .packages
            .iter_mut()
            .find(|p| p.name == name && p.version == version && p.source == source)
        else {
            continue;
        };
        let mine = observed.iter().filter(|o| &o.package == pkg_id);

        if let Some(from) = entry.sources_from.clone() {
            if locked {
                continue;
            }
            let old = std::mem::take(&mut entry.sources);
            for o in mine {
                if let Some(before) = old.get(&o.target) {
                    if before != &o.files {
                        let what = if from == version {
                            format!("{name} {version} (source changed)")
                        } else {
                            format!("{name} {from} -> {version}")
                        };
                        out.moved.push(format!(
                            "{what}, target {}: source list changed ({})",
                            o.target,
                            describe(before, &o.files)
                        ));
                    }
                }
                entry.sources.insert(o.target.clone(), o.files.clone());
            }
            entry.sources_from = None;
            out.changed = true;
            continue;
        }

        for o in mine {
            match entry.sources.get(&o.target) {
                None => {
                    if !locked {
                        entry.sources.insert(o.target.clone(), o.files.clone());
                        out.changed = true;
                    }
                }
                Some(before) if before == &o.files => {}
                Some(before) => {
                    out.drift.push(format!(
                        "{name} {version} target {}: sources changed: {}",
                        o.target,
                        describe(before, &o.files)
                    ));
                    if !locked {
                        entry.sources.insert(o.target.clone(), o.files.clone());
                        out.changed = true;
                    }
                }
            }
        }
    }
    out
}

/// `+2 -1 (+a.c, +b.c, -old.c)`: what going from `before` to `after` did.
fn describe(before: &[String], after: &[String]) -> String {
    let added: Vec<&String> = after.iter().filter(|f| !before.contains(f)).collect();
    let removed: Vec<&String> = before.iter().filter(|f| !after.contains(f)).collect();
    let mut names: Vec<String> = Vec::new();
    for (sign, list) in [("+", &added), ("-", &removed)] {
        for f in list.iter().take(NAMES_SHOWN) {
            names.push(format!("{sign}{f}"));
        }
        if list.len() > NAMES_SHOWN {
            names.push(format!("{sign}{} more", list.len() - NAMES_SHOWN));
        }
    }
    format!("+{} -{} ({})", added.len(), removed.len(), names.join(", "))
}

/// Check a build's source lists against the lockfile at `path`, and record
/// them.
///
/// A normal build warns about drift and rewrites the lockfile. With `locked`
/// drift is an error and the lockfile is left alone. Nothing is checked when
/// the lockfile cannot be read; resolution has already dealt with that.
pub fn check_and_record(path: &Path, observed: &[SourceList], locked: bool) -> Result<()> {
    if observed.is_empty() {
        return Ok(());
    }
    let Ok(mut lockfile) = Lockfile::load(path) else {
        return Ok(());
    };
    let result = reconcile(&mut lockfile, observed, locked);

    if locked && !result.drift.is_empty() {
        bail!(
            "the source files of a locked package changed:\n  {}\n\
             hint: Harbour.lock records which files each fetched package compiles, and this \
             would change it; run without `--locked` (or `harbour update`) to accept the change",
            result.drift.join("\n  ")
        );
    }
    for line in &result.drift {
        tracing::warn!("{line}; Harbour.lock updated");
    }
    for line in &result.moved {
        tracing::warn!("{line}; Harbour.lock updated");
    }
    if result.changed {
        lockfile.save(path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
