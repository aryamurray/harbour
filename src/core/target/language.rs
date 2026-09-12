//! Language standards and related types.
//!
//! This module contains the Language enum and C/C++ standard enums
//! with their parsing implementations.

use serde::{Deserialize, Serialize};

/// Source language for a target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    /// C language (default)
    #[default]
    C,
    /// C++ language
    #[serde(alias = "cpp", alias = "cxx", alias = "c++")]
    Cxx,
    /// Assembly (`.s`, `.S`, `.asm`)
    ///
    /// Not normally written as a target's `lang`: it is dispatched per
    /// source file by extension, so one target can mix assembly with C or
    /// C++ (which is how most crypto and codec libraries are laid out).
    #[serde(alias = "assembly", alias = "s")]
    Asm,
}

impl Language {
    /// Get the language name as a string.
    pub fn as_str(&self) -> &'static str {
        match self {
            Language::C => "c",
            Language::Cxx => "c++",
            Language::Asm => "asm",
        }
    }
}

/// C++ standard version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum CppStandard {
    /// C++11
    #[serde(rename = "11", alias = "c++11", alias = "cpp11")]
    Cpp11,
    /// C++14
    #[serde(rename = "14", alias = "c++14", alias = "cpp14")]
    Cpp14,
    /// C++17
    #[serde(rename = "17", alias = "c++17", alias = "cpp17")]
    Cpp17,
    /// C++20
    #[serde(rename = "20", alias = "c++20", alias = "cpp20")]
    Cpp20,
    /// C++23
    #[serde(rename = "23", alias = "c++23", alias = "cpp23")]
    Cpp23,
}

impl CppStandard {
    /// Get the standard as a compiler flag value (e.g., "c++17").
    pub fn as_flag_value(&self) -> &'static str {
        match self {
            CppStandard::Cpp11 => "c++11",
            CppStandard::Cpp14 => "c++14",
            CppStandard::Cpp17 => "c++17",
            CppStandard::Cpp20 => "c++20",
            CppStandard::Cpp23 => "c++23",
        }
    }

    /// Get the MSVC-style standard flag value (e.g., "c++17", "c++latest" for C++23).
    pub fn as_msvc_flag_value(&self) -> &'static str {
        match self {
            CppStandard::Cpp11 => "c++14", // MSVC doesn't support c++11 flag, use 14
            CppStandard::Cpp14 => "c++14",
            CppStandard::Cpp17 => "c++17",
            CppStandard::Cpp20 => "c++20",
            CppStandard::Cpp23 => "c++latest",
        }
    }
}

impl std::str::FromStr for CppStandard {
    type Err = CppStandardParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "11" | "c++11" | "cpp11" => Ok(CppStandard::Cpp11),
            "14" | "c++14" | "cpp14" => Ok(CppStandard::Cpp14),
            "17" | "c++17" | "cpp17" => Ok(CppStandard::Cpp17),
            "20" | "c++20" | "cpp20" => Ok(CppStandard::Cpp20),
            "23" | "c++23" | "cpp23" => Ok(CppStandard::Cpp23),
            _ => Err(CppStandardParseError(s.to_string())),
        }
    }
}

/// Error returned when parsing an invalid C++ standard string.
#[derive(Debug, Clone)]
pub struct CppStandardParseError(pub String);

impl std::fmt::Display for CppStandardParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "invalid C++ standard '{}', valid values: 11, 14, 17, 20, 23",
            self.0
        )
    }
}

impl std::error::Error for CppStandardParseError {}

impl std::fmt::Display for CppStandard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "C++{}",
            match self {
                CppStandard::Cpp11 => "11",
                CppStandard::Cpp14 => "14",
                CppStandard::Cpp17 => "17",
                CppStandard::Cpp20 => "20",
                CppStandard::Cpp23 => "23",
            }
        )
    }
}

/// C standard version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum CStandard {
    /// C89 (also known as C90, ANSI C)
    #[serde(
        rename = "89",
        alias = "c89",
        alias = "C89",
        alias = "90",
        alias = "c90",
        alias = "C90"
    )]
    C89,
    /// C99
    #[serde(rename = "99", alias = "c99", alias = "C99")]
    C99,
    /// C11
    #[serde(rename = "11", alias = "c11", alias = "C11")]
    C11,
    /// C17 (also known as C18)
    #[serde(
        rename = "17",
        alias = "c17",
        alias = "C17",
        alias = "18",
        alias = "c18",
        alias = "C18"
    )]
    C17,
    /// C23
    #[serde(rename = "23", alias = "c23", alias = "C23")]
    C23,
}

impl CStandard {
    /// Get the standard as a compiler flag value (e.g., "c11").
    pub fn as_flag_value(&self) -> &'static str {
        match self {
            CStandard::C89 => "c89",
            CStandard::C99 => "c99",
            CStandard::C11 => "c11",
            CStandard::C17 => "c17",
            CStandard::C23 => "c23",
        }
    }

    /// Get the GNU-extension variant (e.g., "gnu11").
    pub fn as_gnu_flag_value(&self) -> &'static str {
        match self {
            CStandard::C89 => "gnu89",
            CStandard::C99 => "gnu99",
            CStandard::C11 => "gnu11",
            CStandard::C17 => "gnu17",
            CStandard::C23 => "gnu23",
        }
    }
}

/// A C standard exactly as a manifest asked for it: an ISO standard plus
/// whether GNU extensions were requested.
///
/// The two axes are orthogonal, which is why this is a struct and not ten
/// enum variants. `gnu99` is not "a later C99": it is C99 plus the GNU
/// dialect, and real C packages depend on the difference -- `asm`,
/// `typeof`, statement expressions and `__attribute__` spellings are
/// available under `-std=gnu99` and not under `-std=c99`, and zlib, libuv
/// and the Linux kernel all rely on that. A manifest that writes
/// `c_std = "99"` for portability is asking for the *strict* dialect, and
/// silently giving it the GNU one would be the same class of lie as
/// ignoring the field altogether.
///
/// Unlike `cpp_std`, this is deliberately **per target** rather than folded
/// graph-wide by [`crate::resolver::CppConstraints`]: the C standard does
/// not change the C ABI, so two packages in one graph compiled at different
/// C standards still link. `exceptions`, `rtti` and the C++ standard do
/// change the C++ ABI, which is why those are graph-wide and this is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CStandardSpec {
    /// The ISO standard requested.
    pub standard: CStandard,
    /// Whether GNU extensions were requested (`gnu11` rather than `11`).
    pub gnu: bool,
}

impl CStandardSpec {
    /// A strict-ISO spec for `standard`.
    pub fn iso(standard: CStandard) -> Self {
        CStandardSpec {
            standard,
            gnu: false,
        }
    }

    /// A GNU-dialect spec for `standard`.
    pub fn gnu(standard: CStandard) -> Self {
        CStandardSpec {
            standard,
            gnu: true,
        }
    }

    /// The value for a GCC/Clang `-std=` flag (e.g. `"c11"`, `"gnu11"`).
    pub fn as_flag_value(&self) -> &'static str {
        if self.gnu {
            self.standard.as_gnu_flag_value()
        } else {
            self.standard.as_flag_value()
        }
    }

    /// The value for an MSVC `/std:` flag, when `cl` has one.
    ///
    /// `cl` gained `/std:c11` and `/std:c17` in Visual Studio 2019 16.8 and
    /// has nothing for C89, C99 or C23, and no GNU dialect at all. Returning
    /// `None` is the honest answer for those; emitting a GCC-shaped
    /// `-std=c99` that `cl` would treat as an unknown option (or, worse,
    /// silently emitting nothing) is not. The build warns when this is
    /// `None` or when GNU extensions were asked for -- see
    /// `BuildPlan::warn_unsupported_c_std`.
    pub fn as_msvc_flag_value(&self) -> Option<&'static str> {
        match self.standard {
            CStandard::C11 => Some("c11"),
            CStandard::C17 => Some("c17"),
            CStandard::C89 | CStandard::C99 | CStandard::C23 => None,
        }
    }
}

impl std::fmt::Display for CStandardSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_flag_value())
    }
}

impl std::str::FromStr for CStandardSpec {
    type Err = CStandardParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Accept the GNU dialect under the same spellings as the ISO one:
        // `gnu99`, `gnu-99`, `GNU11`.
        let lower = s.to_ascii_lowercase();
        let rest = lower
            .strip_prefix("gnu")
            .map(|r| r.strip_prefix('-').unwrap_or(r));
        match rest {
            Some(rest) => Ok(CStandardSpec::gnu(
                rest.parse::<CStandard>()
                    .map_err(|_| CStandardParseError(s.to_string()))?,
            )),
            None => Ok(CStandardSpec::iso(s.parse()?)),
        }
    }
}

impl Serialize for CStandardSpec {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_flag_value())
    }
}

impl<'de> Deserialize<'de> for CStandardSpec {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

impl std::str::FromStr for CStandard {
    type Err = CStandardParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "89" | "c89" | "C89" | "90" | "c90" | "C90" => Ok(CStandard::C89),
            "99" | "c99" | "C99" => Ok(CStandard::C99),
            "11" | "c11" | "C11" => Ok(CStandard::C11),
            "17" | "c17" | "C17" | "18" | "c18" | "C18" => Ok(CStandard::C17),
            "23" | "c23" | "C23" => Ok(CStandard::C23),
            _ => Err(CStandardParseError(s.to_string())),
        }
    }
}

/// Error returned when parsing an invalid C standard string.
#[derive(Debug, Clone)]
pub struct CStandardParseError(pub String);

impl std::fmt::Display for CStandardParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "invalid C standard '{}', valid values: 89, 99, 11, 17, 23, and the \
             GNU-dialect forms gnu89, gnu99, gnu11, gnu17, gnu23",
            self.0
        )
    }
}

impl std::error::Error for CStandardParseError {}

impl std::fmt::Display for CStandard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "C{}",
            match self {
                CStandard::C89 => "89",
                CStandard::C99 => "99",
                CStandard::C11 => "11",
                CStandard::C17 => "17",
                CStandard::C23 => "23",
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ISO spellings, all of the aliases, and the GNU forms.
    #[test]
    fn c_standard_spec_parses_iso_and_gnu_spellings() {
        for (input, expected) in [
            ("89", CStandardSpec::iso(CStandard::C89)),
            ("90", CStandardSpec::iso(CStandard::C89)),
            ("c89", CStandardSpec::iso(CStandard::C89)),
            ("99", CStandardSpec::iso(CStandard::C99)),
            ("C99", CStandardSpec::iso(CStandard::C99)),
            ("11", CStandardSpec::iso(CStandard::C11)),
            ("18", CStandardSpec::iso(CStandard::C17)),
            ("23", CStandardSpec::iso(CStandard::C23)),
            ("gnu89", CStandardSpec::gnu(CStandard::C89)),
            ("gnu90", CStandardSpec::gnu(CStandard::C89)),
            ("gnu99", CStandardSpec::gnu(CStandard::C99)),
            ("GNU99", CStandardSpec::gnu(CStandard::C99)),
            ("gnu-11", CStandardSpec::gnu(CStandard::C11)),
            ("gnu23", CStandardSpec::gnu(CStandard::C23)),
        ] {
            assert_eq!(
                input.parse::<CStandardSpec>().unwrap(),
                expected,
                "parsing `{input}`"
            );
        }

        for input in ["", "gnu", "c", "1", "20", "gnu20", "c++11", "gnuu99"] {
            assert!(
                input.parse::<CStandardSpec>().is_err(),
                "`{input}` is not a C standard and must not parse"
            );
        }
    }

    /// `gnu99` and `99` must not collapse into the same thing. They select
    /// different dialects: `-std=c99` defines `__STRICT_ANSI__` and
    /// `-std=gnu99` does not, which is what decides whether `typeof` and
    /// statement expressions compile.
    #[test]
    fn the_gnu_dialect_is_not_a_spelling_of_the_iso_one() {
        let iso = "99".parse::<CStandardSpec>().unwrap();
        let gnu = "gnu99".parse::<CStandardSpec>().unwrap();

        assert_ne!(iso, gnu);
        assert_eq!(iso.standard, gnu.standard);
        assert_eq!(iso.as_flag_value(), "c99");
        assert_eq!(gnu.as_flag_value(), "gnu99");
    }

    /// A manifest value survives a round trip through the build plan, which
    /// is serialized to disk.
    #[test]
    fn c_standard_spec_round_trips_through_serde() {
        for input in ["c89", "99", "gnu11", "gnu23"] {
            let spec: CStandardSpec = input.parse().unwrap();
            let json = serde_json::to_string(&spec).unwrap();
            let back: CStandardSpec = serde_json::from_str(&json).unwrap();
            assert_eq!(spec, back, "round trip of `{input}` via {json}");
        }
    }

    /// `cl` has `/std:c11` and `/std:c17` and nothing else; there is no
    /// `/std:c89`, no `/std:c99`, and no GNU dialect. `None` is the honest
    /// answer for the rest -- the alternative that was almost shipped is
    /// handing `cl` a GCC-shaped `-std=c99` it does not understand.
    #[test]
    fn msvc_has_a_flag_for_c11_and_c17_only() {
        assert_eq!(
            CStandardSpec::iso(CStandard::C11).as_msvc_flag_value(),
            Some("c11")
        );
        assert_eq!(
            CStandardSpec::iso(CStandard::C17).as_msvc_flag_value(),
            Some("c17")
        );
        // The GNU dialect has no MSVC equivalent, so it degrades to the ISO
        // standard of the same version -- and `BuildPlan` warns that the
        // extensions are not available.
        assert_eq!(
            CStandardSpec::gnu(CStandard::C11).as_msvc_flag_value(),
            Some("c11")
        );
        for unsupported in [CStandard::C89, CStandard::C99, CStandard::C23] {
            assert_eq!(
                CStandardSpec::iso(unsupported).as_msvc_flag_value(),
                None,
                "`cl` has no /std: option for {unsupported}"
            );
        }
    }

    /// The error a manifest author sees has to name the GNU forms, or they
    /// are undiscoverable.
    #[test]
    fn the_parse_error_lists_the_gnu_forms() {
        let err = "gnu20".parse::<CStandardSpec>().unwrap_err().to_string();
        assert!(err.contains("gnu99"), "{err}");
        assert!(err.contains("99"), "{err}");
    }
}
