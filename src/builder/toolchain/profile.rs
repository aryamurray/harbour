//! Profile *intent*, as opposed to profile *spelling*.
//!
//! `[profile]` says what kind of build the author wants -- how optimised, how
//! much debug information, which sanitizers, LTO or not. Every one of those is
//! spelled differently by GCC/clang and by MSVC, and two of them are not even
//! the same *shape*: MSVC has no counterpart for `-O3` or `-Ofast`, and it
//! implements only one of the five sanitizers.
//!
//! Nothing in this module knows a single compiler flag. The manifest's strings
//! are parsed here, once, into the types below; each toolchain backend spells
//! them ([`Toolchain::profile_compile_flags`] and
//! [`Toolchain::profile_link_flags`]). That is the same division as
//! [`CxxOptions`], and for the same reason: `-std=`/`/std:`,
//! `-fno-exceptions`/`/EHs-c-` and `-fno-rtti`/`/GR-` are decided once from
//! the resolved constraints and spelled in exactly one place per toolchain.
//!
//! The alternative -- emitting GCC syntax centrally and rewriting it to MSVC
//! syntax somewhere downstream -- is what this repo keeps getting bitten by: a
//! second place that has to know the mapping is a second place that can drift
//! from the first. Here there is no GCC string for anyone to rewrite.
//!
//! [`Toolchain::profile_compile_flags`]: super::Toolchain::profile_compile_flags
//! [`Toolchain::profile_link_flags`]: super::Toolchain::profile_link_flags
//! [`CxxOptions`]: super::CxxOptions

use anyhow::{bail, Result};

use crate::core::manifest::Profile;

/// How hard the author asked the optimiser to work.
///
/// The variants are GCC's set, because that is the set `[profile] opt_level`
/// documents (`0 1 2 3 s z`, plus `g` and `fast`, which GCC and clang accept).
/// MSVC's set is smaller and the mapping is lossy in places; see
/// [`MsvcToolchain::profile_compile_flags`] for what each becomes and which
/// two have no counterpart at all.
///
/// [`MsvcToolchain::profile_compile_flags`]: super::MsvcToolchain::profile_compile_flags
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptLevel {
    /// `opt_level = "0"` -- no optimisation.
    None,
    /// `opt_level = "1"`
    Basic,
    /// `opt_level = "2"`
    Standard,
    /// `opt_level = "3"` -- everything the compiler has.
    Aggressive,
    /// `opt_level = "s"` -- optimise for size.
    Size,
    /// `opt_level = "z"` -- optimise for size, harder.
    SizeAggressive,
    /// `opt_level = "g"` -- optimise, but keep the result debuggable.
    Debug,
    /// `opt_level = "fast"` -- `-O3` plus standards-violating maths.
    Fast,
}

impl OptLevel {
    /// Parse the manifest's `opt_level` string.
    fn parse(value: &str) -> Result<Self> {
        Ok(match value {
            "0" => OptLevel::None,
            "1" => OptLevel::Basic,
            "2" => OptLevel::Standard,
            "3" => OptLevel::Aggressive,
            "s" => OptLevel::Size,
            "z" => OptLevel::SizeAggressive,
            "g" => OptLevel::Debug,
            "fast" => OptLevel::Fast,
            other => bail!(
                "unknown `[profile] opt_level` value `{other}`\n\
                 help: expected one of 0, 1, 2, 3, s, z, g, fast"
            ),
        })
    }

    /// The manifest spelling, for error messages.
    pub fn as_manifest_value(self) -> &'static str {
        match self {
            OptLevel::None => "0",
            OptLevel::Basic => "1",
            OptLevel::Standard => "2",
            OptLevel::Aggressive => "3",
            OptLevel::Size => "s",
            OptLevel::SizeAggressive => "z",
            OptLevel::Debug => "g",
            OptLevel::Fast => "fast",
        }
    }
}

/// How much debug information to produce.
///
/// Three levels rather than GCC's four, because that is what `[profile] debug`
/// documents. MSVC has no level gradation at all -- debug information is on or
/// off -- so [`Limited`] and [`Full`] are the same command line there.
///
/// [`Limited`]: DebugInfo::Limited
/// [`Full`]: DebugInfo::Full
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DebugInfo {
    /// `debug = "0"`, or no `debug` key -- none at all.
    #[default]
    None,
    /// `debug = "1"`
    Limited,
    /// `debug = "2"` or `debug = "full"`
    Full,
}

impl DebugInfo {
    fn parse(value: &str) -> Result<Self> {
        Ok(match value {
            "0" => DebugInfo::None,
            "1" => DebugInfo::Limited,
            "2" | "full" => DebugInfo::Full,
            other => bail!(
                "unknown `[profile] debug` value `{other}`\n\
                 help: expected one of 0, 1, 2, full"
            ),
        })
    }

    /// Whether any debug information at all was asked for.
    pub fn enabled(self) -> bool {
        self != DebugInfo::None
    }
}

/// A sanitizer the author asked for.
///
/// Closed, not a string: "which sanitizers exist" is a fact about compilers,
/// and a typo in a manifest should be an error rather than a flag the
/// compiler rejects three seconds later -- or, on MSVC before this existed,
/// a `D9002 ignoring unknown option` nobody read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sanitizer {
    Address,
    Thread,
    Memory,
    Undefined,
    Leak,
}

impl Sanitizer {
    fn parse(value: &str) -> Result<Self> {
        Ok(match value {
            "address" => Sanitizer::Address,
            "thread" => Sanitizer::Thread,
            "memory" => Sanitizer::Memory,
            "undefined" => Sanitizer::Undefined,
            "leak" => Sanitizer::Leak,
            other => bail!(
                "unknown `[profile] sanitizers` entry `{other}`\n\
                 help: expected one of address, thread, memory, undefined, leak"
            ),
        })
    }

    /// The name as written in the manifest, which is also the value GCC and
    /// clang take after `-fsanitize=`.
    pub fn as_str(self) -> &'static str {
        match self {
            Sanitizer::Address => "address",
            Sanitizer::Thread => "thread",
            Sanitizer::Memory => "memory",
            Sanitizer::Undefined => "undefined",
            Sanitizer::Leak => "leak",
        }
    }
}

/// A profile's compiler-affecting settings, parsed and validated.
///
/// The custom `cflags`/`ldflags` are deliberately *not* here: those are
/// verbatim strings for one particular compiler, written by someone who knows
/// which compiler they are aiming at. There is no intent to recover from them
/// and Harbour passes them through untouched.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileOptions {
    /// Optimisation level, or `None` when the profile does not set one (in
    /// which case Harbour passes no `-O`/`/O` at all and the compiler's own
    /// default applies).
    pub opt_level: Option<OptLevel>,
    /// Debug information level.
    pub debug: DebugInfo,
    /// Sanitizers to enable, in manifest order.
    pub sanitizers: Vec<Sanitizer>,
    /// Whether link-time optimisation was requested.
    pub lto: bool,
}

impl ProfileOptions {
    /// Parse a manifest profile.
    ///
    /// Fails on any value Harbour does not recognise. That is the whole point
    /// of doing it here: an unrecognised `opt_level` used to be pasted
    /// straight into `-O{}` and an unrecognised sanitizer into
    /// `-fsanitize={}`, so `opt_level = "fastest"` reached the compiler as
    /// `-Ofastest`.
    pub fn from_profile(profile: &Profile) -> Result<Self> {
        let opt_level = match profile.opt_level.as_deref() {
            Some(value) => Some(OptLevel::parse(value)?),
            None => None,
        };

        let debug = match profile.debug.as_deref() {
            Some(value) => DebugInfo::parse(value)?,
            None => DebugInfo::None,
        };

        let sanitizers = profile
            .sanitizers
            .iter()
            .map(|s| Sanitizer::parse(s))
            .collect::<Result<Vec<_>>>()?;

        Ok(ProfileOptions {
            opt_level,
            debug,
            sanitizers,
            lto: profile.lto == Some(true),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_documented_value() {
        let profile = Profile {
            opt_level: Some("s".to_string()),
            debug: Some("full".to_string()),
            lto: Some(true),
            sanitizers: vec!["address".to_string(), "undefined".to_string()],
            ..Default::default()
        };

        let opts = ProfileOptions::from_profile(&profile).unwrap();
        assert_eq!(opts.opt_level, Some(OptLevel::Size));
        assert_eq!(opts.debug, DebugInfo::Full);
        assert!(opts.lto);
        assert_eq!(
            opts.sanitizers,
            vec![Sanitizer::Address, Sanitizer::Undefined]
        );
    }

    #[test]
    fn an_absent_profile_asks_for_nothing() {
        let opts = ProfileOptions::from_profile(&Profile::default()).unwrap();
        assert_eq!(opts, ProfileOptions::default());
        assert!(!opts.debug.enabled());
    }

    /// `opt_level = "fastest"` used to be pasted into `-Ofastest`.
    #[test]
    fn an_unknown_opt_level_is_rejected_rather_than_pasted_into_a_flag() {
        let profile = Profile {
            opt_level: Some("fastest".to_string()),
            ..Default::default()
        };
        let err = ProfileOptions::from_profile(&profile)
            .unwrap_err()
            .to_string();
        assert!(err.contains("fastest"), "{err}");
        assert!(err.contains("0, 1, 2, 3, s, z, g, fast"), "{err}");
    }

    #[test]
    fn an_unknown_debug_level_is_rejected() {
        let profile = Profile {
            debug: Some("true".to_string()),
            ..Default::default()
        };
        let err = ProfileOptions::from_profile(&profile)
            .unwrap_err()
            .to_string();
        assert!(err.contains("`true`"), "{err}");
    }

    #[test]
    fn an_unknown_sanitizer_is_rejected() {
        let profile = Profile {
            sanitizers: vec!["address".to_string(), "adress".to_string()],
            ..Default::default()
        };
        let err = ProfileOptions::from_profile(&profile)
            .unwrap_err()
            .to_string();
        assert!(err.contains("adress"), "{err}");
    }
}
