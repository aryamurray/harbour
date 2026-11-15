//! A target's `warnings` setting: how much the compiler should complain
//! about this target's own sources.
//!
//! Intent, not spelling, for the same reason `[profile] opt_level` is: GCC
//! and clang say `-Wall -Wextra`, MSVC says `/W4`, and a manifest that had to
//! name the compiler to turn warnings on needed three `when` blocks to do it.
//! Nothing here knows a flag; each toolchain backend spells the level
//! ([`Toolchain::warning_flags`]).
//!
//! [`Toolchain::warning_flags`]: crate::builder::toolchain::Toolchain::warning_flags

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::core::PackageId;

/// How many warnings a target asks its compiler for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WarningLevel {
    /// `warnings = "none"` -- silence the compiler.
    None,
    /// `warnings = "default"`, or no `warnings` key: Harbour passes nothing
    /// and the compiler's own default applies.
    #[default]
    Default,
    /// `warnings = "all"` -- the set a project is expected to keep clean.
    All,
    /// `warnings = "pedantic"` -- `all`, plus strict-ISO diagnostics where
    /// the compiler has them.
    Pedantic,
}

impl WarningLevel {
    /// Every manifest value, in the order error messages list them.
    pub const VALUES: [&'static str; 4] = ["none", "default", "all", "pedantic"];

    /// The manifest spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            WarningLevel::None => "none",
            WarningLevel::Default => "default",
            WarningLevel::All => "all",
            WarningLevel::Pedantic => "pedantic",
        }
    }
}

impl fmt::Display for WarningLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An unrecognised `warnings` value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WarningLevelParseError(String);

impl fmt::Display for WarningLevelParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown `warnings` value `{}`\nhelp: expected one of {}",
            self.0,
            WarningLevel::VALUES.join(", ")
        )
    }
}

impl std::error::Error for WarningLevelParseError {}

impl FromStr for WarningLevel {
    type Err = WarningLevelParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "none" => Ok(WarningLevel::None),
            "default" => Ok(WarningLevel::Default),
            "all" => Ok(WarningLevel::All),
            "pedantic" => Ok(WarningLevel::Pedantic),
            other => Err(WarningLevelParseError(other.to_string())),
        }
    }
}

impl Serialize for WarningLevel {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for WarningLevel {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// What a target asked for: `warnings` and `warnings_as_errors` together.
///
/// The pair travels as one value from the manifest to the compile step, so
/// the build, `compile_commands.json` and `harbour flags` cannot disagree
/// about either half.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WarningOptions {
    /// `warnings`
    #[serde(default)]
    pub level: WarningLevel,
    /// `warnings_as_errors`
    #[serde(default)]
    pub as_errors: bool,
}

impl WarningOptions {
    /// Whether this asks the compiler for nothing at all.
    pub fn is_default(&self) -> bool {
        *self == WarningOptions::default()
    }

    /// What is left of this once the dependency cap has been applied.
    ///
    /// A fetched package can only ask for *fewer* warnings than the
    /// compiler's default: `none` is kept, `all` and `pedantic` become
    /// `default`, and `warnings_as_errors` is dropped. Whether the cap
    /// applies to a given package at all is [`DepWarnings::caps`]'s question,
    /// not this function's.
    pub fn capped(self) -> Self {
        WarningOptions {
            level: match self.level {
                WarningLevel::None => WarningLevel::None,
                _ => WarningLevel::Default,
            },
            as_errors: false,
        }
    }

    /// Whether [`capped`](Self::capped) would change anything.
    pub fn exceeds_cap(self) -> bool {
        self.capped() != self
    }

    /// The manifest keys that spell this, for quoting back at the user:
    /// `warnings = "all", warnings_as_errors = true`. Only what was set.
    pub fn describe(self) -> String {
        let mut keys = Vec::new();
        if self.level != WarningLevel::Default {
            keys.push(format!("warnings = \"{}\"", self.level));
        }
        if self.as_errors {
            keys.push("warnings_as_errors = true".to_string());
        }
        if keys.is_empty() {
            keys.push("warnings = \"default\"".to_string());
        }
        keys.join(", ")
    }
}

/// Which packages are built with their own `warnings` settings, and which
/// have them capped ([`WarningOptions::capped`]).
///
/// A package on disk (the one being worked on, a workspace member, a `path`
/// dependency) always gets exactly what its manifest says. A package fetched
/// from a registry, git or vcpkg is somebody else's code being built as a
/// dependency: its author's `warnings = "all"` would print diagnostics the
/// consumer cannot act on, and its `warnings_as_errors` would fail the
/// consumer's build the day a newer compiler grows a warning. So a fetched
/// package is capped -- unless the person building says otherwise, in one of
/// the two ways recorded here.
///
/// Only the workspace being built can say so. A `warnings = true` that a
/// *dependency* writes about one of its own dependencies is never collected
/// into [`packages`](Self::packages): a library must not be able to switch
/// diagnostics on in its consumer's build, which is the very thing the cap
/// exists to prevent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DepWarnings {
    /// `--dep-warnings`: lift the cap for every fetched package.
    pub all: bool,
    /// Packages some member of the workspace being built depends on with
    /// `warnings = true`, by name.
    pub packages: BTreeSet<String>,
}

impl DepWarnings {
    /// Whether `package`'s warning settings are capped in this build.
    pub fn caps(&self, package: PackageId) -> bool {
        !package.source_id().is_path()
            && !self.all
            && !self.packages.contains(package.name().as_str())
    }

    /// The settings `package`'s target is compiled with, given what its
    /// manifest `asked` for.
    pub fn apply(&self, package: PackageId, asked: WarningOptions) -> WarningOptions {
        if self.caps(package) {
            asked.capped()
        } else {
            asked
        }
    }
}

/// One target whose manifest asked for more than the cap allows, and did
/// not get it. Collected while planning so the build can say so once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CappedWarnings {
    /// The fetched package.
    pub package: String,
    /// Its target.
    pub target: String,
    /// What the target's manifest asked for.
    pub asked: WarningOptions,
}

/// The `--verbose` note for a build that capped something, or `None` when it
/// capped nothing.
///
/// One message per build however many packages are involved, naming each
/// package once with what its manifest asked for, and both ways to lift the
/// cap.
pub fn capped_warnings_note(capped: &[CappedWarnings]) -> Option<String> {
    let mut by_package: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for entry in capped {
        let asked = by_package.entry(entry.package.as_str()).or_default();
        let described = entry.asked.describe();
        if !asked.contains(&described) {
            asked.push(described);
        }
    }
    if by_package.is_empty() {
        return None;
    }
    let packages: Vec<String> = by_package
        .iter()
        .map(|(package, asked)| format!("{package} ({})", asked.join("; ")))
        .collect();
    Some(format!(
        "warning settings capped for {} fetched {}: {}\n\
         note: a package fetched from a registry or git is compiled with the \
         compiler's default warnings whatever its own manifest asks for \
         (`warnings = \"none\"` is kept). To apply a dependency's own settings, \
         write `warnings = true` on its `[dependencies]` entry in this \
         workspace, or pass `--dep-warnings` for all of them",
        packages.len(),
        if packages.len() == 1 {
            "package"
        } else {
            "packages"
        },
        packages.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_value_parses_back_to_itself() {
        for value in WarningLevel::VALUES {
            let level: WarningLevel = value.parse().unwrap();
            assert_eq!(level.as_str(), value);
        }
    }

    #[test]
    fn an_unknown_value_names_the_valid_ones() {
        let err = "everything"
            .parse::<WarningLevel>()
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("unknown `warnings` value `everything`"),
            "{err}"
        );
        assert!(err.contains("none, default, all, pedantic"), "{err}");
    }

    #[test]
    fn the_cap_only_lets_a_package_ask_for_fewer_warnings() {
        let strict = WarningOptions {
            level: WarningLevel::Pedantic,
            as_errors: true,
        };
        assert_eq!(strict.capped(), WarningOptions::default());
        assert!(strict.exceeds_cap());

        let quiet = WarningOptions {
            level: WarningLevel::None,
            as_errors: true,
        };
        assert_eq!(
            quiet.capped(),
            WarningOptions {
                level: WarningLevel::None,
                as_errors: false,
            }
        );
        assert!(quiet.exceeds_cap());

        // Nothing to cap: the note and `harbour flags` must stay silent.
        let silent = WarningOptions {
            level: WarningLevel::None,
            as_errors: false,
        };
        assert!(!silent.exceeds_cap());
        assert!(!WarningOptions::default().exceeds_cap());
    }

    fn fetched(name: &str) -> PackageId {
        let url = url::Url::parse("https://example.com/repo.git").unwrap();
        let source = crate::core::SourceId::for_git(
            &url,
            crate::core::source_id::GitReference::Tag("v1".to_string()),
        )
        .unwrap();
        PackageId::new(name, semver::Version::new(1, 0, 0), source)
    }

    fn on_disk(name: &str) -> PackageId {
        let tmp = tempfile::TempDir::new().unwrap();
        let source = crate::core::SourceId::for_path(tmp.path()).unwrap();
        PackageId::new(name, semver::Version::new(1, 0, 0), source)
    }

    /// The whole decision: a package on disk is never capped; a fetched one
    /// is, unless it was named or everything was lifted.
    #[test]
    fn the_cap_applies_to_fetched_packages_nobody_lifted_it_for() {
        let strict = WarningOptions {
            level: WarningLevel::All,
            as_errors: true,
        };
        let default = DepWarnings::default();
        assert!(!default.caps(on_disk("local")));
        assert_eq!(default.apply(on_disk("local"), strict), strict);
        assert!(default.caps(fetched("zlib")));
        assert_eq!(
            default.apply(fetched("zlib"), strict),
            WarningOptions::default()
        );

        let named = DepWarnings {
            all: false,
            packages: BTreeSet::from(["zlib".to_string()]),
        };
        assert!(!named.caps(fetched("zlib")));
        assert_eq!(named.apply(fetched("zlib"), strict), strict);
        assert!(named.caps(fetched("other")), "only the package named");

        let all = DepWarnings {
            all: true,
            packages: BTreeSet::new(),
        };
        assert!(!all.caps(fetched("zlib")));
        assert!(!all.caps(fetched("other")));
    }

    #[test]
    fn describe_quotes_only_the_keys_that_were_set() {
        let describe = |level, as_errors| WarningOptions { level, as_errors }.describe();
        assert_eq!(
            describe(WarningLevel::All, true),
            "warnings = \"all\", warnings_as_errors = true"
        );
        assert_eq!(
            describe(WarningLevel::Pedantic, false),
            "warnings = \"pedantic\""
        );
        assert_eq!(
            describe(WarningLevel::Default, true),
            "warnings_as_errors = true"
        );
    }

    #[test]
    fn the_note_names_each_capped_package_once_and_says_how_to_lift_it() {
        assert_eq!(capped_warnings_note(&[]), None);

        let entry = |package: &str, target: &str, level| CappedWarnings {
            package: package.to_string(),
            target: target.to_string(),
            asked: WarningOptions {
                level,
                as_errors: true,
            },
        };
        let note = capped_warnings_note(&[
            entry("zlib", "z", WarningLevel::All),
            entry("zlib", "minizip", WarningLevel::All),
            entry("abc", "abc", WarningLevel::Pedantic),
        ])
        .unwrap();
        assert!(
            note.starts_with(
                "warning settings capped for 2 fetched packages: \
                 abc (warnings = \"pedantic\", warnings_as_errors = true), \
                 zlib (warnings = \"all\", warnings_as_errors = true)\n"
            ),
            "{note}"
        );
        assert!(note.contains("`warnings = true`"), "{note}");
        assert!(note.contains("`--dep-warnings`"), "{note}");
    }
}
