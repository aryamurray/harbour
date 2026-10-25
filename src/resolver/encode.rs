//! Lockfile encoding and decoding.
//!
//! Harbour.lock is the canonical lockfile format for Harbour.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::core::{PackageId, SourceId, Summary};
use crate::resolver::resolve::Resolve;
use crate::util::InternedString;

/// Lockfile representation for serialization.
#[derive(Debug, Serialize, Deserialize)]
pub struct Lockfile {
    /// Lockfile format version
    pub version: u32,

    /// Hash of the root manifest's resolution-affecting fields.
    /// Used for content-based freshness detection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_manifest_hash: Option<String>,

    /// Hashes of workspace member manifests (for multi-package workspaces).
    /// Each entry is (relative_path, hash).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub member_manifest_hashes: Vec<MemberManifestHash>,

    /// Locked packages
    #[serde(rename = "package", default)]
    pub packages: Vec<LockedPackage>,
}

/// Hash entry for a workspace member manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberManifestHash {
    /// Relative path from workspace root
    pub path: String,
    /// Hash of resolution-affecting fields
    pub hash: String,
}

/// A locked package entry.
#[derive(Debug, Serialize, Deserialize)]
pub struct LockedPackage {
    /// Package name
    pub name: String,

    /// Exact version
    pub version: String,

    /// Source URL (path+, git+, registry+)
    pub source: String,

    /// Optional checksum
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checksum: Option<String>,

    /// Dependencies (name version pairs)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<String>,

    /// Registry provenance (only for registry sources)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry_provenance: Option<RegistryProvenance>,

    /// The source files each target of this package compiles, by target name:
    /// the target's unconditional `sources` globs expanded against the fetched
    /// tree, minus its `exclude`, as sorted package-relative paths with `/`
    /// separators. Recorded only for fetched (git and registry) packages; see
    /// `ops::source_lock`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sources: BTreeMap<String, Vec<String>>,

    /// The version `sources` was recorded for, present only while that is
    /// *not* this entry's `version`: a re-resolution moved the package, and
    /// the lists are the old version's until the next build compares them
    /// with the new tree's and replaces them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sources_from: Option<String>,
}

impl LockedPackage {
    /// The `source` string a package is locked under.
    ///
    /// One function, because the lock is written from a `Resolve` and found
    /// again from a `PackageId`, and the two must agree on the spelling.
    pub fn source_string(pkg_id: &PackageId) -> String {
        pkg_id.source_id().to_url_string()
    }
}

/// Registry-specific provenance information for reproducibility.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryProvenance {
    /// Path to shim file in registry (e.g., "z/zlib/1.3.1.toml")
    pub shim_path: String,

    /// SHA256 hash of shim file content
    pub shim_hash: String,

    /// Resolved source information
    pub resolved: ResolvedSource,
}

/// The actual source that was fetched (resolved from shim).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ResolvedSource {
    /// Git source
    #[serde(rename = "git")]
    Git {
        /// Git repository URL
        url: String,
        /// Full commit SHA
        rev: String,
    },
    /// Tarball source
    #[serde(rename = "tarball")]
    Tarball {
        /// Download URL
        url: String,
        /// SHA256 hash
        sha256: String,
    },
}

impl Lockfile {
    /// Create a new lockfile from a Resolve.
    pub fn from_resolve(resolve: &Resolve) -> Self {
        let mut packages: Vec<LockedPackage> = resolve
            .packages()
            .map(|(pkg_id, summary)| {
                let deps: Vec<String> = summary
                    .dependencies()
                    .iter()
                    .filter_map(|dep| {
                        resolve
                            .get_package_by_name(dep.name())
                            .map(|id| format!("{} {}", id.name(), id.version()))
                    })
                    .collect();

                LockedPackage {
                    name: pkg_id.name().to_string(),
                    version: pkg_id.version().to_string(),
                    source: LockedPackage::source_string(pkg_id),
                    checksum: resolve.checksum(*pkg_id).map(|s| s.to_string()),
                    dependencies: deps,
                    registry_provenance: resolve.registry_provenance(*pkg_id),
                    sources: BTreeMap::new(),
                    sources_from: None,
                }
            })
            .collect();

        // Sort for deterministic output
        packages.sort_by(|a, b| a.name.cmp(&b.name).then(a.version.cmp(&b.version)));

        Lockfile {
            version: 1,
            root_manifest_hash: None,
            member_manifest_hashes: Vec::new(),
            packages,
        }
    }

    /// Carry the recorded source lists of `old` over to this lockfile.
    ///
    /// A lockfile is rewritten from a fresh `Resolve`, which knows nothing about
    /// the files a build found; without this a re-resolution would forget every
    /// list. A package whose version and source are unchanged keeps its lists as
    /// they are. One that moved keeps the *old* lists, marked with the version
    /// they belong to (`sources_from`), so the next build can say what the move
    /// did to the file set instead of recording the new one in silence.
    pub fn carry_sources_from(&mut self, old: &Lockfile) {
        for pkg in &mut self.packages {
            let exact = old
                .packages
                .iter()
                .find(|o| o.name == pkg.name && o.version == pkg.version && o.source == pkg.source);
            if let Some(o) = exact {
                pkg.sources = o.sources.clone();
                pkg.sources_from = o.sources_from.clone();
                continue;
            }
            let mut same_name = old.packages.iter().filter(|o| o.name == pkg.name);
            if let (Some(o), None) = (same_name.next(), same_name.next()) {
                if !o.sources.is_empty() {
                    pkg.sources = o.sources.clone();
                    pkg.sources_from = Some(o.sources_from.clone().unwrap_or(o.version.clone()));
                }
            }
        }
    }

    /// Set the root manifest hash for content-based freshness detection.
    pub fn with_manifest_hash(mut self, hash: String) -> Self {
        self.root_manifest_hash = Some(hash);
        self
    }

    /// Set the member manifest hashes for workspace support.
    pub fn with_member_hashes(mut self, hashes: Vec<MemberManifestHash>) -> Self {
        self.member_manifest_hashes = hashes;
        self
    }

    /// Get the root manifest hash, if present.
    pub fn manifest_hash(&self) -> Option<&str> {
        self.root_manifest_hash.as_deref()
    }

    /// Load a lockfile from a path.
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read lockfile: {}", path.display()))?;

        toml::from_str(&content).with_context(|| "failed to parse lockfile")
    }

    /// Save the lockfile to a path.
    pub fn save(&self, path: &Path) -> Result<()> {
        let content = toml::to_string_pretty(self)?;

        // Add header comment
        let with_header = format!(
            "# This file is automatically generated by Harbour.\n\
             # It is not intended for manual editing.\n\n\
             {content}"
        );

        std::fs::write(path, with_header)
            .with_context(|| format!("failed to write lockfile: {}", path.display()))?;

        Ok(())
    }

    /// Convert to a Resolve.
    ///
    /// Note: This creates a partial Resolve without full summaries.
    /// The full summaries need to be loaded from sources.
    pub fn to_resolve(&self) -> Result<Resolve> {
        let mut resolve = Resolve::new();

        // First pass: add all packages
        for pkg in &self.packages {
            let source_id = SourceId::parse(&pkg.source)?;
            let version = pkg.version.parse()?;
            let pkg_id = PackageId::new(&pkg.name, version, source_id);

            // Create a minimal summary (deps will be incomplete)
            let summary = Summary::new(pkg_id, vec![], pkg.checksum.clone());
            resolve.add_package(pkg_id, summary);
        }

        // Second pass: add dependency edges
        for pkg in &self.packages {
            let source_id = SourceId::parse(&pkg.source)?;
            let version = pkg.version.parse()?;
            let pkg_id = PackageId::new(&pkg.name, version, source_id);

            for dep_str in &pkg.dependencies {
                let parts: Vec<&str> = dep_str.split_whitespace().collect();
                if parts.len() >= 2 {
                    if let Some(dep_id) = resolve.get_package_by_name(InternedString::new(parts[0]))
                    {
                        resolve.add_edge(pkg_id, dep_id);
                    }
                }
            }
        }

        Ok(resolve)
    }

    /// Check if the lockfile is compatible with the given version.
    pub fn is_compatible(&self) -> bool {
        self.version == 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use semver::Version;
    use tempfile::TempDir;

    #[test]
    fn test_lockfile_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let source = SourceId::for_path(tmp.path()).unwrap();

        let mut resolve = Resolve::new();

        let pkg_id = PackageId::new("test", Version::new(1, 0, 0), source);
        let summary = Summary::new(pkg_id, vec![], Some("abc123".into()));
        resolve.add_package(pkg_id, summary);

        let lockfile = Lockfile::from_resolve(&resolve);

        // Save and reload
        let lock_path = tmp.path().join("Harbour.lock");
        lockfile.save(&lock_path).unwrap();

        let loaded = Lockfile::load(&lock_path).unwrap();
        assert_eq!(loaded.packages.len(), 1);
        assert_eq!(loaded.packages[0].name, "test");
        assert_eq!(loaded.packages[0].version, "1.0.0");
    }

    #[test]
    fn test_lockfile_format() {
        let lockfile = Lockfile {
            version: 1,
            root_manifest_hash: None,
            member_manifest_hashes: vec![],
            packages: vec![LockedPackage {
                name: "test".to_string(),
                version: "1.0.0".to_string(),
                source: "path+file:///test".to_string(),
                checksum: Some("sha256:abc".to_string()),
                dependencies: vec!["dep 2.0.0".to_string()],
                registry_provenance: None,
                sources: BTreeMap::new(),
                sources_from: None,
            }],
        };

        let toml = toml::to_string_pretty(&lockfile).unwrap();
        assert!(toml.contains("version = 1"));
        assert!(toml.contains("[[package]]"));
    }
}
