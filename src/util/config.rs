//! Configuration file support for Harbour.
//!
//! Harbour supports two configuration file locations:
//! - Global: `~/.harbour/config.toml` - User-wide defaults
//! - Project: `.harbour/config.toml` - Project-specific overrides
//!
//! Project config takes precedence over global config.
//!
//! Toolchain overrides are stored separately:
//! - Global: `~/.harbour/toolchain.toml`
//! - Project: `.harbour/toolchain.toml`

use std::collections::HashMap;
use std::path::{Path, PathBuf};

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

    /// Vcpkg integration settings
    pub vcpkg: VcpkgConfig,
}

/// Toolchain configuration for compiler overrides.
///
/// This is stored in a separate file (`toolchain.toml`) from the main config
/// to allow easy toolchain switching without modifying other settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolchainConfig {
    /// Toolchain settings
    pub toolchain: ToolchainSettings,
}

/// Toolchain settings for C/C++ compilation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolchainSettings {
    /// Path to the C compiler (e.g., /usr/bin/clang)
    pub cc: Option<PathBuf>,

    /// Path to the C++ compiler (e.g., /usr/bin/clang++)
    pub cxx: Option<PathBuf>,

    /// Path to the archiver (e.g., /usr/bin/llvm-ar)
    pub ar: Option<PathBuf>,

    /// Target triple for cross-compilation (e.g., x86_64-unknown-linux-gnu)
    pub target: Option<String>,

    /// Additional C compiler flags
    #[serde(default)]
    pub cflags: Vec<String>,

    /// Additional C++ compiler flags
    #[serde(default)]
    pub cxxflags: Vec<String>,

    /// Additional linker flags
    #[serde(default)]
    pub ldflags: Vec<String>,
}

impl ToolchainConfig {
    /// Load toolchain configuration from a file.
    pub fn load(path: &Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read toolchain config: {}", path.display()))?;

        toml::from_str(&contents)
            .with_context(|| format!("failed to parse toolchain config: {}", path.display()))
    }

    /// Load toolchain configuration with fallback to defaults if file doesn't exist.
    pub fn load_or_default(path: &Path) -> Self {
        if path.exists() {
            Self::load(path).unwrap_or_else(|e| {
                tracing::warn!(
                    "Failed to load toolchain config from {}: {}",
                    path.display(),
                    e
                );
                Self::default()
            })
        } else {
            Self::default()
        }
    }

    /// Save toolchain configuration to a file.
    pub fn save(&self, path: &Path) -> Result<()> {
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create config directory: {}", parent.display())
            })?;
        }

        let contents =
            toml::to_string_pretty(self).with_context(|| "failed to serialize toolchain config")?;

        std::fs::write(path, contents)
            .with_context(|| format!("failed to write toolchain config: {}", path.display()))?;

        Ok(())
    }

    /// Check if any toolchain settings are configured.
    pub fn has_overrides(&self) -> bool {
        self.toolchain.cc.is_some()
            || self.toolchain.cxx.is_some()
            || self.toolchain.ar.is_some()
            || self.toolchain.target.is_some()
            || !self.toolchain.cflags.is_empty()
            || !self.toolchain.cxxflags.is_empty()
            || !self.toolchain.ldflags.is_empty()
    }

    /// Merge another config into this one (other takes precedence).
    pub fn merge(&mut self, other: ToolchainConfig) {
        if other.toolchain.cc.is_some() {
            self.toolchain.cc = other.toolchain.cc;
        }
        if other.toolchain.cxx.is_some() {
            self.toolchain.cxx = other.toolchain.cxx;
        }
        if other.toolchain.ar.is_some() {
            self.toolchain.ar = other.toolchain.ar;
        }
        if other.toolchain.target.is_some() {
            self.toolchain.target = other.toolchain.target;
        }
        if !other.toolchain.cflags.is_empty() {
            self.toolchain.cflags = other.toolchain.cflags;
        }
        if !other.toolchain.cxxflags.is_empty() {
            self.toolchain.cxxflags = other.toolchain.cxxflags;
        }
        if !other.toolchain.ldflags.is_empty() {
            self.toolchain.ldflags = other.toolchain.ldflags;
        }
    }
}

/// Load merged toolchain configuration from global and project locations.
///
/// Order of precedence (highest to lowest):
/// 1. Project config (.harbour/toolchain.toml)
/// 2. Global config (~/.harbour/toolchain.toml)
/// 3. Defaults
pub fn load_toolchain_config(global_path: &Path, project_path: &Path) -> ToolchainConfig {
    let mut config = ToolchainConfig::default();

    // Load global config first
    if global_path.exists() {
        let global = ToolchainConfig::load_or_default(global_path);
        config.merge(global);
    }

    // Project config overrides global
    if project_path.exists() {
        let project = ToolchainConfig::load_or_default(project_path);
        config.merge(project);
    }

    config
}

/// Get the global harbour config directory (~/.harbour).
pub fn global_config_dir() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|b| b.home_dir().join(".harbour"))
}

/// Get the global toolchain config path (~/.harbour/toolchain.toml).
pub fn global_toolchain_config_path() -> Option<PathBuf> {
    global_config_dir().map(|dir| dir.join("toolchain.toml"))
}

/// Get the project toolchain config path (.harbour/toolchain.toml).
pub fn project_toolchain_config_path(project_root: &Path) -> PathBuf {
    project_root.join(".harbour").join("toolchain.toml")
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

/// Vcpkg integration configuration.
///
/// # Example
///
/// ```toml
/// [vcpkg]
/// enabled = true
/// baseline = "abc123"  # Pin default registry
///
/// [vcpkg.registries.internal]
/// kind = "git"
/// repository = "https://github.com/myorg/vcpkg-registry"
/// baseline = "def456"
///
/// [dependencies]
/// zlib = { vcpkg = true }  # Uses default registry
/// mylib = { vcpkg = true, registry = "internal" }  # Uses named registry
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct VcpkgConfig {
    /// Explicitly enable or disable vcpkg integration
    pub enabled: Option<bool>,

    /// Path to the vcpkg root directory
    pub root: Option<PathBuf>,

    /// Vcpkg triplet to use (e.g., x64-windows)
    pub triplet: Option<String>,

    /// Default baseline commit for the Microsoft vcpkg registry
    pub baseline: Option<String>,

    /// Named registries that dependencies can reference via `registry = "name"`
    #[serde(default)]
    pub registries: HashMap<String, VcpkgRegistry>,

    /// Default features to enable for all vcpkg packages
    #[serde(default)]
    pub default_features: Vec<String>,
}

/// A custom vcpkg registry configuration.
///
/// Define registries with names, then reference them in dependencies:
/// ```toml
/// [vcpkg.registries.myregistry]
/// kind = "git"
/// repository = "https://github.com/myorg/vcpkg-registry"
/// baseline = "abc123"
///
/// [dependencies]
/// mylib = { vcpkg = true, registry = "myregistry" }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VcpkgRegistry {
    /// Registry kind: "git" or "filesystem"
    pub kind: VcpkgRegistryKind,

    /// Repository URL (for git) or path (for filesystem)
    pub repository: String,

    /// Baseline commit (required for git registries for reproducibility)
    #[serde(default)]
    pub baseline: Option<String>,

    /// Packages this registry provides (optional, for package routing)
    #[serde(default)]
    pub packages: Vec<String>,
}

/// Kind of vcpkg registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VcpkgRegistryKind {
    /// Git-based registry
    Git,
    /// Local filesystem registry
    Filesystem,
}

impl VcpkgConfig {
    /// Check if custom registries are configured.
    pub fn has_custom_registries(&self) -> bool {
        !self.registries.is_empty()
    }

    /// Get a registry by name.
    pub fn get_registry(&self, name: &str) -> Option<&VcpkgRegistry> {
        self.registries.get(name)
    }

    /// Generate vcpkg-configuration.json content for custom registries.
    ///
    /// This file is used by vcpkg to locate packages in custom registries.
    pub fn generate_vcpkg_configuration(&self) -> Option<String> {
        if self.registries.is_empty() && self.baseline.is_none() {
            return None;
        }

        let mut config = serde_json::json!({});

        // Add default registry if baseline is specified
        if let Some(ref baseline) = self.baseline {
            config["default-registry"] = serde_json::json!({
                "kind": "git",
                "repository": "https://github.com/microsoft/vcpkg",
                "baseline": baseline
            });
        }

        // Add custom registries
        if !self.registries.is_empty() {
            let registries: Vec<serde_json::Value> = self
                .registries
                .iter()
                .map(|(name, r)| {
                    let mut reg = serde_json::json!({
                        "kind": match r.kind {
                            VcpkgRegistryKind::Git => "git",
                            VcpkgRegistryKind::Filesystem => "filesystem",
                        },
                        "repository": r.repository,
                    });
                    // Add packages if specified, otherwise this registry handles all its packages
                    if !r.packages.is_empty() {
                        reg["packages"] = serde_json::json!(r.packages);
                    }
                    if let Some(ref baseline) = r.baseline {
                        reg["baseline"] = serde_json::json!(baseline);
                    }
                    // Add name as a comment for debugging
                    reg["name"] = serde_json::json!(name);
                    reg
                })
                .collect();
            config["registries"] = serde_json::json!(registries);
        }

        serde_json::to_string_pretty(&config).ok()
    }
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

        // Vcpkg settings
        if other.vcpkg.enabled.is_some() {
            self.vcpkg.enabled = other.vcpkg.enabled;
        }
        if other.vcpkg.root.is_some() {
            self.vcpkg.root = other.vcpkg.root;
        }
        if other.vcpkg.triplet.is_some() {
            self.vcpkg.triplet = other.vcpkg.triplet;
        }
        if other.vcpkg.baseline.is_some() {
            self.vcpkg.baseline = other.vcpkg.baseline;
        }
        if !other.vcpkg.registries.is_empty() {
            self.vcpkg.registries = other.vcpkg.registries;
        }
        if !other.vcpkg.default_features.is_empty() {
            self.vcpkg.default_features = other.vcpkg.default_features;
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
        assert!(config.vcpkg.enabled.is_none());
        assert!(config.vcpkg.root.is_none());
        assert!(config.vcpkg.triplet.is_none());
        assert!(config.vcpkg.baseline.is_none());
        assert!(config.vcpkg.registries.is_empty());
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

[vcpkg]
enabled = true
root = "/opt/vcpkg"
triplet = "x64-linux"
"#,
        )
        .unwrap();

        let config = Config::load(&config_path).unwrap();
        assert_eq!(config.build.backend, Some("cmake".to_string()));
        assert_eq!(config.build.linkage, Some("shared".to_string()));
        assert_eq!(config.build.jobs, Some(8));
        assert_eq!(config.ffi.bundle_dir, Some("./dist".to_string()));
        assert_eq!(config.vcpkg.enabled, Some(true));
        assert_eq!(config.vcpkg.root, Some(PathBuf::from("/opt/vcpkg")));
        assert_eq!(config.vcpkg.triplet, Some("x64-linux".to_string()));
    }

    #[test]
    fn test_config_merge() {
        let mut base = Config::default();
        base.build.backend = Some("native".to_string());
        base.build.jobs = Some(4);

        let mut override_cfg = Config::default();
        override_cfg.build.backend = Some("cmake".to_string());
        override_cfg.vcpkg.enabled = Some(true);
        override_cfg.vcpkg.triplet = Some("x64-linux".to_string());

        base.merge(override_cfg);

        assert_eq!(base.build.backend, Some("cmake".to_string()));
        assert_eq!(base.build.jobs, Some(4)); // Not overridden
        assert_eq!(base.vcpkg.enabled, Some(true));
        assert_eq!(base.vcpkg.triplet, Some("x64-linux".to_string()));
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

    #[test]
    fn test_toolchain_config_default() {
        let config = ToolchainConfig::default();
        assert!(config.toolchain.cc.is_none());
        assert!(config.toolchain.cxx.is_none());
        assert!(config.toolchain.ar.is_none());
        assert!(config.toolchain.target.is_none());
        assert!(config.toolchain.cflags.is_empty());
        assert!(!config.has_overrides());
    }

    #[test]
    fn test_toolchain_config_load() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("toolchain.toml");

        std::fs::write(
            &config_path,
            r#"
[toolchain]
cc = "/usr/bin/clang"
cxx = "/usr/bin/clang++"
ar = "/usr/bin/llvm-ar"
target = "x86_64-unknown-linux-gnu"
cflags = ["-Wall", "-Wextra"]
cxxflags = ["-std=c++17"]
ldflags = ["-lpthread"]
"#,
        )
        .unwrap();

        let config = ToolchainConfig::load(&config_path).unwrap();
        assert_eq!(config.toolchain.cc, Some(PathBuf::from("/usr/bin/clang")));
        assert_eq!(
            config.toolchain.cxx,
            Some(PathBuf::from("/usr/bin/clang++"))
        );
        assert_eq!(config.toolchain.ar, Some(PathBuf::from("/usr/bin/llvm-ar")));
        assert_eq!(
            config.toolchain.target,
            Some("x86_64-unknown-linux-gnu".to_string())
        );
        assert_eq!(config.toolchain.cflags, vec!["-Wall", "-Wextra"]);
        assert_eq!(config.toolchain.cxxflags, vec!["-std=c++17"]);
        assert_eq!(config.toolchain.ldflags, vec!["-lpthread"]);
        assert!(config.has_overrides());
    }

    #[test]
    fn test_toolchain_config_save() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("toolchain.toml");

        let mut config = ToolchainConfig::default();
        config.toolchain.cc = Some(PathBuf::from("/usr/bin/gcc"));
        config.toolchain.cflags = vec!["-O2".to_string()];

        config.save(&config_path).unwrap();

        // Reload and verify
        let loaded = ToolchainConfig::load(&config_path).unwrap();
        assert_eq!(loaded.toolchain.cc, Some(PathBuf::from("/usr/bin/gcc")));
        assert_eq!(loaded.toolchain.cflags, vec!["-O2"]);
    }

    #[test]
    fn test_toolchain_config_merge() {
        let mut base = ToolchainConfig::default();
        base.toolchain.cc = Some(PathBuf::from("/usr/bin/gcc"));
        base.toolchain.ar = Some(PathBuf::from("/usr/bin/ar"));
        base.toolchain.cflags = vec!["-Wall".to_string()];

        let mut override_cfg = ToolchainConfig::default();
        override_cfg.toolchain.cc = Some(PathBuf::from("/usr/bin/clang"));
        override_cfg.toolchain.cflags = vec!["-Werror".to_string()];

        base.merge(override_cfg);

        // cc should be overridden
        assert_eq!(base.toolchain.cc, Some(PathBuf::from("/usr/bin/clang")));
        // ar should remain unchanged
        assert_eq!(base.toolchain.ar, Some(PathBuf::from("/usr/bin/ar")));
        // cflags should be replaced (not merged)
        assert_eq!(base.toolchain.cflags, vec!["-Werror"]);
    }

    #[test]
    fn test_toolchain_config_has_overrides() {
        let mut config = ToolchainConfig::default();
        assert!(!config.has_overrides());

        config.toolchain.cc = Some(PathBuf::from("/usr/bin/gcc"));
        assert!(config.has_overrides());

        config.toolchain.cc = None;
        config.toolchain.cflags = vec!["-Wall".to_string()];
        assert!(config.has_overrides());

        config.toolchain.cflags.clear();
        assert!(!config.has_overrides());
    }

    #[test]
    fn test_load_toolchain_config_precedence() {
        let tmp = TempDir::new().unwrap();
        let global_path = tmp.path().join("global.toml");
        let project_path = tmp.path().join("project.toml");

        // Create global config
        std::fs::write(
            &global_path,
            r#"
[toolchain]
cc = "/usr/bin/gcc"
ar = "/usr/bin/ar"
cflags = ["-O2"]
"#,
        )
        .unwrap();

        // Create project config that overrides cc but not ar
        std::fs::write(
            &project_path,
            r#"
[toolchain]
cc = "/usr/bin/clang"
cflags = ["-O3"]
"#,
        )
        .unwrap();

        let config = load_toolchain_config(&global_path, &project_path);

        // Project config should override cc
        assert_eq!(config.toolchain.cc, Some(PathBuf::from("/usr/bin/clang")));
        // Global ar should be preserved
        assert_eq!(config.toolchain.ar, Some(PathBuf::from("/usr/bin/ar")));
        // Project cflags should override global
        assert_eq!(config.toolchain.cflags, vec!["-O3"]);
    }

    #[test]
    fn test_vcpkg_config_with_registries() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");

        // New table-based registry syntax: [vcpkg.registries.name]
        std::fs::write(
            &config_path,
            r#"
[vcpkg]
enabled = true
root = "/opt/vcpkg"
triplet = "x64-linux"
baseline = "abc123def456"

[vcpkg.registries.internal]
kind = "git"
repository = "https://github.com/myorg/vcpkg-registry"
baseline = "def789"
packages = ["mylib", "myotherlib"]
"#,
        )
        .unwrap();

        let config = Config::load(&config_path).unwrap();
        assert_eq!(config.vcpkg.enabled, Some(true));
        assert_eq!(config.vcpkg.baseline, Some("abc123def456".to_string()));
        assert_eq!(config.vcpkg.registries.len(), 1);

        let reg = config.vcpkg.get_registry("internal").unwrap();
        assert_eq!(reg.kind, VcpkgRegistryKind::Git);
        assert_eq!(
            reg.repository,
            "https://github.com/myorg/vcpkg-registry".to_string()
        );
        assert_eq!(reg.baseline, Some("def789".to_string()));
        assert_eq!(reg.packages, vec!["mylib", "myotherlib"]);
    }

    #[test]
    fn test_vcpkg_config_generate_configuration() {
        let mut registries = HashMap::new();
        registries.insert(
            "internal".to_string(),
            VcpkgRegistry {
                kind: VcpkgRegistryKind::Git,
                repository: "https://example.com/registry".to_string(),
                baseline: Some("reg-baseline".to_string()),
                packages: vec!["pkg1".to_string(), "pkg2".to_string()],
            },
        );

        let config = VcpkgConfig {
            enabled: Some(true),
            root: None,
            triplet: None,
            baseline: Some("main-baseline-123".to_string()),
            registries,
            default_features: vec![],
        };

        let json = config.generate_vcpkg_configuration().unwrap();
        assert!(json.contains("main-baseline-123"));
        assert!(json.contains("https://example.com/registry"));
        assert!(json.contains("pkg1"));
        assert!(json.contains("internal")); // Registry name should be included
    }
}
