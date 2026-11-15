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

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

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

    /// What applies when the package is built from `local` sources or not.
    ///
    /// A package on disk (the one being worked on, a workspace member, a
    /// `path` dependency) gets exactly what its manifest says. A package
    /// fetched from a registry, git or vcpkg is somebody else's code being
    /// built as a dependency: its author's `warnings = "all"` would print
    /// diagnostics the consumer cannot act on, and its `warnings_as_errors`
    /// would fail the consumer's build the day a newer compiler grows a
    /// warning. So a fetched package can only ask for *fewer* warnings:
    /// `none` is kept, everything else becomes the compiler's default.
    pub fn for_package(self, local: bool) -> Self {
        if local {
            return self;
        }
        WarningOptions {
            level: match self.level {
                WarningLevel::None => WarningLevel::None,
                _ => WarningLevel::Default,
            },
            as_errors: false,
        }
    }
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
    fn a_fetched_package_can_only_ask_for_fewer_warnings() {
        let strict = WarningOptions {
            level: WarningLevel::Pedantic,
            as_errors: true,
        };
        assert_eq!(strict.for_package(true), strict);
        assert_eq!(strict.for_package(false), WarningOptions::default());

        let quiet = WarningOptions {
            level: WarningLevel::None,
            as_errors: true,
        };
        assert_eq!(
            quiet.for_package(false),
            WarningOptions {
                level: WarningLevel::None,
                as_errors: false,
            }
        );
    }
}
