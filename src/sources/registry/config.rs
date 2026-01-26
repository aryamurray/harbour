//! Registry configuration parsing.
//!
//! Each registry has a config.toml at its root that defines registry metadata.
//!
//! ## Example config.toml
//!
//! ```toml
//! [registry]
//! name = "harbour-curated"
//! registry_version = 1
//! layout = "letter/name/version"
//! default_branch = "main"
//!
//! [curated]
//! min_platform_count = 3
//! requires_ci_pass = true
//! ```

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Registry configuration from config.toml.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryConfig {
    /// Registry metadata section
    pub registry: RegistryMetadata,

    /// Curated registry settings (optional)
    #[serde(default)]
    pub curated: Option<CuratedConfig>,
}

/// Registry metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryMetadata {
    /// Human-readable registry name
    pub name: String,

    /// Registry format version (bump when format changes)
    #[serde(default = "default_registry_version")]
    pub registry_version: u32,

    /// Layout identifier for future-proofing
    #[serde(default = "default_layout")]
    pub layout: String,

    /// Default branch for git operations
    #[serde(default)]
    pub default_branch: Option<String>,
}

fn default_registry_version() -> u32 {
    1
}

fn default_layout() -> String {
    "letter/name/version".to_string()
}

/// Curated registry configuration.
///
/// These settings apply additional quality requirements for packages
/// in a curated registry (vs. a general-purpose registry).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CuratedConfig {
    /// Minimum number of platforms a package must support
    #[serde(default = "default_min_platform_count")]
    pub min_platform_count: usize,

    /// Whether CI must pass before a package can be published
    #[serde(default = "default_requires_ci_pass")]
    pub requires_ci_pass: bool,

    /// Require harness config for consumer testing
    #[serde(default)]
    pub requires_harness: bool,

    /// Maximum tier level allowed (0 = foundation only)
    #[serde(default)]
    pub max_tier: Option<u8>,
}

fn default_min_platform_count() -> usize {
    3
}

fn default_requires_ci_pass() -> bool {
    true
}

impl Default for CuratedConfig {
    fn default() -> Self {
        CuratedConfig {
            min_platform_count: default_min_platform_count(),
            requires_ci_pass: default_requires_ci_pass(),
            requires_harness: false,
            max_tier: None,
        }
    }
}

impl RegistryConfig {
    /// Load registry config from a path.
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read registry config: {}", path.display()))?;

        Self::parse(&content)
    }

    /// Parse registry config from TOML content.
    pub fn parse(content: &str) -> Result<Self> {
        let config: RegistryConfig =
            toml::from_str(content).with_context(|| "failed to parse registry config")?;

        config.validate()?;
        Ok(config)
    }

    /// Validate the registry configuration.
    pub fn validate(&self) -> Result<()> {
        // Currently we only support version 1
        if self.registry.registry_version != 1 {
            anyhow::bail!(
                "unsupported registry version {}, expected 1",
                self.registry.registry_version
            );
        }

        // Currently we only support the letter/name/version layout
        if self.registry.layout != "letter/name/version" {
            anyhow::bail!(
                "unsupported registry layout '{}', expected 'letter/name/version'",
                self.registry.layout
            );
        }

        Ok(())
    }

    /// Get the registry name.
    pub fn name(&self) -> &str {
        &self.registry.name
    }

    /// Get the default branch, if specified.
    pub fn default_branch(&self) -> Option<&str> {
        self.registry.default_branch.as_deref()
    }

    /// Check if this is a curated registry.
    pub fn is_curated(&self) -> bool {
        self.curated.is_some()
    }

    /// Get the curated config, if present.
    pub fn curated(&self) -> Option<&CuratedConfig> {
        self.curated.as_ref()
    }

    /// Get minimum platform count for curated registries.
    pub fn min_platform_count(&self) -> usize {
        self.curated
            .as_ref()
            .map(|c| c.min_platform_count)
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_config() {
        let content = r#"
[registry]
name = "harbour-official"
registry_version = 1
layout = "letter/name/version"
default_branch = "main"
"#;

        let config = RegistryConfig::parse(content).unwrap();
        assert_eq!(config.name(), "harbour-official");
        assert_eq!(config.default_branch(), Some("main"));
    }

    #[test]
    fn test_parse_minimal_config() {
        let content = r#"
[registry]
name = "my-registry"
"#;

        let config = RegistryConfig::parse(content).unwrap();
        assert_eq!(config.name(), "my-registry");
        assert_eq!(config.registry.registry_version, 1);
        assert_eq!(config.registry.layout, "letter/name/version");
        assert!(config.default_branch().is_none());
    }

    #[test]
    fn test_invalid_version() {
        let content = r#"
[registry]
name = "test"
registry_version = 2
"#;

        let result = RegistryConfig::parse(content);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("unsupported registry version"));
    }

    #[test]
    fn test_invalid_layout() {
        let content = r#"
[registry]
name = "test"
layout = "flat"
"#;

        let result = RegistryConfig::parse(content);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("unsupported registry layout"));
    }

    #[test]
    fn test_parse_curated_config() {
        let content = r#"
[registry]
name = "harbour-curated"
registry_version = 1
layout = "letter/name/version"
default_branch = "main"

[curated]
min_platform_count = 3
requires_ci_pass = true
requires_harness = true
max_tier = 2
"#;

        let config = RegistryConfig::parse(content).unwrap();
        assert!(config.is_curated());

        let curated = config.curated().unwrap();
        assert_eq!(curated.min_platform_count, 3);
        assert!(curated.requires_ci_pass);
        assert!(curated.requires_harness);
        assert_eq!(curated.max_tier, Some(2));

        assert_eq!(config.min_platform_count(), 3);
    }

    #[test]
    fn test_curated_config_defaults() {
        let content = r#"
[registry]
name = "harbour-curated"

[curated]
"#;

        let config = RegistryConfig::parse(content).unwrap();
        assert!(config.is_curated());

        let curated = config.curated().unwrap();
        assert_eq!(curated.min_platform_count, 3); // default
        assert!(curated.requires_ci_pass); // default true
        assert!(!curated.requires_harness); // default false
        assert_eq!(curated.max_tier, None); // default None
    }

    #[test]
    fn test_non_curated_registry() {
        let content = r#"
[registry]
name = "community-registry"
"#;

        let config = RegistryConfig::parse(content).unwrap();
        assert!(!config.is_curated());
        assert!(config.curated().is_none());
        assert_eq!(config.min_platform_count(), 0); // non-curated returns 0
    }
}
