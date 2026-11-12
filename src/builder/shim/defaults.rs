//! Backend defaults and policy preferences.
//!
//! Defaults are soft preferences that can change without "capabilities changed".
//! Hard constraints belong in `capabilities.rs`.

use std::collections::HashMap;

use crate::builder::shim::capabilities::InjectionMethod;
use crate::core::manifest::Profile;

/// Build profile kind (matches CLI --release / default debug).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ProfileKind {
    /// Debug build (unoptimized, with debug info)
    #[default]
    Debug,
    /// Release build (optimized, minimal debug info)
    Release,
    /// Custom profile by name
    Custom(&'static str),
}

impl ProfileKind {
    /// Get the profile name as a string.
    pub fn as_str(&self) -> &str {
        match self {
            ProfileKind::Debug => "debug",
            ProfileKind::Release => "release",
            ProfileKind::Custom(name) => name,
        }
    }
}

impl std::fmt::Display for ProfileKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Sanitizer kinds supported by toolchains.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SanitizerKind {
    /// Address sanitizer (memory errors)
    Address,
    /// Thread sanitizer (data races)
    Thread,
    /// Memory sanitizer (uninitialized reads)
    Memory,
    /// Undefined behavior sanitizer
    UndefinedBehavior,
    /// Leak sanitizer
    Leak,
}

impl SanitizerKind {
    /// Get the sanitizer name as a string.
    pub fn as_str(&self) -> &'static str {
        match self {
            SanitizerKind::Address => "address",
            SanitizerKind::Thread => "thread",
            SanitizerKind::Memory => "memory",
            SanitizerKind::UndefinedBehavior => "undefined",
            SanitizerKind::Leak => "leak",
        }
    }
}

/// Backend defaults/policy.
///
/// These are preferences that affect behavior but are not hard constraints.
/// Changing defaults doesn't change what the backend CAN do.
#[derive(Debug, Clone)]
pub struct BackendDefaults {
    /// Preferred injection method order (policy, not capability).
    /// First method that's applicable will be used.
    pub injection_order: Vec<InjectionMethod>,

    /// Profile -> backend build type mappings.
    /// e.g., ProfileKind::Debug -> "Debug", ProfileKind::Release -> "Release"
    pub profile_mappings: HashMap<ProfileKind, String>,

    /// Default sanitizer flags per sanitizer kind.
    pub sanitizer_flags: HashMap<SanitizerKind, Vec<String>>,

    /// Default parallel job count (None = use system default)
    pub default_jobs: Option<usize>,
}

impl Default for BackendDefaults {
    fn default() -> Self {
        let mut profile_mappings = HashMap::new();
        profile_mappings.insert(ProfileKind::Debug, "Debug".to_string());
        profile_mappings.insert(ProfileKind::Release, "Release".to_string());

        BackendDefaults {
            injection_order: vec![InjectionMethod::IncludeLib],
            profile_mappings,
            sanitizer_flags: HashMap::new(),
            default_jobs: None,
        }
    }
}

impl BackendDefaults {
    /// Create defaults for the Native backend.
    pub fn native() -> Self {
        // Native uses direct include/lib paths
        let mut defaults = Self {
            injection_order: vec![InjectionMethod::IncludeLib],
            ..Default::default()
        };

        // Sanitizer flags for GCC/Clang
        let mut sanitizers = HashMap::new();
        sanitizers.insert(
            SanitizerKind::Address,
            vec![
                "-fsanitize=address".to_string(),
                "-fno-omit-frame-pointer".to_string(),
            ],
        );
        sanitizers.insert(SanitizerKind::Thread, vec!["-fsanitize=thread".to_string()]);
        sanitizers.insert(
            SanitizerKind::Memory,
            vec![
                "-fsanitize=memory".to_string(),
                "-fno-omit-frame-pointer".to_string(),
            ],
        );
        sanitizers.insert(
            SanitizerKind::UndefinedBehavior,
            vec!["-fsanitize=undefined".to_string()],
        );
        sanitizers.insert(SanitizerKind::Leak, vec!["-fsanitize=leak".to_string()]);
        defaults.sanitizer_flags = sanitizers;

        defaults
    }

    /// Create defaults for the Custom backend.
    pub fn custom() -> Self {
        // Custom backends primarily use env vars
        Self {
            injection_order: vec![InjectionMethod::EnvVars],
            ..Default::default()
        }
    }

    /// Get the build type string for a profile.
    pub fn build_type(&self, profile: ProfileKind) -> String {
        self.profile_mappings
            .get(&profile)
            .cloned()
            .unwrap_or_else(|| profile.as_str().to_string())
    }

    /// Get sanitizer flags for a given sanitizer kind.
    pub fn get_sanitizer_flags(&self, kind: SanitizerKind) -> &[String] {
        self.sanitizer_flags
            .get(&kind)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }
}

/// Create a Harbour Profile from a ProfileKind with backend defaults.
pub fn profile_from_kind(kind: ProfileKind) -> Profile {
    match kind {
        ProfileKind::Debug => Profile {
            inherits: None,
            opt_level: Some("0".to_string()),
            debug: Some("2".to_string()),
            lto: Some(false),
            sanitizers: Vec::new(),
            cflags: Vec::new(),
            ldflags: Vec::new(),
        },
        ProfileKind::Release => Profile {
            inherits: None,
            opt_level: Some("3".to_string()),
            debug: Some("0".to_string()),
            lto: Some(false),
            sanitizers: Vec::new(),
            cflags: Vec::new(),
            ldflags: Vec::new(),
        },
        ProfileKind::Custom(_) => Profile::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_profile_kind_display() {
        assert_eq!(ProfileKind::Debug.to_string(), "debug");
        assert_eq!(ProfileKind::Release.to_string(), "release");
    }

    #[test]
    fn test_native_defaults() {
        let defaults = BackendDefaults::native();
        assert_eq!(defaults.injection_order[0], InjectionMethod::IncludeLib);
        assert!(!defaults.sanitizer_flags.is_empty());
    }

    #[test]
    fn test_build_type_mapping() {
        let defaults = BackendDefaults::native();
        assert_eq!(defaults.build_type(ProfileKind::Debug), "Debug");
        assert_eq!(defaults.build_type(ProfileKind::Release), "Release");
    }

    #[test]
    fn test_custom_defaults() {
        let defaults = BackendDefaults::custom();
        assert_eq!(defaults.injection_order[0], InjectionMethod::EnvVars);
    }

    #[test]
    fn test_sanitizer_flags() {
        let defaults = BackendDefaults::native();
        let asan_flags = defaults.get_sanitizer_flags(SanitizerKind::Address);
        assert!(asan_flags.iter().any(|f| f.contains("sanitize=address")));
    }
}
