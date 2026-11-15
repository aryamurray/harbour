//! `static` / `shared` / `auto`: the linkage a command line or config asks for.
//!
//! Parsed from `--linkage` (on `harbour build`) and `[build] linkage` in
//! config, and carried in [`BuildOptions`](crate::ops::harbour_build::BuildOptions).

/// Linkage preference from user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkagePreference {
    /// Prefer static linking
    Static,
    /// Prefer shared/dynamic linking
    Shared,
    /// No preference, with fallback order
    Auto {
        /// Preferred artifact kinds in order of preference
        prefer: Vec<ArtifactKind>,
    },
}

impl Default for LinkagePreference {
    fn default() -> Self {
        // Default to auto with preference for static
        LinkagePreference::Auto {
            prefer: vec![ArtifactKind::Static, ArtifactKind::Shared],
        }
    }
}

impl LinkagePreference {
    /// Create a static linkage preference.
    pub fn static_() -> Self {
        LinkagePreference::Static
    }

    /// Create a shared linkage preference.
    pub fn shared() -> Self {
        LinkagePreference::Shared
    }

    /// Create an auto preference with static preferred.
    pub fn auto_prefer_static() -> Self {
        LinkagePreference::Auto {
            prefer: vec![ArtifactKind::Static, ArtifactKind::Shared],
        }
    }

    /// Create an auto preference with shared preferred.
    pub fn auto_prefer_shared() -> Self {
        LinkagePreference::Auto {
            prefer: vec![ArtifactKind::Shared, ArtifactKind::Static],
        }
    }

    /// Check if this preference allows static linking.
    pub fn allows_static(&self) -> bool {
        match self {
            LinkagePreference::Static => true,
            LinkagePreference::Shared => false,
            LinkagePreference::Auto { prefer } => prefer.contains(&ArtifactKind::Static),
        }
    }

    /// Check if this preference allows shared linking.
    pub fn allows_shared(&self) -> bool {
        match self {
            LinkagePreference::Static => false,
            LinkagePreference::Shared => true,
            LinkagePreference::Auto { prefer } => prefer.contains(&ArtifactKind::Shared),
        }
    }

    /// Check if this preference requires shared linking (for FFI).
    pub fn requires_shared(&self) -> bool {
        matches!(self, LinkagePreference::Shared)
    }
}

impl std::str::FromStr for LinkagePreference {
    type Err = LinkagePreferenceParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "static" => Ok(LinkagePreference::Static),
            "shared" | "dynamic" => Ok(LinkagePreference::Shared),
            "auto" => Ok(LinkagePreference::default()),
            _ => Err(LinkagePreferenceParseError(s.to_string())),
        }
    }
}

/// Error returned when parsing an invalid linkage preference.
#[derive(Debug, Clone)]
pub struct LinkagePreferenceParseError(pub String);

impl std::fmt::Display for LinkagePreferenceParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "invalid linkage preference '{}', valid values: static, shared, auto",
            self.0
        )
    }
}

impl std::error::Error for LinkagePreferenceParseError {}

/// Artifact kind for linkage preference ordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArtifactKind {
    /// Static library (.a / .lib)
    Static,
    /// Shared library (.so / .dylib / .dll)
    Shared,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_linkage_preference_parse() {
        assert!(matches!(
            "static".parse::<LinkagePreference>().unwrap(),
            LinkagePreference::Static
        ));
        assert!(matches!(
            "shared".parse::<LinkagePreference>().unwrap(),
            LinkagePreference::Shared
        ));
        assert!(matches!(
            "dynamic".parse::<LinkagePreference>().unwrap(),
            LinkagePreference::Shared
        ));
        assert!(matches!(
            "auto".parse::<LinkagePreference>().unwrap(),
            LinkagePreference::Auto { .. }
        ));
        assert!("invalid".parse::<LinkagePreference>().is_err());
    }

    #[test]
    fn test_linkage_allows() {
        assert!(LinkagePreference::Static.allows_static());
        assert!(!LinkagePreference::Static.allows_shared());
        assert!(!LinkagePreference::Shared.allows_static());
        assert!(LinkagePreference::Shared.allows_shared());
        assert!(LinkagePreference::default().allows_static());
        assert!(LinkagePreference::default().allows_shared());
    }
}
