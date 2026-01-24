//! User intent and backend options.
//!
//! BuildIntent represents what the user wants (abstract).
//! BackendOptions represents opaque backend-specific configuration.

use crate::builder::shim::capabilities::BackendId;
use crate::builder::shim::defaults::ProfileKind;
use crate::builder::toolchain::ToolchainPlatform;
use crate::core::target::{CppStandard, TargetKind};

/// Linkage preference from user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkagePreference {
    /// Prefer static linking
    Static,
    /// Prefer shared/dynamic linking
    Shared,
    /// Let the backend decide, with fallback order
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

/// Toolchain pin - request a specific compiler/version.
#[derive(Debug, Clone)]
pub struct ToolchainPin {
    /// Compiler kind (gcc, clang, msvc)
    pub compiler: ToolchainPlatform,
    /// Version requirement (optional)
    pub version: Option<semver::VersionReq>,
}

impl ToolchainPin {
    /// Create a new toolchain pin.
    pub fn new(compiler: ToolchainPlatform) -> Self {
        ToolchainPin {
            compiler,
            version: None,
        }
    }

    /// Create a toolchain pin with a version requirement.
    pub fn with_version(compiler: ToolchainPlatform, version: semver::VersionReq) -> Self {
        ToolchainPin {
            compiler,
            version: Some(version),
        }
    }

    /// Check if a toolchain matches this pin.
    pub fn matches(&self, platform: ToolchainPlatform, version: Option<&semver::Version>) -> bool {
        if self.compiler != platform {
            return false;
        }
        if let Some(ref req) = self.version {
            if let Some(ver) = version {
                return req.matches(ver);
            }
            return false; // Version required but not available
        }
        true
    }
}

/// Target triple for cross-compilation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TargetTriple {
    /// The triple string (e.g., "x86_64-unknown-linux-gnu")
    pub triple: String,
}

impl TargetTriple {
    /// Create a new target triple.
    pub fn new(triple: impl Into<String>) -> Self {
        TargetTriple {
            triple: triple.into(),
        }
    }

    /// Check if this is the host triple.
    pub fn is_host(&self) -> bool {
        // Simple heuristic: check against common host triples
        #[cfg(target_os = "linux")]
        {
            self.triple.contains("linux")
        }
        #[cfg(target_os = "macos")]
        {
            self.triple.contains("darwin") || self.triple.contains("macos")
        }
        #[cfg(target_os = "windows")]
        {
            self.triple.contains("windows") || self.triple.contains("msvc")
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            false
        }
    }

    /// Get the OS from the triple.
    pub fn os(&self) -> Option<&str> {
        let parts: Vec<&str> = self.triple.split('-').collect();
        parts.get(2).copied()
    }

    /// Get the architecture from the triple.
    pub fn arch(&self) -> Option<&str> {
        let parts: Vec<&str> = self.triple.split('-').collect();
        parts.first().copied()
    }
}

impl std::fmt::Display for TargetTriple {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.triple)
    }
}

/// What the user wants - NOT how the backend does it.
///
/// BuildIntent represents the abstract user request before backend translation.
/// This is validated against backend capabilities before being converted to
/// backend-specific operations.
#[derive(Debug, Clone)]
pub struct BuildIntent {
    /// Linkage preference (static, shared, auto)
    pub linkage: LinkagePreference,

    /// Desired output artifact kinds
    pub outputs: Vec<TargetKind>,

    /// Build profile (debug, release)
    pub profile: ProfileKind,

    /// FFI build mode: guarantee shared lib + runtime bundle
    pub ffi: bool,

    /// Pin to a specific toolchain
    pub toolchain_pin: Option<ToolchainPin>,

    /// Target triple for cross-compilation
    pub target_triple: Option<TargetTriple>,

    /// Force a specific backend
    pub backend: Option<BackendId>,

    /// Requested C++ standard (overrides manifest)
    pub cpp_std: Option<CppStandard>,

    /// Enable tests
    pub with_tests: bool,

    /// Specific targets to build (None = all)
    pub targets: Option<Vec<String>>,

    /// Parallelism level (None = default)
    pub jobs: Option<usize>,
}

impl Default for BuildIntent {
    fn default() -> Self {
        BuildIntent {
            linkage: LinkagePreference::default(),
            outputs: vec![TargetKind::StaticLib, TargetKind::Exe],
            profile: ProfileKind::Debug,
            ffi: false,
            toolchain_pin: None,
            target_triple: None,
            backend: None,
            cpp_std: None,
            with_tests: false,
            targets: None,
            jobs: None,
        }
    }
}

impl BuildIntent {
    /// Create a new build intent with default values.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a release build intent.
    pub fn release() -> Self {
        BuildIntent {
            profile: ProfileKind::Release,
            ..Default::default()
        }
    }

    /// Set the linkage preference.
    pub fn with_linkage(mut self, linkage: LinkagePreference) -> Self {
        self.linkage = linkage;
        self
    }

    /// Set FFI mode (requires shared library output).
    pub fn with_ffi(mut self, ffi: bool) -> Self {
        self.ffi = ffi;
        if ffi {
            // FFI implies shared linkage
            self.linkage = LinkagePreference::Shared;
        }
        self
    }

    /// Set the build profile.
    pub fn with_profile(mut self, profile: ProfileKind) -> Self {
        self.profile = profile;
        self
    }

    /// Set the target triple for cross-compilation.
    pub fn with_target_triple(mut self, triple: TargetTriple) -> Self {
        self.target_triple = Some(triple);
        self
    }

    /// Force a specific backend.
    pub fn with_backend(mut self, backend: BackendId) -> Self {
        self.backend = Some(backend);
        self
    }

    /// Set the C++ standard.
    pub fn with_cpp_std(mut self, std: CppStandard) -> Self {
        self.cpp_std = Some(std);
        self
    }

    /// Enable tests.
    pub fn with_tests(mut self, enable: bool) -> Self {
        self.with_tests = enable;
        self
    }

    /// Set specific targets to build.
    pub fn with_targets(mut self, targets: Vec<String>) -> Self {
        self.targets = Some(targets);
        self
    }

    /// Check if this is a cross-compilation request.
    pub fn is_cross_compile(&self) -> bool {
        self.target_triple
            .as_ref()
            .map(|t| !t.is_host())
            .unwrap_or(false)
    }

    /// Check if shared libraries are required.
    pub fn requires_shared(&self) -> bool {
        self.ffi || self.linkage.requires_shared()
    }
}

/// Backend-specific options - opaque to Harbour core.
///
/// Each backend interprets its options differently:
/// - CMake: CMAKE_* defines
/// - Meson: meson options
/// - Custom: environment variables or script args
#[derive(Debug, Clone, Default)]
pub struct BackendOptions {
    /// Opaque options table (backend-specific keys)
    pub options: toml::Table,
}

impl BackendOptions {
    /// Create empty backend options.
    pub fn new() -> Self {
        BackendOptions {
            options: toml::Table::new(),
        }
    }

    /// Create backend options from a TOML table.
    pub fn from_table(table: toml::Table) -> Self {
        BackendOptions { options: table }
    }

    /// Get a string option.
    pub fn get_string(&self, key: &str) -> Option<&str> {
        self.options.get(key).and_then(|v| v.as_str())
    }

    /// Get a bool option.
    pub fn get_bool(&self, key: &str) -> Option<bool> {
        self.options.get(key).and_then(|v| v.as_bool())
    }

    /// Get an integer option.
    pub fn get_integer(&self, key: &str) -> Option<i64> {
        self.options.get(key).and_then(|v| v.as_integer())
    }

    /// Get an array option.
    pub fn get_array(&self, key: &str) -> Option<&toml::value::Array> {
        self.options.get(key).and_then(|v| v.as_array())
    }

    /// Set a string option.
    pub fn set_string(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.options
            .insert(key.into(), toml::Value::String(value.into()));
    }

    /// Set a bool option.
    pub fn set_bool(&mut self, key: impl Into<String>, value: bool) {
        self.options
            .insert(key.into(), toml::Value::Boolean(value));
    }

    /// Set an integer option.
    pub fn set_integer(&mut self, key: impl Into<String>, value: i64) {
        self.options
            .insert(key.into(), toml::Value::Integer(value));
    }

    /// Check if the options are empty.
    pub fn is_empty(&self) -> bool {
        self.options.is_empty()
    }

    /// Iterate over all options.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &toml::Value)> {
        self.options.iter()
    }

    /// Merge another BackendOptions into this one.
    /// Existing keys are overwritten.
    pub fn merge(&mut self, other: &BackendOptions) {
        for (key, value) in &other.options {
            self.options.insert(key.clone(), value.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_linkage_preference_parse() {
        assert!(matches!("static".parse::<LinkagePreference>().unwrap(), LinkagePreference::Static));
        assert!(matches!("shared".parse::<LinkagePreference>().unwrap(), LinkagePreference::Shared));
        assert!(matches!("dynamic".parse::<LinkagePreference>().unwrap(), LinkagePreference::Shared));
        assert!(matches!("auto".parse::<LinkagePreference>().unwrap(), LinkagePreference::Auto { .. }));
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

    #[test]
    fn test_build_intent_ffi() {
        let intent = BuildIntent::new().with_ffi(true);
        assert!(intent.ffi);
        assert!(intent.requires_shared());
        assert!(matches!(intent.linkage, LinkagePreference::Shared));
    }

    #[test]
    fn test_build_intent_cross_compile() {
        let intent = BuildIntent::new()
            .with_target_triple(TargetTriple::new("aarch64-unknown-linux-gnu"));

        // On non-aarch64-linux systems, this is cross-compile
        #[cfg(not(all(target_arch = "aarch64", target_os = "linux")))]
        assert!(intent.is_cross_compile());
    }

    #[test]
    fn test_backend_options() {
        let mut opts = BackendOptions::new();
        opts.set_string("CMAKE_BUILD_TYPE", "Release");
        opts.set_bool("BUILD_SHARED_LIBS", true);
        opts.set_integer("MAX_THREADS", 8);

        assert_eq!(opts.get_string("CMAKE_BUILD_TYPE"), Some("Release"));
        assert_eq!(opts.get_bool("BUILD_SHARED_LIBS"), Some(true));
        assert_eq!(opts.get_integer("MAX_THREADS"), Some(8));
    }

    #[test]
    fn test_backend_options_merge() {
        let mut opts1 = BackendOptions::new();
        opts1.set_string("KEY1", "value1");
        opts1.set_string("KEY2", "original");

        let mut opts2 = BackendOptions::new();
        opts2.set_string("KEY2", "overwritten");
        opts2.set_string("KEY3", "value3");

        opts1.merge(&opts2);

        assert_eq!(opts1.get_string("KEY1"), Some("value1"));
        assert_eq!(opts1.get_string("KEY2"), Some("overwritten"));
        assert_eq!(opts1.get_string("KEY3"), Some("value3"));
    }

    #[test]
    fn test_target_triple() {
        let triple = TargetTriple::new("x86_64-unknown-linux-gnu");
        assert_eq!(triple.arch(), Some("x86_64"));
        assert_eq!(triple.os(), Some("linux"));
    }
}
