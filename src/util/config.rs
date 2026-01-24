//! Configuration file support for Harbour.
//!
//! Harbour supports two configuration file locations:
//! - Global: `~/.harbour/config.toml` - User-wide defaults
//! - Project: `.harbour/config.toml` - Project-specific overrides
//!
//! Project config takes precedence over global config.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::builder::shim::{BackendId, LinkagePreference};

/// Harbour configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Build settings
    pub build: BuildConfig,

    /// FFI settings
    pub ffi: FfiConfig,

    /// Network settings
    pub net: NetConfig,
}

/// Build-related configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BuildConfig {
    /// Default build backend (native, cmake, meson, custom)
    pub backend: Option<String>,

    /// Default library linkage preference (static, shared, auto)
    pub linkage: Option<String>,

    /// Default number of parallel jobs (None = auto-detect)
    pub jobs: Option<usize>,

    /// Always emit compile_commands.json
    #[serde(default)]
    pub emit_compile_commands: bool,

    /// Default C++ standard version
    pub cpp_std: Option<String>,
}

/// FFI-related configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FfiConfig {
    /// Default bundle output directory
    pub bundle_dir: Option<String>,

    /// Include transitive dependencies by default
    #[serde(default = "default_true")]
    pub include_transitive: bool,

    /// Rewrite RPATH by default
    #[serde(default = "default_true")]
    pub rpath_rewrite: bool,
}

impl Default for FfiConfig {
    fn default() -> Self {
        FfiConfig {
            bundle_dir: None,
            include_transitive: true,
            rpath_rewrite: true,
        }
    }
}

/// Network-related configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NetConfig {
    /// Git fetch timeout in seconds
    pub git_timeout: Option<u64>,

    /// Offline mode (don't fetch from network)
    #[serde(default)]
    pub offline: bool,
}

fn default_true() -> bool {
    true
}

impl Config {
    /// Load configuration from a file.
    pub fn load(path: &Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read config file: {}", path.display()))?;

        toml::from_str(&contents)
            .with_context(|| format!("failed to parse config file: {}", path.display()))
    }

    /// Load configuration with fallback to defaults if file doesn't exist.
    pub fn load_or_default(path: &Path) -> Self {
        if path.exists() {
            Self::load(path).unwrap_or_else(|e| {
                tracing::warn!("Failed to load config from {}: {}", path.display(), e);
                Self::default()
            })
        } else {
            Self::default()
        }
    }

    /// Merge another config into this one (other takes precedence).
    pub fn merge(&mut self, other: Config) {
        // Build settings
        if other.build.backend.is_some() {
            self.build.backend = other.build.backend;
        }
        if other.build.linkage.is_some() {
            self.build.linkage = other.build.linkage;
        }
        if other.build.jobs.is_some() {
            self.build.jobs = other.build.jobs;
        }
        if other.build.emit_compile_commands {
            self.build.emit_compile_commands = true;
        }
        if other.build.cpp_std.is_some() {
            self.build.cpp_std = other.build.cpp_std;
        }

        // FFI settings
        if other.ffi.bundle_dir.is_some() {
            self.ffi.bundle_dir = other.ffi.bundle_dir;
        }
        // Note: these default to true, so we don't merge them

        // Net settings
        if other.net.git_timeout.is_some() {
            self.net.git_timeout = other.net.git_timeout;
        }
        if other.net.offline {
            self.net.offline = true;
        }
    }

    /// Parse backend from config string.
    pub fn backend(&self) -> Option<BackendId> {
        self.build.backend.as_ref().and_then(|s| s.parse().ok())
    }

    /// Parse linkage from config string.
    pub fn linkage(&self) -> Option<LinkagePreference> {
        self.build.linkage.as_ref().and_then(|s| s.parse().ok())
    }
}

/// Load merged configuration from global and project locations.
///
/// Order of precedence (highest to lowest):
/// 1. Project config (.harbour/config.toml)
/// 2. Global config (~/.harbour/config.toml)
/// 3. Defaults
pub fn load_config(global_path: &Path, project_path: &Path) -> Config {
    let mut config = Config::default();

    // Load global config first
    if global_path.exists() {
        let global = Config::load_or_default(global_path);
        config.merge(global);
    }

    // Project config overrides global
    if project_path.exists() {
        let project = Config::load_or_default(project_path);
        config.merge(project);
    }

    config
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_config_default() {
        let config = Config::default();
        assert!(config.build.backend.is_none());
        assert!(config.build.linkage.is_none());
        assert!(config.ffi.include_transitive);
        assert!(config.ffi.rpath_rewrite);
    }

    #[test]
    fn test_config_load() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");

        std::fs::write(
            &config_path,
            r#"
[build]
backend = "cmake"
linkage = "shared"
jobs = 8

[ffi]
bundle_dir = "./dist"
"#,
        )
        .unwrap();

        let config = Config::load(&config_path).unwrap();
        assert_eq!(config.build.backend, Some("cmake".to_string()));
        assert_eq!(config.build.linkage, Some("shared".to_string()));
        assert_eq!(config.build.jobs, Some(8));
        assert_eq!(config.ffi.bundle_dir, Some("./dist".to_string()));
    }

    #[test]
    fn test_config_merge() {
        let mut base = Config::default();
        base.build.backend = Some("native".to_string());
        base.build.jobs = Some(4);

        let mut override_cfg = Config::default();
        override_cfg.build.backend = Some("cmake".to_string());

        base.merge(override_cfg);

        assert_eq!(base.build.backend, Some("cmake".to_string()));
        assert_eq!(base.build.jobs, Some(4)); // Not overridden
    }

    #[test]
    fn test_config_parse_backend() {
        let mut config = Config::default();
        config.build.backend = Some("cmake".to_string());

        assert_eq!(config.backend(), Some(BackendId::CMake));
    }

    #[test]
    fn test_config_parse_linkage() {
        let mut config = Config::default();
        config.build.linkage = Some("shared".to_string());

        assert!(matches!(config.linkage(), Some(LinkagePreference::Shared)));
    }
}
