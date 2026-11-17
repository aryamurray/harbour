//! Does a target end up with the sources it was promised?
//!
//! Source lists are globs, and an empty glob is legal (`src/**/*.S` matches
//! nothing on a platform with no assembly). Left there, a typo'd directory, a
//! manifest rewrite that moves files, or an `exclude` that is too wide produces
//! a library that builds, exits 0 and contains nothing. The rules here close
//! that without taking the legitimate empty glob away:
//!
//! - a target that must produce an artifact from sources (`staticlib`,
//!   `sharedlib`, `exe`) and ends up with none is an **error**;
//! - individual empty patterns are noted at `--verbose`;
//! - under `--strict-sources`, every pattern that applies must match.
//!
//! `header-only` targets never reach this (they have nothing to compile) and a
//! target whose artifact comes from a `custom` recipe is dispatched before
//! sources are read, so it is not asked to have any.

use anyhow::{bail, Result};

use crate::core::target::{ConditionalSources, Language, Target, TargetKind};
use crate::util::fs::{exclude_covers, PatternMatch, SourceExpansion};

/// One source pattern, with where in the manifest it came from.
struct Origin<'a> {
    matched: &'a PatternMatch,
    /// `sources`, or a description of the `when` block that added it.
    from: String,
}

/// The default `sources` for a target that names none.
///
/// Shared with the manifest parser, which applies them; the strict check needs
/// to recognise them, because they are alternatives by design (`.cpp`, `.cc`
/// and `.cxx`) and a project using just one of those is not mistaken.
pub fn default_source_patterns(lang: Language) -> Vec<String> {
    match lang {
        Language::Cxx => vec![
            "src/**/*.cpp".to_string(),
            "src/**/*.cc".to_string(),
            "src/**/*.cxx".to_string(),
        ],
        Language::C => vec!["src/**/*.c".to_string()],
        // Only ever reached if a manifest sets `lang = "asm"` explicitly;
        // assembly is normally mixed into a C or C++ target and dispatched
        // per file.
        Language::Asm => vec!["src/**/*.S".to_string(), "src/**/*.s".to_string()],
    }
}

fn kind_name(kind: TargetKind) -> &'static str {
    match kind {
        TargetKind::Exe => "exe",
        TargetKind::StaticLib => "staticlib",
        TargetKind::SharedLib => "sharedlib",
        TargetKind::HeaderOnly => "header-only",
        TargetKind::Prebuilt => "prebuilt",
    }
}

/// Pair each expanded pattern with the manifest list it came from.
///
/// `expansion.patterns` is the unconditional list followed by each active
/// block's, in the order `Target::resolved_sources` merged them.
fn origins<'a>(
    target: &Target,
    active: &[(usize, String, &ConditionalSources)],
    expansion: &'a SourceExpansion,
) -> Vec<Origin<'a>> {
    let mut matches = expansion.patterns.iter();
    let mut out = Vec::new();
    for _ in 0..target.sources.len() {
        if let Some(matched) = matches.next() {
            out.push(Origin {
                matched,
                from: "sources".to_string(),
            });
        }
    }
    for (index, condition, block) in active {
        for _ in 0..block.sources.len() {
            if let Some(matched) = matches.next() {
                out.push(Origin {
                    matched,
                    from: format!(
                        "`[[targets.{}.when]]` block {index} ({condition})",
                        target.name
                    ),
                });
            }
        }
    }
    out
}

fn describe(m: &PatternMatch) -> String {
    match (m.matched, m.kept) {
        (0, _) => format!("`{}` matched 0 files", m.pattern),
        (n, 0) => format!(
            "`{}` matched {n} file(s), all removed by `exclude`",
            m.pattern
        ),
        (n, k) if n == k => format!("`{}` matched {n} file(s)", m.pattern),
        (n, k) => format!("`{}` matched {n} file(s), {k} after `exclude`", m.pattern),
    }
}

/// Apply the source-coverage rules to one native target.
///
/// `named_excluded` says whether a pattern naming a file is one `exclude`
/// removes; such a file is expected to be absent (LuaJIT drops `lj_vm.S` under
/// MSVC), so strict mode does not count its pattern as empty.
#[allow(clippy::too_many_arguments)]
pub fn check_sources(
    package: &str,
    target: &Target,
    active: &[(usize, String, &ConditionalSources)],
    expansion: &SourceExpansion,
    exclude: &[String],
    strict: bool,
    package_root: &std::path::Path,
) -> Result<()> {
    let origins = origins(target, active, expansion);

    // After `when` merging and after `prebuild` has run: the caller expands
    // sources only then, so generated files are already on disk and counted.
    if expansion.files.is_empty()
        && matches!(
            target.kind,
            TargetKind::Exe | TargetKind::StaticLib | TargetKind::SharedLib
        )
    {
        let mut lines: Vec<String> = origins
            .iter()
            .map(|o| format!("  {}: {}", o.from, describe(o.matched)))
            .collect();
        for (index, condition, block) in active {
            if block.sources.is_empty() {
                lines.push(format!(
                    "  `[[targets.{}.when]]` block {index} ({condition}) is active but lists no \
                     `sources`",
                    target.name
                ));
            }
        }
        if lines.is_empty() {
            lines.push("  `sources` is empty".to_string());
        }
        let removed_by_exclude = origins.iter().any(|o| o.matched.matched > 0);
        let cause = if removed_by_exclude {
            "`exclude` removed every file the patterns matched; narrow it"
        } else {
            "the patterns look in the wrong place; they are relative to the package root, so \
             check the directory the sources are actually in"
        };
        bail!(
            "target '{}' of package '{}' (kind {}) has no source files to compile\n{}\n\
             hint: {}\n\
             hint: package root is {}; if `when` blocks above are not active on this platform \
             they were not counted",
            target.name,
            package,
            kind_name(target.kind),
            lines.join("\n"),
            cause,
            package_root.display()
        );
    }

    let empty: Vec<&Origin> = origins
        .iter()
        .filter(|o| o.matched.matched == 0)
        // A named file `exclude` removes is not expected to exist.
        .filter(|o| {
            o.matched.pattern.contains(['*', '?', '['])
                || !exclude
                    .iter()
                    .any(|e| exclude_covers(e, &o.matched.pattern))
        })
        .collect();
    if empty.is_empty() {
        return Ok(());
    }

    let list = |sep: &str| {
        empty
            .iter()
            .map(|o| format!("`{}` ({})", o.matched.pattern, o.from))
            .collect::<Vec<_>>()
            .join(sep)
    };

    // `sources` left out of the manifest means the defaults, which are
    // alternatives (`.cpp` or `.cc`): requiring each to match would reject
    // every C++ project that uses one extension.
    let defaulted = target.sources == default_source_patterns(target.lang);
    let strict_empty: Vec<&&Origin> = empty
        .iter()
        .filter(|o| !(defaulted && o.from == "sources"))
        .collect();
    if strict && !strict_empty.is_empty() {
        bail!(
            "target '{}' of package '{}': --strict-sources requires every source pattern to \
             match a file, but {} matched none:\n  {}\n\
             hint: remove the pattern, correct it, or move it into a `[[targets.{}.when]]` \
             block for the platform that has those files (an inactive block is not checked)",
            target.name,
            package,
            strict_empty.len(),
            strict_empty
                .iter()
                .map(|o| format!("`{}` ({})", o.matched.pattern, o.from))
                .collect::<Vec<_>>()
                .join("\n  "),
            target.name
        );
    }

    // Quiet by default: an empty glob is a legitimate platform difference, and
    // a warning on every build of `src/**/*.S` would be noise that teaches
    // people to ignore warnings. `--verbose` is where it is looked for.
    tracing::debug!(
        "target '{}' of package '{}': source pattern(s) matched no files: {}",
        target.name,
        package,
        list(", ")
    );
    Ok(())
}
