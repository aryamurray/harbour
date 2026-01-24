//! Shim file parsing for registry packages.
//!
//! A shim is a lightweight TOML file that references actual package sources.
//! It contains metadata about where to fetch the source code (git or tarball),
//! optional patches to apply, and surface overrides for bootstrap packages.

use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::util::hash::sha256_file;

/// A parsed shim file from a registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Shim {
    /// Package metadata
    pub package: ShimPackage,

    /// Source specification (git or tarball)
    pub source: ShimSource,

    /// Patches to apply in order
    #[serde(default)]
    pub patches: Vec<ShimPatch>,

    /// Surface overrides for bootstrap packages without Harbor.toml
    #[serde(default)]
    pub surface: Option<ShimSurface>,
}

/// Package metadata in a shim file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShimPackage {
    /// Package name (must be lowercase [a-z0-9_-])
    pub name: String,

    /// Exact version
    pub version: String,
}

/// Source specification - where to fetch the actual code.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub struct ShimSource {
    /// Git source
    #[serde(default)]
    pub git: Option<GitSource>,

    /// Tarball source
    #[serde(default)]
    pub tarball: Option<TarballSource>,
}

/// Git source specification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitSource {
    /// Git repository URL
    pub url: String,

    /// Full commit SHA (40 hex chars) - tags/branches not allowed
    pub rev: String,
}

/// Tarball source specification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TarballSource {
    /// Download URL
    pub url: String,

    /// SHA256 hash of the tarball
    pub sha256: String,

    /// Directory prefix to strip from tarball (e.g., "zlib-1.3.1")
    #[serde(default)]
    pub strip_prefix: Option<String>,
}

/// A patch to apply to the source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShimPatch {
    /// Path to patch file, relative to shim's directory
    pub file: String,

    /// SHA256 hash of the patch file bytes
    pub sha256: String,
}

/// Surface overrides for bootstrap packages.
///
/// Use sparingly - only for packages that predate Harbour.
/// If both shim and source have surfaces, a warning is logged.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ShimSurface {
    /// Compile-time surface
    #[serde(default)]
    pub compile: Option<ShimCompileSurface>,
}

/// Compile-time surface overrides.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ShimCompileSurface {
    /// Public defines to export to dependents
    #[serde(default)]
    pub public: Option<ShimPublicSurface>,
}

/// Public surface exposed to dependents.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ShimPublicSurface {
    /// Preprocessor defines
    #[serde(default)]
    pub defines: Vec<String>,

    /// Include directories (relative to source root)
    #[serde(default)]
    pub include_dirs: Vec<String>,
}

impl Shim {
    /// Load and parse a shim file from the given path.
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read shim file: {}", path.display()))?;

        Self::parse(&content, path)
    }

    /// Parse a shim from TOML content.
    pub fn parse(content: &str, path: &Path) -> Result<Self> {
        let shim: Shim = toml::from_str(content)
            .with_context(|| format!("failed to parse shim file: {}", path.display()))?;

        shim.validate()?;
        Ok(shim)
    }

    /// Validate the shim file contents.
    pub fn validate(&self) -> Result<()> {
        // Validate package name
        validate_package_name(&self.package.name)?;

        // Validate version is valid semver
        semver::Version::parse(&self.package.version)
            .with_context(|| format!("invalid version '{}' in shim", self.package.version))?;

        // Validate source - must have exactly one of git or tarball
        match (&self.source.git, &self.source.tarball) {
            (Some(_), Some(_)) => {
                bail!("shim cannot specify both git and tarball sources");
            }
            (None, None) => {
                bail!("shim must specify either git or tarball source");
            }
            (Some(git), None) => {
                // Validate git rev is a full SHA (40 hex chars)
                if git.rev.len() != 40 || !git.rev.chars().all(|c| c.is_ascii_hexdigit()) {
                    bail!(
                        "git rev must be a full 40-character SHA, got '{}'",
                        git.rev
                    );
                }

                // Validate URL is parseable
                url::Url::parse(&git.url).with_context(|| {
                    format!("invalid git URL '{}' in shim", git.url)
                })?;
            }
            (None, Some(tarball)) => {
                // Validate SHA256 hash format (64 hex chars)
                if tarball.sha256.len() != 64
                    || !tarball.sha256.chars().all(|c| c.is_ascii_hexdigit())
                {
                    bail!(
                        "tarball sha256 must be a 64-character hex string, got '{}'",
                        tarball.sha256
                    );
                }

                // Validate URL is parseable
                url::Url::parse(&tarball.url).with_context(|| {
                    format!("invalid tarball URL '{}' in shim", tarball.url)
                })?;
            }
        }

        // Validate patch SHA256 hashes
        for patch in &self.patches {
            if patch.sha256.len() != 64 || !patch.sha256.chars().all(|c| c.is_ascii_hexdigit()) {
                bail!(
                    "patch sha256 must be a 64-character hex string for '{}', got '{}'",
                    patch.file,
                    patch.sha256
                );
            }
        }

        Ok(())
    }

    /// Check if this shim uses a git source.
    pub fn is_git(&self) -> bool {
        self.source.git.is_some()
    }

    /// Check if this shim uses a tarball source.
    pub fn is_tarball(&self) -> bool {
        self.source.tarball.is_some()
    }

    /// Get the git source, if present.
    pub fn git_source(&self) -> Option<&GitSource> {
        self.source.git.as_ref()
    }

    /// Get the tarball source, if present.
    pub fn tarball_source(&self) -> Option<&TarballSource> {
        self.source.tarball.as_ref()
    }

    /// Compute a stable hash for caching purposes.
    ///
    /// This creates a hash from the source URL and revision/sha256,
    /// used to create unique cache directories.
    pub fn source_hash(&self) -> String {
        use crate::util::hash::sha256_str;

        let input = if let Some(git) = &self.source.git {
            format!("git:{}:{}", git.url, git.rev)
        } else if let Some(tarball) = &self.source.tarball {
            format!("tarball:{}:{}", tarball.url, tarball.sha256)
        } else {
            unreachable!("validated shim must have source")
        };

        // Return first 16 chars of hash
        sha256_str(&input)[..16].to_string()
    }
}

/// Validate a package name according to Harbour rules.
///
/// Package names must be:
/// - Lowercase only
/// - Characters: [a-z0-9_-]
/// - Non-empty
/// - First character must be [a-z]
pub fn validate_package_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("package name cannot be empty");
    }

    let first_char = name.chars().next().unwrap();
    if !first_char.is_ascii_lowercase() {
        bail!(
            "invalid package name '{}': must start with lowercase letter [a-z]",
            name
        );
    }

    for c in name.chars() {
        if !matches!(c, 'a'..='z' | '0'..='9' | '_' | '-') {
            bail!(
                "invalid package name '{}': only [a-z0-9_-] allowed, found '{}'",
                name,
                c
            );
        }
    }

    Ok(())
}

/// Compute the shim path for a package name and version.
///
/// Algorithm:
/// - letter = first ASCII lowercase char of name
/// - path = <letter>/<name>/<version>.toml
///
/// Examples:
/// - zlib 1.3.1 -> z/zlib/1.3.1.toml
/// - sqlite 3.45.0 -> s/sqlite/3.45.0.toml
pub fn shim_path(name: &str, version: &str) -> Result<String> {
    validate_package_name(name)?;

    let first_char = name.chars().next().unwrap();
    Ok(format!("{}/{}/{}.toml", first_char, name, version))
}

/// Verify a patch file's hash matches the expected value.
pub fn verify_patch_hash(patch_path: &Path, expected_hash: &str) -> Result<()> {
    let actual_hash = sha256_file(patch_path)?;

    if actual_hash != expected_hash {
        bail!(
            "patch file hash mismatch for '{}': expected {}, got {}",
            patch_path.display(),
            expected_hash,
            actual_hash
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_validate_package_name() {
        // Valid names
        assert!(validate_package_name("zlib").is_ok());
        assert!(validate_package_name("sqlite3").is_ok());
        assert!(validate_package_name("my-lib").is_ok());
        assert!(validate_package_name("my_lib").is_ok());
        assert!(validate_package_name("lib123").is_ok());

        // Invalid names
        assert!(validate_package_name("").is_err());
        assert!(validate_package_name("Zlib").is_err()); // uppercase
        assert!(validate_package_name("123lib").is_err()); // starts with number
        assert!(validate_package_name("-lib").is_err()); // starts with dash
        assert!(validate_package_name("my.lib").is_err()); // contains dot
        assert!(validate_package_name("my lib").is_err()); // contains space
    }

    #[test]
    fn test_shim_path() {
        assert_eq!(shim_path("zlib", "1.3.1").unwrap(), "z/zlib/1.3.1.toml");
        assert_eq!(
            shim_path("sqlite", "3.45.0").unwrap(),
            "s/sqlite/3.45.0.toml"
        );
        assert_eq!(
            shim_path("my-lib", "0.1.0").unwrap(),
            "m/my-lib/0.1.0.toml"
        );
    }

    #[test]
    fn test_parse_git_shim() {
        let content = r#"
[package]
name = "zlib"
version = "1.3.1"

[source.git]
url = "https://github.com/madler/zlib"
rev = "04f42ceca40f73e2978b50e93806c2a18c1281fc"
"#;

        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("shim.toml");

        let shim = Shim::parse(content, &path).unwrap();
        assert_eq!(shim.package.name, "zlib");
        assert_eq!(shim.package.version, "1.3.1");
        assert!(shim.is_git());
        assert!(!shim.is_tarball());

        let git = shim.git_source().unwrap();
        assert_eq!(git.url, "https://github.com/madler/zlib");
        assert_eq!(git.rev, "04f42ceca40f73e2978b50e93806c2a18c1281fc");
    }

    #[test]
    fn test_parse_tarball_shim() {
        let content = r#"
[package]
name = "zlib"
version = "1.3.1"

[source.tarball]
url = "https://example.com/zlib-1.3.1.tar.gz"
sha256 = "9a93b2b7dfdac77ceba5a558a580e74667dd6fede4585b91eefb60f03b72df23"
strip_prefix = "zlib-1.3.1"
"#;

        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("shim.toml");

        let shim = Shim::parse(content, &path).unwrap();
        assert!(!shim.is_git());
        assert!(shim.is_tarball());

        let tarball = shim.tarball_source().unwrap();
        assert_eq!(tarball.strip_prefix, Some("zlib-1.3.1".to_string()));
    }

    #[test]
    fn test_parse_shim_with_patches() {
        let content = r#"
[package]
name = "zlib"
version = "1.3.1"

[source.git]
url = "https://github.com/madler/zlib"
rev = "04f42ceca40f73e2978b50e93806c2a18c1281fc"

[[patches]]
file = "patches/fix-cmake.patch"
sha256 = "abc123def456789012345678901234567890123456789012345678901234abcd"
"#;

        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("shim.toml");

        let shim = Shim::parse(content, &path).unwrap();
        assert_eq!(shim.patches.len(), 1);
        assert_eq!(shim.patches[0].file, "patches/fix-cmake.patch");
    }

    #[test]
    fn test_parse_shim_with_surface() {
        let content = r#"
[package]
name = "zlib"
version = "1.3.1"

[source.git]
url = "https://github.com/madler/zlib"
rev = "04f42ceca40f73e2978b50e93806c2a18c1281fc"

[surface.compile.public]
defines = ["ZLIB_CONST"]
include_dirs = ["include"]
"#;

        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("shim.toml");

        let shim = Shim::parse(content, &path).unwrap();
        let surface = shim.surface.as_ref().unwrap();
        let compile = surface.compile.as_ref().unwrap();
        let public = compile.public.as_ref().unwrap();
        assert_eq!(public.defines, vec!["ZLIB_CONST"]);
        assert_eq!(public.include_dirs, vec!["include"]);
    }

    #[test]
    fn test_invalid_rev_length() {
        let content = r#"
[package]
name = "zlib"
version = "1.3.1"

[source.git]
url = "https://github.com/madler/zlib"
rev = "abc123"
"#;

        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("shim.toml");

        let result = Shim::parse(content, &path);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("40-character SHA"));
    }

    #[test]
    fn test_source_hash() {
        let content = r#"
[package]
name = "zlib"
version = "1.3.1"

[source.git]
url = "https://github.com/madler/zlib"
rev = "04f42ceca40f73e2978b50e93806c2a18c1281fc"
"#;

        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("shim.toml");

        let shim = Shim::parse(content, &path).unwrap();
        let hash = shim.source_hash();

        // Hash should be 16 hex chars
        assert_eq!(hash.len(), 16);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));

        // Same content should produce same hash
        let shim2 = Shim::parse(content, &path).unwrap();
        assert_eq!(shim.source_hash(), shim2.source_hash());
    }
}
