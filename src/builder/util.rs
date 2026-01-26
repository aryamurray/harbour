//! Shared utilities for the builder module.

use std::process::Command;

use anyhow::{bail, Context, Result};

/// Detect a tool's version by running it with --version and parsing the output.
///
/// # Arguments
/// * `tool` - The name of the tool to run (e.g., "cmake", "meson")
/// * `version_parser` - A function that extracts a semver::Version from the stdout
///
/// # Example
/// ```ignore
/// let version = detect_tool_version("cmake", |stdout| {
///     // Parse "cmake version 3.20.5"
///     for line in stdout.lines() {
///         if line.starts_with("cmake version ") {
///             let version_str = line.trim_start_matches("cmake version ").trim();
///             let clean_version = version_str.split('-').next().unwrap_or(version_str);
///             return clean_version.parse().ok();
///         }
///     }
///     None
/// })?;
/// ```
pub fn detect_tool_version<F>(tool: &str, version_parser: F) -> Result<semver::Version>
where
    F: FnOnce(&str) -> Option<semver::Version>,
{
    let output = Command::new(tool)
        .arg("--version")
        .output()
        .with_context(|| format!("failed to run {} --version", tool))?;

    if !output.status.success() {
        bail!("{} --version failed", tool);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    version_parser(&stdout)
        .ok_or_else(|| anyhow::anyhow!("could not parse {} version from output: {}", tool, stdout))
}

/// Parse a version string into semver::Version, handling incomplete versions.
///
/// Handles versions like "3.20.5", "1.3.0.dev1", or versions with only major.minor parts.
pub fn parse_version_flexible(version_str: &str) -> Option<semver::Version> {
    // Remove any suffix after the first non-version character
    let clean_version = version_str
        .trim()
        .split(|c: char| !c.is_ascii_digit() && c != '.')
        .next()
        .unwrap_or(version_str);

    // Try direct parse first
    if let Ok(v) = clean_version.parse() {
        return Some(v);
    }

    // Handle versions with less than 3 parts
    let parts: Vec<&str> = clean_version.split('.').collect();
    let major = parts.first().and_then(|s| s.parse().ok())?;
    let minor = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let patch = parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);

    Some(semver::Version::new(major, minor, patch))
}

/// Parse compiler define flags (both GCC-style `-D` and MSVC-style `/D`).
///
/// Returns a vector of (name, value) pairs. The value is `None` for defines
/// without an explicit value (e.g., `-DFOO`), and `Some(value)` for defines
/// with a value (e.g., `-DFOO=bar`).
///
/// Flags that don't start with `-D` or `/D` are silently ignored.
pub fn parse_define_flags(defines: &[String]) -> Vec<(String, Option<String>)> {
    let mut parsed = Vec::new();

    for define in defines {
        let trimmed = define
            .strip_prefix("-D")
            .or_else(|| define.strip_prefix("/D"));

        let Some(rest) = trimmed else {
            continue;
        };

        if let Some((name, value)) = rest.split_once('=') {
            parsed.push((name.to_string(), Some(value.to_string())));
        } else if !rest.is_empty() {
            parsed.push((rest.to_string(), None));
        }
    }

    parsed
}

/// Extract library name from a file path.
///
/// Handles both Unix and Windows library naming conventions:
/// - Strips "lib" prefix (Unix): `libfoo.a` → `foo`
/// - Strips version suffixes: `libfoo.so.1.2.3` → `foo`
/// - Handles Windows names: `foo.lib` → `foo`
///
/// # Example
/// ```ignore
/// use std::path::Path;
/// assert_eq!(extract_lib_name(Path::new("libz.a")), Some("z".to_string()));
/// assert_eq!(extract_lib_name(Path::new("zlib.lib")), Some("zlib".to_string()));
/// ```
pub fn extract_lib_name(path: &std::path::Path) -> Option<String> {
    let stem = path.file_stem()?.to_string_lossy();

    // Remove lib prefix if present
    let name = stem
        .strip_prefix("lib")
        .map(|s| s.to_string())
        .unwrap_or_else(|| stem.to_string());

    // Remove version suffixes (e.g., libfoo.so.1.2.3 -> foo)
    let name = name.split('.').next().unwrap_or(&name).to_string();

    Some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_define_flags_basic() {
        let defines = vec![
            "-DFOO".to_string(),
            "-DBAR=123".to_string(),
            "-DBAZ=hello".to_string(),
        ];

        let parsed = parse_define_flags(&defines);

        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0], ("FOO".to_string(), None));
        assert_eq!(parsed[1], ("BAR".to_string(), Some("123".to_string())));
        assert_eq!(parsed[2], ("BAZ".to_string(), Some("hello".to_string())));
    }

    #[test]
    fn test_parse_define_flags_msvc_style() {
        let defines = vec![
            "/DWIN32".to_string(),
            "/D_UNICODE".to_string(),
            "/DVERSION=2.0".to_string(),
        ];

        let parsed = parse_define_flags(&defines);

        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0], ("WIN32".to_string(), None));
        assert_eq!(parsed[1], ("_UNICODE".to_string(), None));
        assert_eq!(parsed[2], ("VERSION".to_string(), Some("2.0".to_string())));
    }

    #[test]
    fn test_parse_define_flags_mixed() {
        let defines = vec![
            "-DUNIX".to_string(),
            "/DWINDOWS".to_string(),
            "NOT_A_DEFINE".to_string(), // Should be skipped
        ];

        let parsed = parse_define_flags(&defines);

        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0], ("UNIX".to_string(), None));
        assert_eq!(parsed[1], ("WINDOWS".to_string(), None));
    }

    #[test]
    fn test_parse_define_flags_ignores_invalid() {
        let defines = vec![
            "-DVALID".to_string(),
            "INVALID_NO_PREFIX".to_string(),
            "-D".to_string(), // Empty define after prefix
            "/D".to_string(), // Empty define after prefix
            "-DALSO_VALID".to_string(),
        ];

        let parsed = parse_define_flags(&defines);

        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0], ("VALID".to_string(), None));
        assert_eq!(parsed[1], ("ALSO_VALID".to_string(), None));
    }

    #[test]
    fn test_parse_define_flags_empty() {
        let defines: Vec<String> = vec![];
        let parsed = parse_define_flags(&defines);
        assert!(parsed.is_empty());
    }

    #[test]
    fn test_extract_lib_name_unix_static() {
        use std::path::Path;
        assert_eq!(extract_lib_name(Path::new("libz.a")), Some("z".to_string()));
        assert_eq!(extract_lib_name(Path::new("libpng.a")), Some("png".to_string()));
    }

    #[test]
    fn test_extract_lib_name_unix_shared() {
        use std::path::Path;
        assert_eq!(extract_lib_name(Path::new("libfoo.so")), Some("foo".to_string()));
        assert_eq!(extract_lib_name(Path::new("libbar.so.1")), Some("bar".to_string()));
        assert_eq!(extract_lib_name(Path::new("libz.so.1.2.3")), Some("z".to_string()));
    }

    #[test]
    fn test_extract_lib_name_windows() {
        use std::path::Path;
        assert_eq!(extract_lib_name(Path::new("zlib.lib")), Some("zlib".to_string()));
        assert_eq!(extract_lib_name(Path::new("foo.dll")), Some("foo".to_string()));
    }

    #[test]
    fn test_extract_lib_name_no_prefix() {
        use std::path::Path;
        assert_eq!(extract_lib_name(Path::new("sqlite3.a")), Some("sqlite3".to_string()));
    }
}
