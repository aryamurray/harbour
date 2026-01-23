//! String interning for efficient identifier storage and comparison.
//!
//! InternedString provides O(1) equality checks and zero-cost cloning
//! by storing all strings in a global interner.

use std::borrow::Borrow;
use std::cmp::Ordering;
use std::collections::HashSet;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::sync::{LazyLock, RwLock};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Global string interner
static INTERNER: LazyLock<RwLock<HashSet<&'static str>>> =
    LazyLock::new(|| RwLock::new(HashSet::new()));

/// An interned string that provides O(1) equality and zero-cost cloning.
///
/// All InternedStrings with the same content point to the same memory location,
/// making equality checks a simple pointer comparison.
#[derive(Clone, Copy)]
pub struct InternedString {
    inner: &'static str,
}

impl InternedString {
    /// Create a new interned string from any string-like type.
    pub fn new(s: impl AsRef<str>) -> Self {
        let s = s.as_ref();

        // Fast path: check if already interned (read lock only)
        {
            let interner = INTERNER.read().unwrap();
            if let Some(&interned) = interner.get(s) {
                return InternedString { inner: interned };
            }
        }

        // Slow path: need to intern (write lock)
        let mut interner = INTERNER.write().unwrap();

        // Double-check after acquiring write lock
        if let Some(&interned) = interner.get(s) {
            return InternedString { inner: interned };
        }

        // Leak the string to get a 'static reference
        let leaked: &'static str = Box::leak(s.to_string().into_boxed_str());
        interner.insert(leaked);

        InternedString { inner: leaked }
    }

    /// Get the underlying string slice.
    #[inline]
    pub fn as_str(&self) -> &'static str {
        self.inner
    }

    /// Check if the string is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Get the length of the string.
    #[inline]
    pub fn len(&self) -> usize {
        self.inner.len()
    }
}

impl Default for InternedString {
    fn default() -> Self {
        InternedString::new("")
    }
}

impl Deref for InternedString {
    type Target = str;

    #[inline]
    fn deref(&self) -> &str {
        self.inner
    }
}

impl AsRef<str> for InternedString {
    #[inline]
    fn as_ref(&self) -> &str {
        self.inner
    }
}

impl Borrow<str> for InternedString {
    #[inline]
    fn borrow(&self) -> &str {
        self.inner
    }
}

impl PartialEq for InternedString {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        // Pointer comparison - the magic of interning!
        std::ptr::eq(self.inner, other.inner)
    }
}

impl Eq for InternedString {}

impl PartialOrd for InternedString {
    #[inline]
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for InternedString {
    #[inline]
    fn cmp(&self, other: &Self) -> Ordering {
        self.inner.cmp(other.inner)
    }
}

impl Hash for InternedString {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Hash the pointer address for speed (all equal strings have same address)
        std::ptr::hash(self.inner, state)
    }
}

impl fmt::Debug for InternedString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.inner, f)
    }
}

impl fmt::Display for InternedString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self.inner, f)
    }
}

impl From<&str> for InternedString {
    fn from(s: &str) -> Self {
        InternedString::new(s)
    }
}

impl From<String> for InternedString {
    fn from(s: String) -> Self {
        InternedString::new(s)
    }
}

impl From<&String> for InternedString {
    fn from(s: &String) -> Self {
        InternedString::new(s)
    }
}

impl Serialize for InternedString {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.inner.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for InternedString {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(InternedString::new(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_interning_equality() {
        let a = InternedString::new("hello");
        let b = InternedString::new("hello");
        let c = InternedString::new("world");

        assert_eq!(a, b);
        assert_ne!(a, c);

        // Verify they point to the same memory
        assert!(std::ptr::eq(a.inner, b.inner));
    }

    #[test]
    fn test_hash_consistency() {
        use std::collections::HashMap;

        let mut map = HashMap::new();
        let key = InternedString::new("test");
        map.insert(key, 42);

        let lookup = InternedString::new("test");
        assert_eq!(map.get(&lookup), Some(&42));
    }

    #[test]
    fn test_clone_is_cheap() {
        let original = InternedString::new("some long string that would be expensive to clone");
        let cloned = original;

        // Both point to the same memory
        assert!(std::ptr::eq(original.inner, cloned.inner));
    }
}
