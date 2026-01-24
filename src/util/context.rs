//! Global context for Harbour operations.
//!
//! Provides centralized access to configuration, paths, and environment.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::{Context, Result};
use directories::ProjectDirs;

use crate::core::workspace::{find_manifest as ws_find_manifest, ManifestError};

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
        })
    }

    /// Create a GlobalContext with a specific working directory.
    pub fn with_cwd(cwd: PathBuf) -> Result<Self> {
        let mut ctx = Self::new()?;
        ctx.cwd = cwd;
        Ok(ctx)
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
}
