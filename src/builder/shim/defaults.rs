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

/// Generator configuration (CMake/Meson specific).
#[derive(Debug, Clone, Default)]
pub struct GeneratorConfig {
    /// Generator supports multiple configurations (Debug/Release) in one build dir
    /// Visual Studio = true, Ninja = false, Unix Makefiles = false
    pub multi_config: bool,
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

    /// Preferred generator (CMake/Meson specific).
    /// e.g., "Ninja", "Unix Makefiles", "Visual Studio 17 2022"
    pub preferred_generator: Option<String>,

    /// Generator -> configuration mapping (derived at runtime).
    /// Note: multi_config is a property of the generator, not the backend.
    pub generator_configs: HashMap<String, GeneratorConfig>,

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
            preferred_generator: None,
            generator_configs: HashMap::new(),
            profile_mappings,
            sanitizer_flags: HashMap::new(),
            default_jobs: None,
        }
    }
}

impl BackendDefaults {
    /// Create defaults for the Native backend.
    pub fn native() -> Self {
        let mut defaults = Self::default();

        // Native uses direct include/lib paths
        defaults.injection_order = vec![InjectionMethod::IncludeLib];

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

    /// Create defaults for the CMake backend.
    pub fn cmake() -> Self {
        let mut defaults = Self::default();

        // CMake prefers prefix path, then defines, then toolchain files
        defaults.injection_order = vec![
            InjectionMethod::PrefixPath,
            InjectionMethod::CMakeDefines,
            InjectionMethod::ToolchainFile,
        ];

        // Prefer Ninja if available
        defaults.preferred_generator = Some("Ninja".to_string());

        // Known generator configurations
        let mut configs = HashMap::new();
        configs.insert(
            "Ninja".to_string(),
            GeneratorConfig {
                multi_config: false,
            },
        );
        configs.insert(
            "Ninja Multi-Config".to_string(),
            GeneratorConfig { multi_config: true },
        );
        configs.insert(
            "Unix Makefiles".to_string(),
            GeneratorConfig {
                multi_config: false,
            },
        );
        configs.insert(
            "NMake Makefiles".to_string(),
            GeneratorConfig {
                multi_config: false,
            },
        );
        // Visual Studio generators
        for vs in [
            "Visual Studio 17 2022",
            "Visual Studio 16 2019",
            "Visual Studio 15 2017",
        ] {
            configs.insert(vs.to_string(), GeneratorConfig { multi_config: true });
        }
        configs.insert("Xcode".to_string(), GeneratorConfig { multi_config: true });
        defaults.generator_configs = configs;

        // CMake build types
        let mut mappings = HashMap::new();
        mappings.insert(ProfileKind::Debug, "Debug".to_string());
        mappings.insert(ProfileKind::Release, "Release".to_string());
        defaults.profile_mappings = mappings;

        defaults
    }

    /// Create defaults for the Meson backend.
    pub fn meson() -> Self {
        let mut defaults = Self::default();

        // Meson prefers prefix path
        defaults.injection_order = vec![
            InjectionMethod::PrefixPath,
            InjectionMethod::MesonWrap,
            InjectionMethod::EnvVars,
        ];

        // Meson build types
        let mut mappings = HashMap::new();
        mappings.insert(ProfileKind::Debug, "debug".to_string());
        mappings.insert(ProfileKind::Release, "release".to_string());
        defaults.profile_mappings = mappings;

        defaults
    }

    /// Create defaults for the Custom backend.
    pub fn custom() -> Self {
        let mut defaults = Self::default();

        // Custom backends primarily use env vars
        defaults.injection_order = vec![InjectionMethod::EnvVars];

        defaults
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

    /// Check if the preferred generator is multi-config.
    pub fn is_multi_config(&self) -> bool {
        if let Some(ref gen) = self.preferred_generator {
            self.generator_configs
                .get(gen)
                .map(|c| c.multi_config)
                .unwrap_or(false)
        } else {
            false
        }
    }

    /// Get the generator config for a given generator name.
    pub fn generator_config(&self, name: &str) -> Option<&GeneratorConfig> {
        self.generator_configs.get(name)
    }
}

/// Create a Harbour Profile from a ProfileKind with backend defaults.
pub fn profile_from_kind(kind: ProfileKind) -> Profile {
    match kind {
        ProfileKind::Debug => Profile {
            opt_level: Some("0".to_string()),
            debug: Some("2".to_string()),
            lto: Some(false),
            sanitizers: Vec::new(),
            cflags: Vec::new(),
            ldflags: Vec::new(),
        },
        ProfileKind::Release => Profile {
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
    fn test_cmake_defaults() {
        let defaults = BackendDefaults::cmake();
        assert_eq!(defaults.injection_order[0], InjectionMethod::PrefixPath);
        assert_eq!(defaults.preferred_generator, Some("Ninja".to_string()));
        assert!(!defaults.is_multi_config()); // Ninja is single-config
    }

    #[test]
    fn test_cmake_multi_config_detection() {
        let defaults = BackendDefaults::cmake();
        assert!(
            defaults
                .generator_config("Visual Studio 17 2022")
                .unwrap()
                .multi_config
        );
        assert!(defaults.generator_config("Xcode").unwrap().multi_config);
        assert!(!defaults.generator_config("Ninja").unwrap().multi_config);
    }

    #[test]
    fn test_build_type_mapping() {
        let defaults = BackendDefaults::cmake();
        assert_eq!(defaults.build_type(ProfileKind::Debug), "Debug");
        assert_eq!(defaults.build_type(ProfileKind::Release), "Release");
    }

    #[test]
    fn test_sanitizer_flags() {
        let defaults = BackendDefaults::native();
        let asan_flags = defaults.get_sanitizer_flags(SanitizerKind::Address);
        assert!(asan_flags.iter().any(|f| f.contains("sanitize=address")));
    }
}
