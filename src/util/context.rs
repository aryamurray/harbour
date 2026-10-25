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

/// Environment variable that, when set, replaces the built-in registry
/// list with a single registry pointing at the given URL.
///
/// This exists so integration tests (and CI) can point `harbour` at a
/// local, fully offline registry fixture (e.g. a `file://` URL to a
/// checked-out registry directory) instead of the real
/// `DEFAULT_REGISTRY_URL`. It is not part of the stable CLI surface.
pub const REGISTRY_OVERRIDE_ENV: &str = "HARBOUR_TEST_REGISTRY_URL";

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

/// Which file the global `config.toml` is read from, and a message when that
/// is not the documented location.
///
/// The documented location is `~/.harbour/config.toml`, next to the global
/// `toolchain.toml`. Earlier versions read it from the platform *cache*
/// directory instead (`~/Library/Caches/com.harbour.harbour/` on macOS), which
/// the OS may purge and users do not look in. So that an existing install keeps
/// working with no action, a config found only at that old location is still
/// read, with a notice saying where it should live. Nothing is ever moved,
/// copied or deleted on the user's behalf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalConfigResolution {
    /// The file to read. When no config exists anywhere this is the documented
    /// location, i.e. where one should be created.
    pub path: PathBuf,
    /// A one-line explanation when the file in use is the legacy one, or when a
    /// legacy file exists and is being ignored.
    pub notice: Option<String>,
}

/// Decide which global `config.toml` to read.
///
/// 1. `<config_dir>/config.toml` if it exists. If a legacy file also exists it
///    is ignored, and the notice says so.
/// 2. Otherwise `<legacy_dir>/config.toml` if it exists, with a notice naming
///    both paths and the command that moves it.
/// 3. Otherwise `<config_dir>/config.toml`, with no notice (built-in defaults
///    apply because the file is absent).
pub fn resolve_global_config(
    config_dir: &Path,
    legacy_dir: Option<&Path>,
) -> GlobalConfigResolution {
    let new = config_dir.join("config.toml");
    let old = legacy_dir
        .map(|d| d.join("config.toml"))
        .filter(|o| *o != new && o.is_file());

    if new.is_file() {
        let notice = old.map(|old| {
            format!(
                "ignoring the config at {} (the platform cache directory, where older versions \
                 kept it); {} is used instead. Delete the old file once you no longer need it",
                old.display(),
                new.display()
            )
        });
        return GlobalConfigResolution { path: new, notice };
    }

    match old {
        Some(old) => {
            let notice = format!(
                "reading the config at {}, the platform cache directory where older versions \
                 kept it; it belongs at {}. Move it with: mkdir -p '{}' && mv '{}' '{}'",
                old.display(),
                new.display(),
                config_dir.display(),
                old.display(),
                new.display()
            );
            GlobalConfigResolution {
                path: old,
                notice: Some(notice),
            }
        }
        None => GlobalConfigResolution {
            path: new,
            notice: None,
        },
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

    /// Root of Harbour's global *caches* (downloaded sources, registry clones):
    /// the platform cache directory. Configuration does not live here; see
    /// `config_file`.
    home: PathBuf,

    /// The global `config.toml` actually in use, resolved once at construction
    /// (see [`resolve_global_config`]).
    config_file: PathBuf,

    /// Set when the config in use is the legacy one, or a legacy one is ignored.
    config_notice: Option<String>,

    /// Whether to use verbose output
    verbose: bool,

    /// Whether to use colors in output
    color: bool,

    /// Configured registries
    registries: RegistryList,
}

impl GlobalContext {
    /// Create a new GlobalContext with defaults.
    ///
    /// Reads the machine's own configuration: `~/.harbour/config.toml` (falling
    /// back, with a notice, to the legacy copy in the platform cache directory;
    /// see [`resolve_global_config`]) and `./.harbour/config.toml`, and honours
    /// [`REGISTRY_OVERRIDE_ENV`]. Code that must not see the machine it runs on
    /// -- every test -- uses [`GlobalContext::with_dirs`] or
    /// [`GlobalContext::with_cwd_isolated`] instead.
    pub fn new() -> Result<Self> {
        let cwd = std::env::current_dir().context("failed to get current directory")?;

        // Caches: the platform cache directory.
        let cache_home = if let Some(dirs) = PROJECT_DIRS.as_ref() {
            dirs.cache_dir().to_path_buf()
        } else {
            // Fallback to ~/.harbour using directories::BaseDirs
            directories::BaseDirs::new()
                .map(|b| b.home_dir().join(".harbour"))
                .unwrap_or_else(|| PathBuf::from(".harbour"))
        };

        // Configuration: `~/.harbour`, the documented location (and where
        // `toolchain.toml` already lives). The cache directory is consulted only
        // as the legacy location.
        let config_dir =
            crate::util::config::global_config_dir().unwrap_or_else(|| cache_home.clone());
        let legacy = (config_dir != cache_home).then(|| cache_home.clone());

        let mut ctx = Self::with_config_dirs(config_dir, legacy, cache_home, cwd)?;

        // Said once per process: a legacy config is a thing to fix, not to hear
        // about on every command of every run.
        if let Some(notice) = &ctx.config_notice {
            static SAID: std::sync::Once = std::sync::Once::new();
            SAID.call_once(|| tracing::warn!("{notice}"));
        }

        // An explicit override wins over the config file. It is an opt-in the
        // integration tests use on a *subprocess*; `with_dirs` deliberately
        // does not read the environment at all.
        if let Ok(url) = std::env::var(REGISTRY_OVERRIDE_ENV) {
            if !url.is_empty() {
                let mut list = RegistryList::new();
                list.add(RegistryEntry::new("test-override", url));
                ctx.registries = list;
            }
        }
        Ok(ctx)
    }

    /// Create a GlobalContext from explicit directories.
    ///
    /// The only configuration it reads is `<home>/config.toml` and
    /// `<cwd>/.harbour/config.toml`; it never looks at the platform cache or
    /// config directories, the user's real home, or the environment. A `home`
    /// with no `config.toml` therefore yields exactly the built-in defaults on
    /// every machine, which is what a test of the defaults needs.
    ///
    /// Configured `[[registries]]` replace the defaults rather than adding to
    /// them, so a build resolves against exactly what the config lists.
    pub fn with_dirs(home: PathBuf, cwd: PathBuf) -> Result<Self> {
        Self::with_config_dirs(home.clone(), None, home, cwd)
    }

    /// Like [`with_dirs`](Self::with_dirs), with the global config directory
    /// and an optional legacy one named separately from the cache `home`.
    ///
    /// Reads `<config_dir>/config.toml`, or `<legacy_dir>/config.toml` when
    /// that is absent (see [`resolve_global_config`]); still never the
    /// environment or the machine's real directories. The resolution is kept
    /// on the context ([`config_path`](Self::config_path),
    /// [`config_notice`](Self::config_notice)) so every command that re-reads
    /// the global config reads the same file this context did.
    pub fn with_config_dirs(
        config_dir: PathBuf,
        legacy_dir: Option<PathBuf>,
        home: PathBuf,
        cwd: PathBuf,
    ) -> Result<Self> {
        let resolution = resolve_global_config(&config_dir, legacy_dir.as_deref());
        let config = crate::util::config::load_config(
            &resolution.path,
            &cwd.join(".harbour").join("config.toml"),
        );
        let registries = if config.registries.is_empty() {
            RegistryList::with_defaults()
        } else {
            let mut list = RegistryList::new();
            for entry in config.registries {
                if entry.enabled {
                    list.add(entry);
                }
            }
            list
        };

        Ok(GlobalContext {
            cwd,
            home,
            config_file: resolution.path,
            config_notice: resolution.notice,
            verbose: false,
            color: true,
            registries,
        })
    }

    /// Create a GlobalContext with a specific working directory.
    ///
    /// Still reads the machine's own global configuration (it is [`Self::new`]
    /// with the directory swapped afterwards), so tests should prefer
    /// [`Self::with_cwd_isolated`].
    pub fn with_cwd(cwd: PathBuf) -> Result<Self> {
        let mut ctx = Self::new()?;
        ctx.cwd = cwd;
        Ok(ctx)
    }

    /// A context with a specific working directory and an empty private home,
    /// for tests: it sees no configuration from the machine it runs on, so its
    /// registries are the built-in defaults and its cache and config paths are
    /// somewhere nothing else reads or writes.
    ///
    /// The home is a path that does not exist yet, unique to this call, under
    /// the OS temporary directory. Nothing is created unless the code under
    /// test writes there.
    pub fn with_cwd_isolated(cwd: PathBuf) -> Result<Self> {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let home =
            std::env::temp_dir().join(format!("harbour-isolated-home-{}-{n}", std::process::id()));
        Self::with_dirs(home, cwd)
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

    /// Get the root of Harbour's global caches (the platform cache directory;
    /// see [`config_path`](Self::config_path) for where configuration lives).
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

    /// Get the global configuration file in use: `~/.harbour/config.toml`, or
    /// the legacy copy in the platform cache directory when only that exists
    /// (see [`resolve_global_config`]). When no config exists anywhere this is
    /// the documented location, where one should be created.
    pub fn config_path(&self) -> PathBuf {
        self.config_file.clone()
    }

    /// The notice about a legacy global config, if there is one: the file in
    /// use is the legacy one, or a legacy file is being ignored.
    pub fn config_notice(&self) -> Option<&str> {
        self.config_notice.as_deref()
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

    /// Get the default registry URL.
    ///
    /// Returns the URL of the first enabled registry with the highest priority.
    /// Falls back to the built-in DEFAULT_REGISTRY_URL if no registries are configured.
    pub fn default_registry_url(&self) -> url::Url {
        self.registries
            .enabled()
            .next()
            .map(|r| r.url.as_str())
            .unwrap_or(DEFAULT_REGISTRY_URL)
            .parse()
            .expect("invalid default registry URL")
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
                        return Err(ManifestError::NotFound {
                            dir: self.cwd.clone(),
                        });
                    }
                }
            }
        }
    }

    /// Find the workspace root (directory containing Harbour.toml).
    pub fn find_workspace_root(&self) -> Result<PathBuf, ManifestError> {
        self.find_manifest()
            .map(|p| p.parent().unwrap().to_path_buf())
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
        // hermetic: allow-ambient-context -- this tests how `new()` derives the
        // real home and cwd; it asserts on paths only, never on configuration.
        let ctx = GlobalContext::new().unwrap();
        assert!(ctx.cwd().is_absolute());
        assert!(ctx.home().to_string_lossy().contains("harbour"));
    }

    fn write_registry_config(dir: &Path, name: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("config.toml"),
            format!(
                "[[registries]]\nname = \"{name}\"\nurl = \"https://example.invalid/{name}\"\n"
            ),
        )
        .unwrap();
    }

    fn registry_names(ctx: &GlobalContext) -> Vec<String> {
        ctx.registries()
            .registries
            .iter()
            .map(|r| r.name.clone())
            .collect()
    }

    /// Build a context from a config dir, a legacy dir and an empty cwd, all
    /// under one temp dir, so nothing of the machine's own is read.
    fn ctx_with(tmp: &TempDir, new: bool, old: bool) -> (GlobalContext, PathBuf, PathBuf) {
        let config_dir = tmp.path().join("config");
        let legacy_dir = tmp.path().join("cache");
        let cwd = tmp.path().join("cwd");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(&legacy_dir).unwrap();
        if new {
            write_registry_config(&config_dir, "from-new");
        }
        if old {
            write_registry_config(&legacy_dir, "from-old");
        }
        let ctx = GlobalContext::with_config_dirs(
            config_dir.clone(),
            Some(legacy_dir.clone()),
            legacy_dir.clone(),
            cwd,
        )
        .unwrap();
        (ctx, config_dir, legacy_dir)
    }

    #[test]
    fn global_config_new_location_only() {
        let tmp = TempDir::new().unwrap();
        let (ctx, config_dir, _) = ctx_with(&tmp, true, false);
        assert_eq!(registry_names(&ctx), ["from-new"]);
        assert_eq!(ctx.config_path(), config_dir.join("config.toml"));
        assert!(ctx.config_notice().is_none());
    }

    #[test]
    fn global_config_legacy_only_is_read_with_a_notice() {
        let tmp = TempDir::new().unwrap();
        let (ctx, config_dir, legacy_dir) = ctx_with(&tmp, false, true);
        assert_eq!(registry_names(&ctx), ["from-old"]);
        assert_eq!(ctx.config_path(), legacy_dir.join("config.toml"));
        let notice = ctx
            .config_notice()
            .expect("a legacy config must be reported");
        assert!(notice.contains(&config_dir.join("config.toml").display().to_string()));
        assert!(notice.contains("mv"));
        // Never moved or deleted on the user's behalf.
        assert!(legacy_dir.join("config.toml").is_file());
        assert!(!config_dir.join("config.toml").exists());
    }

    #[test]
    fn global_config_both_prefers_new_and_says_the_old_is_ignored() {
        let tmp = TempDir::new().unwrap();
        let (ctx, config_dir, legacy_dir) = ctx_with(&tmp, true, true);
        assert_eq!(registry_names(&ctx), ["from-new"]);
        assert_eq!(ctx.config_path(), config_dir.join("config.toml"));
        let notice = ctx
            .config_notice()
            .expect("an ignored legacy config must be reported");
        assert!(notice.starts_with("ignoring"));
        assert!(notice.contains(&legacy_dir.join("config.toml").display().to_string()));
    }

    #[test]
    fn global_config_neither_gives_defaults_and_the_documented_path() {
        let tmp = TempDir::new().unwrap();
        let (ctx, config_dir, _) = ctx_with(&tmp, false, false);
        assert_eq!(ctx.config_path(), config_dir.join("config.toml"));
        assert!(ctx.config_notice().is_none());
        assert_eq!(
            registry_names(&ctx),
            registry_names(
                &GlobalContext::with_dirs(tmp.path().join("h"), tmp.path().join("cwd")).unwrap()
            )
        );
    }

    #[test]
    fn resolve_ignores_a_legacy_dir_equal_to_the_config_dir() {
        let tmp = TempDir::new().unwrap();
        write_registry_config(tmp.path(), "x");
        let r = resolve_global_config(tmp.path(), Some(tmp.path()));
        assert_eq!(r.path, tmp.path().join("config.toml"));
        assert!(r.notice.is_none());
    }

    #[test]
    fn test_find_manifest() {
        let tmp = TempDir::new().unwrap();
        let manifest = tmp.path().join("Harbor.toml");
        std::fs::write(
            &manifest,
            "[package]\nname = \"test\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();

        let ctx = GlobalContext::with_cwd_isolated(tmp.path().to_path_buf()).unwrap();
        assert_eq!(ctx.find_manifest().ok(), Some(manifest));
    }

    #[test]
    fn test_find_manifest_ambiguous() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("Harbour.toml"),
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("Harbor.toml"),
            "[package]\nname = \"b\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();

        let ctx = GlobalContext::with_cwd_isolated(tmp.path().to_path_buf()).unwrap();
        let result = ctx.find_manifest();
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ManifestError::AmbiguousManifest { .. }
        ));
    }

    #[test]
    fn test_registry_entry() {
        let entry = RegistryEntry::new("test", "https://example.com/registry").with_priority(50);

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
        // The built-in defaults, from a home with no config, whatever the
        // machine running the test has configured.
        let tmp = TempDir::new().unwrap();
        let ctx =
            GlobalContext::with_dirs(tmp.path().join("home"), tmp.path().to_path_buf()).unwrap();
        assert!(ctx.registries().contains("curated"));
        assert!(ctx.registries().contains("default"));
    }

    #[test]
    fn test_default_registry_url() {
        // With the BUILT-IN defaults (a home with no config), not whatever the
        // machine running the test has configured: this used to read the real
        // global config and failed on a machine with a local registry.
        let tmp = TempDir::new().unwrap();
        let ctx =
            GlobalContext::with_dirs(tmp.path().join("home"), tmp.path().to_path_buf()).unwrap();
        let url = ctx.default_registry_url();

        // Should return the curated registry (highest priority)
        assert_eq!(url.as_str(), CURATED_REGISTRY_URL);
    }

    #[test]
    fn test_default_registry_url_fallback() {
        // Test with empty registry list
        let tmp = TempDir::new().unwrap();
        let ctx = GlobalContext::with_dirs(tmp.path().join("home"), tmp.path().to_path_buf())
            .unwrap()
            .with_registries(RegistryList::new());

        let url = ctx.default_registry_url();

        // Should fallback to DEFAULT_REGISTRY_URL
        assert_eq!(url.as_str(), DEFAULT_REGISTRY_URL);
    }

    /// A test must not build its context from the machine it runs on.
    ///
    /// The plain context constructors (`new`, and `with_cwd` which calls it)
    /// read the developer's real global config, so a test using either passes or fails
    /// depending on what is configured where it runs: `test_default_registry_url`
    /// failed on a machine with a local registry configured and passed in CI,
    /// where nothing is. This reads every test module's source and fails on a
    /// direct use, so the mistake cannot come back in a test that does not
    /// exist yet. Use `GlobalContext::with_dirs` (explicit home, nothing
    /// ambient) or `with_cwd_isolated`. A test that really is about the real
    /// home puts `hermetic: allow-ambient-context` in a comment within three
    /// lines above the call and says why.
    #[test]
    fn no_test_builds_a_context_from_the_machine_config() {
        fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    rust_files(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    out.push(path);
                }
            }
        }

        // The needles are assembled so this very test does not match itself.
        let needles = [
            concat!("GlobalContext", "::new("),
            concat!("GlobalContext", "::with_cwd("),
        ];
        let allow = concat!("hermetic: ", "allow-ambient-context");
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));

        let mut files = Vec::new();
        rust_files(&root.join("src"), &mut files);
        rust_files(&root.join("tests"), &mut files);

        let mut offenders = Vec::new();
        for file in files {
            let text = std::fs::read_to_string(&file).unwrap();
            let lines: Vec<&str> = text.lines().collect();
            // Files under tests/ are all test code; in src/ only what follows
            // the first `#[cfg(test)]` is.
            let in_tests_dir = file.starts_with(root.join("tests"));
            let start = if in_tests_dir {
                0
            } else {
                match lines.iter().position(|l| l.trim() == "#[cfg(test)]") {
                    Some(i) => i + 1,
                    None => continue,
                }
            };
            for (i, line) in lines.iter().enumerate().skip(start) {
                if needles.iter().any(|n| line.contains(n)) {
                    let from = i.saturating_sub(3);
                    if !lines[from..=i].iter().any(|l| l.contains(allow)) {
                        offenders.push(format!(
                            "{}:{}",
                            file.strip_prefix(root).unwrap_or(&file).display(),
                            i + 1
                        ));
                    }
                }
            }
        }

        assert!(
            offenders.is_empty(),
            "these tests build a GlobalContext from the machine's real config, so their \
             result depends on where they run: {offenders:#?}\n\
             use `GlobalContext::with_dirs(home, cwd)` or `with_cwd_isolated(cwd)`"
        );
    }

    /// `with_dirs` is exactly what its directories say: a home with a config
    /// gets that config's registries and nothing else, a home with none gets
    /// the built-in defaults, and neither is influenced by where the test runs.
    #[test]
    fn with_dirs_uses_only_the_directories_it_is_given() {
        let tmp = TempDir::new().unwrap();

        // No config: the built-in defaults.
        let empty =
            GlobalContext::with_dirs(tmp.path().join("empty-home"), tmp.path().to_path_buf())
                .unwrap();
        assert!(empty.registries().contains("curated"));
        assert!(empty.registries().contains("default"));
        assert_eq!(empty.default_registry_url().as_str(), CURATED_REGISTRY_URL);

        // A config: exactly its registries, replacing the defaults.
        let home = tmp.path().join("configured-home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join("config.toml"),
            "[[registries]]\nname = \"only-this\"\nurl = \"file:///nowhere\"\npriority = -5\n",
        )
        .unwrap();
        let configured = GlobalContext::with_dirs(home, tmp.path().to_path_buf()).unwrap();
        assert!(configured.registries().contains("only-this"));
        assert!(
            !configured.registries().contains("curated"),
            "configured registries replace the defaults"
        );
        assert_eq!(
            configured.default_registry_url().as_str(),
            "file:///nowhere"
        );
    }

    /// `with_cwd_isolated` really is isolated: two calls get different empty
    /// homes, and neither contains a config.
    #[test]
    fn with_cwd_isolated_gets_an_empty_private_home() {
        let tmp = TempDir::new().unwrap();
        let a = GlobalContext::with_cwd_isolated(tmp.path().to_path_buf()).unwrap();
        let b = GlobalContext::with_cwd_isolated(tmp.path().to_path_buf()).unwrap();
        assert_ne!(a.home(), b.home());
        assert!(!a.config_path().exists());
        assert!(a.registries().contains("curated"));
    }
}
