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
}

impl Language {
    /// Get the language name as a string.
    pub fn as_str(&self) -> &'static str {
        match self {
            Language::C => "c",
            Language::Cxx => "c++",
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
            "invalid C standard '{}', valid values: 89, 99, 11, 17, 23",
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
