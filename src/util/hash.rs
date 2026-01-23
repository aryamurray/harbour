//! Hashing utilities for checksums and fingerprinting.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

/// Compute SHA256 hash of a byte slice.
pub fn sha256_bytes(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

/// Compute SHA256 hash of a string.
pub fn sha256_str(s: &str) -> String {
    sha256_bytes(s.as_bytes())
}

/// Compute SHA256 hash of a file.
pub fn sha256_file(path: &Path) -> Result<String> {
    let file = File::open(path)
        .with_context(|| format!("failed to open file for hashing: {}", path.display()))?;

    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];

    loop {
        let bytes_read = reader.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }

    Ok(hex::encode(hasher.finalize()))
}

/// A hasher for building fingerprints from multiple components.
#[derive(Default)]
pub struct Fingerprint {
    hasher: Sha256,
}

impl Fingerprint {
    /// Create a new fingerprint builder.
    pub fn new() -> Self {
        Fingerprint {
            hasher: Sha256::new(),
        }
    }

    /// Add a string component to the fingerprint.
    pub fn update_str(&mut self, s: &str) -> &mut Self {
        self.hasher.update(s.as_bytes());
        self.hasher.update(b"\0"); // Separator
        self
    }

    /// Add multiple strings to the fingerprint.
    pub fn update_strs<'a>(&mut self, items: impl IntoIterator<Item = &'a str>) -> &mut Self {
        for s in items {
            self.update_str(s);
        }
        self
    }

    /// Add an optional string component.
    pub fn update_opt(&mut self, opt: Option<&str>) -> &mut Self {
        match opt {
            Some(s) => {
                self.hasher.update(b"\x01"); // Present marker
                self.update_str(s);
            }
            None => {
                self.hasher.update(b"\x00"); // Absent marker
            }
        }
        self
    }

    /// Add a boolean component.
    pub fn update_bool(&mut self, b: bool) -> &mut Self {
        self.hasher.update(&[b as u8]);
        self
    }

    /// Finalize and return the fingerprint as a hex string.
    pub fn finish(self) -> String {
        hex::encode(self.hasher.finalize())
    }

    /// Finalize and return a short fingerprint (first 16 chars).
    pub fn finish_short(self) -> String {
        self.finish()[..16].to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_sha256_str() {
        let hash = sha256_str("hello");
        assert_eq!(
            hash,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn test_sha256_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("test.txt");
        std::fs::write(&path, "hello").unwrap();

        let hash = sha256_file(&path).unwrap();
        assert_eq!(
            hash,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn test_fingerprint() {
        let fp1 = {
            let mut fp = Fingerprint::new();
            fp.update_str("hello").update_str("world");
            fp.finish()
        };

        let fp2 = {
            let mut fp = Fingerprint::new();
            fp.update_str("hello").update_str("world");
            fp.finish()
        };

        let fp3 = {
            let mut fp = Fingerprint::new();
            fp.update_str("hello").update_str("different");
            fp.finish()
        };

        assert_eq!(fp1, fp2);
        assert_ne!(fp1, fp3);
    }
}
