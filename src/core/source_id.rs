//! Source identification - WHERE packages come from.
//!
//! SourceId is an interned identifier that uniquely identifies a package source.
//! It's designed for cheap comparison and cloning.

use std::collections::HashMap;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, RwLock};

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use url::Url;

/// Global source ID interner
static SOURCE_INTERNER: LazyLock<RwLock<HashMap<SourceIdInner, &'static SourceIdInner>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// A unique identifier for a package source (interned).
///
/// SourceIds are cheap to clone and compare (pointer comparison).
#[derive(Clone, Copy)]
pub struct SourceId {
    inner: &'static SourceIdInner,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SourceIdInner {
    kind: SourceKind,
    url: Url,
    /// For git sources, the precise commit hash once resolved
    precise: Option<String>,
    /// Original path for path dependencies (for display)
    original_path: Option<PathBuf>,
}

/// The kind of package source.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    /// Local filesystem path
    Path,
    /// Git repository
    Git(GitReference),
    /// Registry (future)
    Registry,
    /// Vcpkg port
    Vcpkg,
}

/// Git reference specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GitReference {
    /// Default branch (usually main/master)
    DefaultBranch,
    /// Specific branch
    Branch(String),
    /// Specific tag
    Tag(String),
    /// Specific revision (commit hash)
    Rev(String),
}

impl Default for GitReference {
    fn default() -> Self {
        GitReference::DefaultBranch
    }
}

impl SourceId {
    /// Create a SourceId for a local path.
    pub fn for_path(path: &Path) -> Result<Self> {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let url = Url::from_file_path(&canonical)
            .map_err(|_| anyhow::anyhow!("invalid path: {}", path.display()))?;

        Self::intern(SourceIdInner {
            kind: SourceKind::Path,
            url,
            precise: None,
            original_path: Some(path.to_path_buf()),
        })
    }

    /// Create a SourceId for a git repository.
    pub fn for_git(url: &Url, reference: GitReference) -> Result<Self> {
        Self::intern(SourceIdInner {
            kind: SourceKind::Git(reference),
            url: url.clone(),
            precise: None,
            original_path: None,
        })
    }

    /// Create a SourceId for a registry.
    pub fn for_registry(url: &Url) -> Result<Self> {
        Self::intern(SourceIdInner {
            kind: SourceKind::Registry,
            url: url.clone(),
            precise: None,
            original_path: None,
        })
    }

    /// Create a SourceId for a vcpkg port.
    ///
    /// # Arguments
    /// * `port` - The vcpkg port name
    /// * `triplet` - Target triplet (e.g., "x64-windows")
    /// * `libs` - Library name overrides
    /// * `features` - Features to enable (e.g., ["wayland", "x11"])
    /// * `baseline` - Baseline commit for reproducibility
    /// * `registry` - Named registry from config (references [vcpkg.registries.NAME])
    pub fn for_vcpkg(
        port: &str,
        triplet: Option<&str>,
        libs: Option<&[String]>,
        features: Option<&[String]>,
        baseline: Option<&str>,
        registry: Option<&str>,
    ) -> Result<Self> {
        let mut url = Url::parse(&format!("vcpkg://{}", port))?;
        let query = build_vcpkg_query(triplet, libs, features, baseline, registry);
        if !query.is_empty() {
            url.set_query(Some(&query));
        }

        Self::intern(SourceIdInner {
            kind: SourceKind::Vcpkg,
            url,
            precise: None,
            original_path: None,
        })
    }

    /// Create a SourceId with a precise commit hash (for lockfiles).
    pub fn with_precise(&self, precise: impl Into<String>) -> Self {
        let mut inner = (*self.inner).clone();
        inner.precise = Some(precise.into());
        Self::intern(inner).expect("re-interning should not fail")
    }

    /// Parse a SourceId from a lockfile URL string.
    ///
    /// Format: `kind+url#precise` or `kind+url?query#precise`
    /// Examples:
    /// - `path+file:///home/user/mylib`
    /// - `git+https://github.com/user/repo?tag=v1.0#abc123`
    pub fn parse(s: &str) -> Result<Self> {
        let (kind_str, rest) = s
            .split_once('+')
            .ok_or_else(|| anyhow::anyhow!("invalid source ID: missing kind prefix"))?;

        let (url_str, precise) = if let Some((u, p)) = rest.rsplit_once('#') {
            (u, Some(p.to_string()))
        } else {
            (rest, None)
        };

        let mut url = Url::parse(url_str)?;

        let kind = match kind_str {
            "path" => SourceKind::Path,
            "git" => {
                // Parse query parameters for git reference
                let reference = if let Some(query) = url.query() {
                    let r = Self::parse_git_reference(query)?;
                    // Clear query string - git reference is stored separately in SourceKind
                    url.set_query(None);
                    r
                } else {
                    GitReference::DefaultBranch
                };
                SourceKind::Git(reference)
            }
            "registry" => SourceKind::Registry,
            "vcpkg" => SourceKind::Vcpkg,
            _ => bail!("unknown source kind: {}", kind_str),
        };

        let original_path = if kind == SourceKind::Path {
            url.to_file_path().ok()
        } else {
            None
        };

        Self::intern(SourceIdInner {
            kind,
            url,
            precise,
            original_path,
        })
    }

    fn parse_git_reference(query: &str) -> Result<GitReference> {
        for param in query.split('&') {
            if let Some((key, value)) = param.split_once('=') {
                match key {
                    "branch" => return Ok(GitReference::Branch(value.to_string())),
                    "tag" => return Ok(GitReference::Tag(value.to_string())),
                    "rev" => return Ok(GitReference::Rev(value.to_string())),
                    _ => {}
                }
            }
        }
        Ok(GitReference::DefaultBranch)
    }

    fn intern(inner: SourceIdInner) -> Result<Self> {
        // Fast path: check if already interned
        {
            let interner = SOURCE_INTERNER.read().unwrap();
            if let Some(&interned) = interner.get(&inner) {
                return Ok(SourceId { inner: interned });
            }
        }

        // Slow path: intern the new source ID
        let mut interner = SOURCE_INTERNER.write().unwrap();

        // Double-check after acquiring write lock
        if let Some(&interned) = interner.get(&inner) {
            return Ok(SourceId { inner: interned });
        }

        let leaked: &'static SourceIdInner = Box::leak(Box::new(inner.clone()));
        interner.insert(inner, leaked);

        Ok(SourceId { inner: leaked })
    }

    /// Get the source kind.
    pub fn kind(&self) -> &SourceKind {
        &self.inner.kind
    }

    /// Get the URL.
    pub fn url(&self) -> &Url {
        &self.inner.url
    }

    /// Get the precise version (git commit hash).
    pub fn precise(&self) -> Option<&str> {
        self.inner.precise.as_deref()
    }

    /// Get the original path for path dependencies.
    pub fn path(&self) -> Option<&Path> {
        self.inner.original_path.as_deref()
    }

    /// Check if this is a path source.
    pub fn is_path(&self) -> bool {
        matches!(self.inner.kind, SourceKind::Path)
    }

    /// Check if this is a git source.
    pub fn is_git(&self) -> bool {
        matches!(self.inner.kind, SourceKind::Git(_))
    }

    /// Check if this is a registry source.
    pub fn is_registry(&self) -> bool {
        matches!(self.inner.kind, SourceKind::Registry)
    }

    /// Check if this is a vcpkg source.
    pub fn is_vcpkg(&self) -> bool {
        matches!(self.inner.kind, SourceKind::Vcpkg)
    }

    /// Get the git reference if this is a git source.
    pub fn git_reference(&self) -> Option<&GitReference> {
        match &self.inner.kind {
            SourceKind::Git(r) => Some(r),
            _ => None,
        }
    }

    /// Convert to a lockfile URL string.
    pub fn to_url_string(&self) -> String {
        let kind_prefix = match &self.inner.kind {
            SourceKind::Path => "path",
            SourceKind::Git(_) => "git",
            SourceKind::Registry => "registry",
            SourceKind::Vcpkg => "vcpkg",
        };

        let mut url = self.inner.url.clone();

        // Add git reference as query params
        if let SourceKind::Git(reference) = &self.inner.kind {
            match reference {
                GitReference::DefaultBranch => {}
                GitReference::Branch(b) => {
                    url.set_query(Some(&format!("branch={}", b)));
                }
                GitReference::Tag(t) => {
                    url.set_query(Some(&format!("tag={}", t)));
                }
                GitReference::Rev(r) => {
                    url.set_query(Some(&format!("rev={}", r)));
                }
            }
        }

        let base = format!("{}+{}", kind_prefix, url);

        if let Some(ref precise) = self.inner.precise {
            format!("{}#{}", base, precise)
        } else {
            base
        }
    }

    /// Get a stable hash for caching purposes.
    pub fn stable_hash(&self) -> u64 {
        use std::hash::DefaultHasher;
        let mut hasher = DefaultHasher::new();
        self.inner.hash(&mut hasher);
        hasher.finish()
    }

    /// Get the vcpkg port name, if applicable.
    pub fn vcpkg_port(&self) -> Option<&str> {
        if !self.is_vcpkg() {
            return None;
        }
        self.inner.url.host_str().or_else(|| {
            self.inner
                .url
                .path()
                .trim_start_matches('/')
                .split('/')
                .next()
        })
    }

    /// Get the vcpkg triplet, if specified.
    pub fn vcpkg_triplet(&self) -> Option<String> {
        if !self.is_vcpkg() {
            return None;
        }
        parse_vcpkg_query(&self.inner.url).triplet
    }

    /// Get the vcpkg library list override, if specified.
    pub fn vcpkg_libs(&self) -> Option<Vec<String>> {
        if !self.is_vcpkg() {
            return None;
        }
        parse_vcpkg_query(&self.inner.url).libs
    }

    /// Get the vcpkg features to enable, if specified.
    pub fn vcpkg_features(&self) -> Option<Vec<String>> {
        if !self.is_vcpkg() {
            return None;
        }
        parse_vcpkg_query(&self.inner.url).features
    }

    /// Get the vcpkg baseline commit, if specified.
    pub fn vcpkg_baseline(&self) -> Option<String> {
        if !self.is_vcpkg() {
            return None;
        }
        parse_vcpkg_query(&self.inner.url).baseline
    }

    /// Get the vcpkg registry name, if specified.
    ///
    /// This references a named registry defined in [vcpkg.registries.NAME].
    pub fn vcpkg_registry(&self) -> Option<String> {
        if !self.is_vcpkg() {
            return None;
        }
        parse_vcpkg_query(&self.inner.url).registry
    }
}

impl PartialEq for SourceId {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.inner, other.inner)
    }
}

impl Eq for SourceId {}

impl Hash for SourceId {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::ptr::hash(self.inner, state)
    }
}

impl fmt::Debug for SourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SourceId")
            .field("kind", &self.inner.kind)
            .field("url", &self.inner.url.as_str())
            .field("precise", &self.inner.precise)
            .finish()
    }
}

impl fmt::Display for SourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.inner.kind {
            SourceKind::Path => {
                if let Some(path) = &self.inner.original_path {
                    write!(f, "{}", path.display())
                } else {
                    write!(f, "{}", self.inner.url)
                }
            }
            SourceKind::Git(reference) => {
                write!(f, "{}", self.inner.url)?;
                match reference {
                    GitReference::DefaultBranch => {}
                    GitReference::Branch(b) => write!(f, "?branch={}", b)?,
                    GitReference::Tag(t) => write!(f, "?tag={}", t)?,
                    GitReference::Rev(r) => write!(f, "?rev={}", r)?,
                }
                if let Some(ref precise) = self.inner.precise {
                    write!(f, "#{}", &precise[..8.min(precise.len())])?;
                }
                Ok(())
            }
            SourceKind::Registry => {
                write!(f, "registry+{}", self.inner.url)
            }
            SourceKind::Vcpkg => {
                let port = self.vcpkg_port().unwrap_or("unknown");
                if let Some(triplet) = self.vcpkg_triplet() {
                    write!(f, "vcpkg:{}:{}", port, triplet)
                } else {
                    write!(f, "vcpkg:{}", port)
                }
            }
        }
    }
}

fn build_vcpkg_query(
    triplet: Option<&str>,
    libs: Option<&[String]>,
    features: Option<&[String]>,
    baseline: Option<&str>,
    registry: Option<&str>,
) -> String {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    if let Some(triplet) = triplet {
        serializer.append_pair("triplet", triplet);
    }
    if let Some(libs) = libs {
        if !libs.is_empty() {
            serializer.append_pair("libs", &libs.join(","));
        }
    }
    if let Some(features) = features {
        if !features.is_empty() {
            serializer.append_pair("features", &features.join(","));
        }
    }
    if let Some(baseline) = baseline {
        serializer.append_pair("baseline", baseline);
    }
    if let Some(registry) = registry {
        serializer.append_pair("registry", registry);
    }
    serializer.finish()
}

/// Parsed vcpkg query parameters.
struct VcpkgQueryParams {
    triplet: Option<String>,
    libs: Option<Vec<String>>,
    features: Option<Vec<String>>,
    baseline: Option<String>,
    registry: Option<String>,
}

fn parse_vcpkg_query(url: &Url) -> VcpkgQueryParams {
    let mut params = VcpkgQueryParams {
        triplet: None,
        libs: None,
        features: None,
        baseline: None,
        registry: None,
    };

    if let Some(query) = url.query() {
        for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
            match key.as_ref() {
                "triplet" => params.triplet = Some(value.into_owned()),
                "libs" => {
                    let list = value
                        .split(',')
                        .filter(|item| !item.is_empty())
                        .map(|item| item.to_string())
                        .collect::<Vec<_>>();
                    if !list.is_empty() {
                        params.libs = Some(list);
                    }
                }
                "features" => {
                    let list = value
                        .split(',')
                        .filter(|item| !item.is_empty())
                        .map(|item| item.to_string())
                        .collect::<Vec<_>>();
                    if !list.is_empty() {
                        params.features = Some(list);
                    }
                }
                "baseline" => params.baseline = Some(value.into_owned()),
                "registry" => params.registry = Some(value.into_owned()),
                _ => {}
            }
        }
    }

    params
}

impl Serialize for SourceId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.to_url_string().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SourceId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        SourceId::parse(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_path_source_id() {
        let tmp = TempDir::new().unwrap();
        let id1 = SourceId::for_path(tmp.path()).unwrap();
        let id2 = SourceId::for_path(tmp.path()).unwrap();

        // Same path should yield same interned ID
        assert_eq!(id1, id2);
        assert!(std::ptr::eq(id1.inner, id2.inner));
        assert!(id1.is_path());
    }

    #[test]
    fn test_git_source_id() {
        let url = Url::parse("https://github.com/user/repo").unwrap();
        let id1 = SourceId::for_git(&url, GitReference::Tag("v1.0".into())).unwrap();
        let id2 = SourceId::for_git(&url, GitReference::Tag("v1.0".into())).unwrap();

        assert_eq!(id1, id2);
        assert!(id1.is_git());
        assert_eq!(id1.git_reference(), Some(&GitReference::Tag("v1.0".into())));
    }

    #[test]
    fn test_source_id_parsing() {
        let url_str = "git+https://github.com/user/repo?tag=v1.0#abc123def456";
        let id = SourceId::parse(url_str).unwrap();

        assert!(id.is_git());
        assert_eq!(id.precise(), Some("abc123def456"));
        assert_eq!(id.git_reference(), Some(&GitReference::Tag("v1.0".into())));
    }

    #[test]
    fn test_source_id_roundtrip() {
        let url = Url::parse("https://github.com/user/repo").unwrap();
        let id = SourceId::for_git(&url, GitReference::Branch("main".into()))
            .unwrap()
            .with_precise("deadbeef");

        let url_str = id.to_url_string();
        let parsed = SourceId::parse(&url_str).unwrap();

        // Should have same values (but may not be pointer-equal due to re-interning)
        assert_eq!(parsed.url(), id.url());
        assert_eq!(parsed.precise(), id.precise());
    }

    #[test]
    fn test_vcpkg_source_id() {
        let id = SourceId::for_vcpkg(
            "glfw3",
            Some("x64-windows"),
            Some(&["glfw".to_string()]),
            None,
            None,
            None,
        )
        .unwrap();

        assert!(id.is_vcpkg());
        assert_eq!(id.vcpkg_port(), Some("glfw3"));
        assert_eq!(id.vcpkg_triplet(), Some("x64-windows".to_string()));
        assert_eq!(id.vcpkg_libs(), Some(vec!["glfw".to_string()]));

        let url_str = id.to_url_string();
        let parsed = SourceId::parse(&url_str).unwrap();
        assert!(parsed.is_vcpkg());
        assert_eq!(parsed.vcpkg_port(), Some("glfw3"));
        assert_eq!(parsed.vcpkg_triplet(), Some("x64-windows".to_string()));
        assert_eq!(parsed.vcpkg_libs(), Some(vec!["glfw".to_string()]));
    }

    #[test]
    fn test_vcpkg_source_id_with_features_and_baseline() {
        let features = vec!["wayland".to_string(), "x11".to_string()];
        let id = SourceId::for_vcpkg(
            "glfw3",
            Some("x64-linux"),
            None,
            Some(&features),
            Some("abc123"),
            None,
        )
        .unwrap();

        assert!(id.is_vcpkg());
        assert_eq!(id.vcpkg_port(), Some("glfw3"));
        assert_eq!(id.vcpkg_triplet(), Some("x64-linux".to_string()));
        assert_eq!(id.vcpkg_libs(), None);
        assert_eq!(
            id.vcpkg_features(),
            Some(vec!["wayland".to_string(), "x11".to_string()])
        );
        assert_eq!(id.vcpkg_baseline(), Some("abc123".to_string()));

        // Test round-trip
        let url_str = id.to_url_string();
        let parsed = SourceId::parse(&url_str).unwrap();
        assert!(parsed.is_vcpkg());
        assert_eq!(parsed.vcpkg_port(), Some("glfw3"));
        assert_eq!(parsed.vcpkg_triplet(), Some("x64-linux".to_string()));
        assert_eq!(
            parsed.vcpkg_features(),
            Some(vec!["wayland".to_string(), "x11".to_string()])
        );
        assert_eq!(parsed.vcpkg_baseline(), Some("abc123".to_string()));
    }

    #[test]
    fn test_vcpkg_source_id_with_registry() {
        let id = SourceId::for_vcpkg(
            "mylib",
            None,
            None,
            None,
            None,
            Some("internal"),
        )
        .unwrap();

        assert!(id.is_vcpkg());
        assert_eq!(id.vcpkg_port(), Some("mylib"));
        assert_eq!(id.vcpkg_registry(), Some("internal".to_string()));

        // Test round-trip
        let url_str = id.to_url_string();
        let parsed = SourceId::parse(&url_str).unwrap();
        assert_eq!(parsed.vcpkg_registry(), Some("internal".to_string()));
    }
}
