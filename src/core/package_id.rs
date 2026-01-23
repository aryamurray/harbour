//! Package identification - WHAT package (name + version + source).
//!
//! PackageId uniquely identifies a specific package instance.
//! It's interned for cheap comparison and cloning.

use std::collections::HashMap;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::{LazyLock, RwLock};

use semver::Version;
use serde::{Deserialize, Serialize};

use crate::core::SourceId;
use crate::util::InternedString;

/// Global package ID interner
static PACKAGE_INTERNER: LazyLock<RwLock<HashMap<PackageIdInner, &'static PackageIdInner>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// A unique identifier for a package (interned).
///
/// PackageIds are cheap to clone and compare (pointer comparison).
/// They combine name, version, and source to uniquely identify a package instance.
#[derive(Clone, Copy)]
pub struct PackageId {
    inner: &'static PackageIdInner,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PackageIdInner {
    name: InternedString,
    version: Version,
    source_id: SourceId,
}

impl PackageId {
    /// Create a new package ID.
    pub fn new(name: impl Into<InternedString>, version: Version, source_id: SourceId) -> Self {
        let inner = PackageIdInner {
            name: name.into(),
            version,
            source_id,
        };

        Self::intern(inner)
    }

    fn intern(inner: PackageIdInner) -> Self {
        // Fast path: check if already interned
        {
            let interner = PACKAGE_INTERNER.read().unwrap();
            if let Some(&interned) = interner.get(&inner) {
                return PackageId { inner: interned };
            }
        }

        // Slow path: intern the new package ID
        let mut interner = PACKAGE_INTERNER.write().unwrap();

        // Double-check after acquiring write lock
        if let Some(&interned) = interner.get(&inner) {
            return PackageId { inner: interned };
        }

        let leaked: &'static PackageIdInner = Box::leak(Box::new(inner.clone()));
        interner.insert(inner, leaked);

        PackageId { inner: leaked }
    }

    /// Get the package name.
    pub fn name(&self) -> InternedString {
        self.inner.name
    }

    /// Get the package version.
    pub fn version(&self) -> &Version {
        &self.inner.version
    }

    /// Get the source ID.
    pub fn source_id(&self) -> SourceId {
        self.inner.source_id
    }

    /// Get a display string like "name v1.2.3"
    pub fn display_name(&self) -> String {
        format!("{} v{}", self.inner.name, self.inner.version)
    }

    /// Get a stable hash for caching purposes.
    pub fn stable_hash(&self) -> u64 {
        use std::hash::DefaultHasher;
        let mut hasher = DefaultHasher::new();
        self.inner.name.as_str().hash(&mut hasher);
        self.inner.version.to_string().hash(&mut hasher);
        self.inner.source_id.stable_hash().hash(&mut hasher);
        hasher.finish()
    }
}

impl PartialEq for PackageId {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.inner, other.inner)
    }
}

impl Eq for PackageId {}

impl Hash for PackageId {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::ptr::hash(self.inner, state)
    }
}

impl PartialOrd for PackageId {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PackageId {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.inner
            .name
            .cmp(&other.inner.name)
            .then_with(|| self.inner.version.cmp(&other.inner.version))
    }
}

impl fmt::Debug for PackageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PackageId")
            .field("name", &self.inner.name.as_str())
            .field("version", &self.inner.version)
            .field("source", &self.inner.source_id)
            .finish()
    }
}

impl fmt::Display for PackageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} v{} ({})",
            self.inner.name, self.inner.version, self.inner.source_id
        )
    }
}

impl Serialize for PackageId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // Serialize as a struct for lockfile
        #[derive(Serialize)]
        struct PackageIdData<'a> {
            name: &'a str,
            version: String,
            source: String,
        }

        let data = PackageIdData {
            name: self.inner.name.as_str(),
            version: self.inner.version.to_string(),
            source: self.inner.source_id.to_url_string(),
        };

        data.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for PackageId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct PackageIdData {
            name: String,
            version: String,
            source: String,
        }

        let data = PackageIdData::deserialize(deserializer)?;
        let version = data.version.parse().map_err(serde::de::Error::custom)?;
        let source_id = SourceId::parse(&data.source).map_err(serde::de::Error::custom)?;

        Ok(PackageId::new(data.name, version, source_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_package_id_interning() {
        let tmp = TempDir::new().unwrap();
        let source = SourceId::for_path(tmp.path()).unwrap();

        let id1 = PackageId::new("mylib", Version::new(1, 0, 0), source);
        let id2 = PackageId::new("mylib", Version::new(1, 0, 0), source);

        assert_eq!(id1, id2);
        assert!(std::ptr::eq(id1.inner, id2.inner));
    }

    #[test]
    fn test_package_id_different_versions() {
        let tmp = TempDir::new().unwrap();
        let source = SourceId::for_path(tmp.path()).unwrap();

        let id1 = PackageId::new("mylib", Version::new(1, 0, 0), source);
        let id2 = PackageId::new("mylib", Version::new(2, 0, 0), source);

        assert_ne!(id1, id2);
    }

    #[test]
    fn test_package_id_ordering() {
        let tmp = TempDir::new().unwrap();
        let source = SourceId::for_path(tmp.path()).unwrap();

        let id1 = PackageId::new("aaa", Version::new(1, 0, 0), source);
        let id2 = PackageId::new("bbb", Version::new(1, 0, 0), source);
        let id3 = PackageId::new("aaa", Version::new(2, 0, 0), source);

        assert!(id1 < id2);
        assert!(id1 < id3);
    }

    #[test]
    fn test_display() {
        let tmp = TempDir::new().unwrap();
        let source = SourceId::for_path(tmp.path()).unwrap();
        let id = PackageId::new("mylib", Version::new(1, 2, 3), source);

        assert_eq!(id.display_name(), "mylib v1.2.3");
    }
}
