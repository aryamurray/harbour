//! Global context for Harbour operations.
//!
//! Provides centralized access to configuration, paths, and environment.
//!
//! ## Multi-Registry Support
//!
//! Harbour supports multiple registries with priority-based resolution.
//! When resolving a package, registries are searched in priority order
//! (lower number = higher priority). First match wins.
//!
//! Future syntax for pinned registry:
//! ```toml
//! zlib = { version = "1.3", registry = "curated" }
//! ```

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::core::workspace::{find_manifest as ws_find_manifest, ManifestError};

/// Default registry URL for Harbour packages.
///
/// This is used when a dependency specifies only a version string
/// (e.g., `zlib = "1.3.1"`) without an explicit source.
pub const DEFAULT_REGISTRY_URL: &str = "https://github.com/aryamurray/harbour-registry";

/// Default curated registry URL.
pub const CURATED_REGISTRY_URL: &str = "https://github.com/aryamurray/harbour-registry";

// =============================================================================
// Multi-Registry Support
// =============================================================================

/// A configured package registry.
///
/// Registries are searched in priority order (lower = higher priority).
/// The first registry to contain a matching package wins.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryEntry {
    /// Human-readable registry name (e.g., "curated", "community")
    pub name: String,

    /// Git URL for the registry index
    pub url: String,

    /// Priority for resolution (lower = higher priority)
    /// Curated registries typically have priority 0-99
    /// Community registries typically have priority 100+
    #[serde(default = "default_priority")]
    pub priority: i32,

    /// Whether this registry is enabled
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_priority() -> i32 {
    100
}

fn default_enabled() -> bool {
    true
}

impl RegistryEntry {
    /// Create a new registry entry.
    pub fn new(name: impl Into<String>, url: impl Into<String>) -> Self {
        RegistryEntry {
            name: name.into(),
            url: url.into(),
            priority: default_priority(),
            enabled: true,
        }
    }

    /// Create a registry entry with a specific priority.
    pub fn with_priority(mut self, priority: i32) -> Self {
        self.priority = priority;
        self
    }

    /// Disable this registry.
    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }
}

impl Default for RegistryEntry {
    fn default() -> Self {
        RegistryEntry {
            name: "default".to_string(),
            url: DEFAULT_REGISTRY_URL.to_string(),
            priority: default_priority(),
            enabled: true,
        }
    }
}

/// List of configured registries.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RegistryList {
    /// Configured registries
    #[serde(default)]
    pub registries: Vec<RegistryEntry>,
}

impl RegistryList {
    /// Create a new empty registry list.
    pub fn new() -> Self {
        RegistryList {
            registries: Vec::new(),
        }
    }

    /// Create a registry list with default registries.
    pub fn with_defaults() -> Self {
        RegistryList {
            registries: vec![
                RegistryEntry::new("curated", CURATED_REGISTRY_URL).with_priority(0),
                RegistryEntry::new("default", DEFAULT_REGISTRY_URL).with_priority(100),
            ],
        }
    }

    /// Add a registry.
    pub fn add(&mut self, entry: RegistryEntry) {
        self.registries.push(entry);
        self.sort_by_priority();
    }

    /// Sort registries by priority.
    fn sort_by_priority(&mut self) {
        self.registries.sort_by_key(|r| r.priority);
    }

    /// Get enabled registries in priority order.
    pub fn enabled(&self) -> impl Iterator<Item = &RegistryEntry> {
        self.registries.iter().filter(|r| r.enabled)
    }

    /// Find a registry by name.
    pub fn by_name(&self, name: &str) -> Option<&RegistryEntry> {
        self.registries.iter().find(|r| r.name == name)
    }

    /// Check if a registry with the given name exists.
    pub fn contains(&self, name: &str) -> bool {
        self.registries.iter().any(|r| r.name == name)
    }
}

/// Project directories for Harbour
static PROJECT_DIRS: LazyLock<Option<ProjectDirs>> =
    LazyLock::new(|| ProjectDirs::from("com", "harbour", "harbour"));

/// Global context containing configuration and paths.
#[derive(Debug, Clone)]
pub struct GlobalContext {
    /// Current working directory
    cwd: PathBuf,

    /// Home directory for global Harbour data (~/.harbour/)
    home: PathBuf,

    /// Whether to use verbose output
    verbose: bool,

    /// Whether to use colors in output
    color: bool,

    /// Configured registries
    registries: RegistryList,
}

impl GlobalContext {
    /// Create a new GlobalContext with defaults.
    pub fn new() -> Result<Self> {
        let cwd = std::env::current_dir().context("failed to get current directory")?;

        let home = if let Some(dirs) = PROJECT_DIRS.as_ref() {
            dirs.cache_dir().to_path_buf()
        } else {
            // Fallback to ~/.harbour
            dirs::home_dir()
                .map(|h| h.join(".harbour"))
                .unwrap_or_else(|| PathBuf::from(".harbour"))
        };

        Ok(GlobalContext {
            cwd,
            home,
            verbose: false,
            color: true,
            registries: RegistryList::with_defaults(),
        })
    }

    /// Create a GlobalContext with a specific working directory.
    pub fn with_cwd(cwd: PathBuf) -> Result<Self> {
        let mut ctx = Self::new()?;
        ctx.cwd = cwd;
        Ok(ctx)
    }

    /// Create a GlobalContext with custom registries.
    pub fn with_registries(mut self, registries: RegistryList) -> Self {
        self.registries = registries;
        self
    }

    /// Set verbose mode.
    pub fn set_verbose(&mut self, verbose: bool) {
        self.verbose = verbose;
    }

    /// Set color output.
    pub fn set_color(&mut self, color: bool) {
        self.color = color;
    }

    /// Get the current working directory.
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// Get the Harbour home directory (~/.harbour/).
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// Get the global cache directory for downloaded sources.
    pub fn cache_dir(&self) -> PathBuf {
        self.home.join("cache")
    }

    /// Get the git cache directory.
    pub fn git_cache_dir(&self) -> PathBuf {
        self.cache_dir().join("git")
    }

    /// Get the registry cache directory (future).
    pub fn registry_cache_dir(&self) -> PathBuf {
        self.cache_dir().join("registry")
    }

    /// Get the global configuration file path.
    pub fn config_path(&self) -> PathBuf {
        self.home.join("config.toml")
    }

    /// Get the project-local Harbour directory.
    pub fn project_harbour_dir(&self) -> PathBuf {
        self.cwd.join(".harbour")
    }

    /// Get the project-local target directory.
    pub fn target_dir(&self) -> PathBuf {
        self.project_harbour_dir().join("target")
    }

    /// Get the project-local cache directory.
    pub fn project_cache_dir(&self) -> PathBuf {
        self.project_harbour_dir().join("cache")
    }

    /// Check if verbose mode is enabled.
    pub fn is_verbose(&self) -> bool {
        self.verbose
    }

    /// Check if color output is enabled.
    pub fn color(&self) -> bool {
        self.color
    }

    /// Get the configured registries.
    pub fn registries(&self) -> &RegistryList {
        &self.registries
    }

    /// Get a mutable reference to the configured registries.
    pub fn registries_mut(&mut self) -> &mut RegistryList {
        &mut self.registries
    }

    /// Add a registry to the configuration.
    pub fn add_registry(&mut self, entry: RegistryEntry) {
        self.registries.add(entry);
    }

    /// Find the manifest file (Harbour.toml or Harbor.toml) starting from cwd and searching upward.
    ///
    /// Returns an error if both manifest files exist in the same directory (ambiguous).
    /// Returns None if no manifest is found in the directory tree.
    pub fn find_manifest(&self) -> Result<PathBuf, ManifestError> {
        let mut current = self.cwd.clone();
        loop {
            match ws_find_manifest(&current) {
                Ok(path) => return Ok(path),
                Err(ManifestError::AmbiguousManifest { primary, alias }) => {
                    return Err(ManifestError::AmbiguousManifest { primary, alias });
                }
                Err(ManifestError::NotFound { .. }) => {
                    // Not in this directory, keep searching upward
                    if !current.pop() {
                        return Err(ManifestError::NotFound { dir: self.cwd.clone() });
                    }
                }
            }
        }
    }

    /// Find the workspace root (directory containing Harbour.toml).
    pub fn find_workspace_root(&self) -> Result<PathBuf, ManifestError> {
        self.find_manifest().map(|p| p.parent().unwrap().to_path_buf())
    }

    /// Ensure a directory exists, creating it if necessary.
    pub fn ensure_dir(&self, path: &Path) -> Result<()> {
        if !path.exists() {
            std::fs::create_dir_all(path)
                .with_context(|| format!("failed to create directory: {}", path.display()))?;
        }
        Ok(())
    }
}

impl Default for GlobalContext {
    fn default() -> Self {
        Self::new().expect("failed to create default GlobalContext")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_context_paths() {
        let ctx = GlobalContext::new().unwrap();
        assert!(ctx.cwd().is_absolute());
        assert!(ctx.home().to_string_lossy().contains("harbour"));
    }

    #[test]
    fn test_find_manifest() {
        let tmp = TempDir::new().unwrap();
        let manifest = tmp.path().join("Harbor.toml");
        std::fs::write(&manifest, "[package]\nname = \"test\"\nversion = \"0.1.0\"\n").unwrap();

        let ctx = GlobalContext::with_cwd(tmp.path().to_path_buf()).unwrap();
        assert_eq!(ctx.find_manifest().ok(), Some(manifest));
    }

    #[test]
    fn test_find_manifest_ambiguous() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("Harbour.toml"), "[package]\nname = \"a\"\nversion = \"0.1.0\"\n").unwrap();
        std::fs::write(tmp.path().join("Harbor.toml"), "[package]\nname = \"b\"\nversion = \"0.1.0\"\n").unwrap();

        let ctx = GlobalContext::with_cwd(tmp.path().to_path_buf()).unwrap();
        let result = ctx.find_manifest();
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ManifestError::AmbiguousManifest { .. }));
    }

    #[test]
    fn test_registry_entry() {
        let entry = RegistryEntry::new("test", "https://example.com/registry")
            .with_priority(50);

        assert_eq!(entry.name, "test");
        assert_eq!(entry.url, "https://example.com/registry");
        assert_eq!(entry.priority, 50);
        assert!(entry.enabled);
    }

    #[test]
    fn test_registry_list_defaults() {
        let list = RegistryList::with_defaults();
        assert_eq!(list.registries.len(), 2);

        // First should be curated (priority 0)
        assert_eq!(list.registries[0].name, "curated");
        assert_eq!(list.registries[0].priority, 0);

        // Second should be default (priority 100)
        assert_eq!(list.registries[1].name, "default");
        assert_eq!(list.registries[1].priority, 100);
    }

    #[test]
    fn test_registry_list_priority_sorting() {
        let mut list = RegistryList::new();
        list.add(RegistryEntry::new("low", "https://low.com").with_priority(200));
        list.add(RegistryEntry::new("high", "https://high.com").with_priority(10));
        list.add(RegistryEntry::new("mid", "https://mid.com").with_priority(50));

        // Should be sorted by priority
        assert_eq!(list.registries[0].name, "high");
        assert_eq!(list.registries[1].name, "mid");
        assert_eq!(list.registries[2].name, "low");
    }

    #[test]
    fn test_registry_list_enabled() {
        let mut list = RegistryList::new();
        list.add(RegistryEntry::new("enabled", "https://enabled.com"));
        list.add(RegistryEntry::new("disabled", "https://disabled.com").disabled());

        let enabled: Vec<_> = list.enabled().collect();
        assert_eq!(enabled.len(), 1);
        assert_eq!(enabled[0].name, "enabled");
    }

    #[test]
    fn test_registry_list_by_name() {
        let list = RegistryList::with_defaults();

        assert!(list.by_name("curated").is_some());
        assert!(list.by_name("default").is_some());
        assert!(list.by_name("nonexistent").is_none());
    }

    #[test]
    fn test_context_registries() {
        let ctx = GlobalContext::new().unwrap();
        assert!(ctx.registries().contains("curated"));
        assert!(ctx.registries().contains("default"));
    }
}
