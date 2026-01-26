//! Backend capability types - hard constraints on what backends can do.
//!
//! Capabilities are immutable facts about backends, not configuration.
//! Policy decisions (e.g., injection order) belong in `defaults.rs`.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Unique identifier for a build backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BackendId {
    /// Harbour's native C/C++ builder
    Native,
    /// CMake-based builds
    CMake,
    /// Meson-based builds
    Meson,
    /// Custom user-defined build commands
    Custom,
}

impl BackendId {
    /// Get the backend name as a string.
    pub fn as_str(&self) -> &'static str {
        match self {
            BackendId::Native => "native",
            BackendId::CMake => "cmake",
            BackendId::Meson => "meson",
            BackendId::Custom => "custom",
        }
    }
}

impl std::fmt::Display for BackendId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl std::str::FromStr for BackendId {
    type Err = BackendIdParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "native" => Ok(BackendId::Native),
            "cmake" => Ok(BackendId::CMake),
            "meson" => Ok(BackendId::Meson),
            "custom" => Ok(BackendId::Custom),
            _ => Err(BackendIdParseError(s.to_string())),
        }
    }
}

/// Error returned when parsing an invalid backend ID.
#[derive(Debug, Clone)]
pub struct BackendIdParseError(pub String);

impl std::fmt::Display for BackendIdParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "invalid backend ID '{}', valid values: native, cmake, meson, custom",
            self.0
        )
    }
}

impl std::error::Error for BackendIdParseError {}

/// Backend identity information.
#[derive(Debug, Clone)]
pub struct BackendIdentity {
    /// Unique backend identifier
    pub id: BackendId,

    /// Shim implementation version (for compatibility tracking)
    pub shim_version: semver::Version,

    /// Required backend tool version (e.g., CMake >= 3.16)
    /// None for backends that don't have external tools (like Native)
    pub backend_version_req: Option<semver::VersionReq>,
}

/// How a backend supports a particular build phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PhaseSupport {
    /// Phase is required and always runs
    Required,
    /// Phase is supported but optional
    Optional,
    /// Phase is not supported by this backend
    NotSupported,
}

impl PhaseSupport {
    /// Check if this phase is supported at all.
    pub fn is_supported(&self) -> bool {
        !matches!(self, PhaseSupport::NotSupported)
    }

    /// Check if this phase is required.
    pub fn is_required(&self) -> bool {
        matches!(self, PhaseSupport::Required)
    }
}

/// Build phase capabilities.
#[derive(Debug, Clone)]
pub struct PhaseCapabilities {
    /// Configure phase (CMake: cmake .., Meson: meson setup)
    pub configure: PhaseSupport,

    /// Build phase (compile & link)
    pub build: PhaseSupport,

    /// Test phase (ctest, meson test, etc.)
    pub test: PhaseSupport,

    /// Install phase (copy artifacts to prefix)
    pub install: PhaseSupport,

    /// Clean phase (remove build artifacts)
    pub clean: PhaseSupport,
}

impl Default for PhaseCapabilities {
    fn default() -> Self {
        PhaseCapabilities {
            configure: PhaseSupport::NotSupported,
            build: PhaseSupport::Required,
            test: PhaseSupport::Optional,
            install: PhaseSupport::Optional,
            clean: PhaseSupport::Required,
        }
    }
}

/// Platform/cross-compilation capabilities.
///
/// Note: We don't enumerate OS/arch - CMake/Meson run anywhere.
/// Only real backend limits are captured here.
#[derive(Debug, Clone, Default)]
pub struct PlatformCapabilities {
    /// Can build for target != host?
    pub cross_compile: bool,

    /// Supports --sysroot or equivalent
    pub sysroot_support: bool,

    /// Supports toolchain files (CMake CMAKE_TOOLCHAIN_FILE, Meson cross files)
    pub toolchain_file_support: bool,

    /// Backend only works on host (custom scripts might be host-only)
    pub host_only: bool,
}

/// Artifact generation capabilities.
#[derive(Debug, Clone)]
pub struct ArtifactCapabilities {
    /// Can produce static libraries (.a / .lib)
    pub static_lib: bool,

    /// Can produce shared libraries (.so / .dylib / .dll)
    pub shared_lib: bool,

    /// Can produce executables
    pub executable: bool,

    /// Can handle header-only libraries
    pub header_only: bool,

    /// Can produce test binaries
    pub test_binary: bool,

    /// Can produce both static and shared in a single invocation
    pub static_shared_single_invocation: bool,

    /// Install paths are deterministic across builds
    pub deterministic_install: bool,
}

impl Default for ArtifactCapabilities {
    fn default() -> Self {
        ArtifactCapabilities {
            static_lib: true,
            shared_lib: true,
            executable: true,
            header_only: true,
            test_binary: true,
            static_shared_single_invocation: false,
            deterministic_install: true,
        }
    }
}

/// RPATH handling support level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum RpathSupport {
    /// Full RPATH control ($ORIGIN, @executable_path, etc.)
    #[default]
    Full,
    /// Basic RPATH support
    Basic,
    /// No RPATH support
    None,
}

/// Linkage capabilities.
#[derive(Debug, Clone)]
pub struct LinkageCapabilities {
    /// Can link statically
    pub static_linking: bool,

    /// Can link dynamically
    pub shared_linking: bool,

    /// Can control symbol visibility (hidden by default, etc.)
    pub symbol_visibility_control: bool,

    /// RPATH handling support level
    pub rpath_handling: RpathSupport,

    /// Can generate import libraries (Windows DLL .lib files)
    pub import_lib_generation: bool,

    /// Can produce FFI shared lib + runtime deps bundle
    pub runtime_bundle: bool,
}

impl Default for LinkageCapabilities {
    fn default() -> Self {
        LinkageCapabilities {
            static_linking: true,
            shared_linking: true,
            symbol_visibility_control: true,
            rpath_handling: RpathSupport::Full,
            import_lib_generation: true,
            runtime_bundle: true,
        }
    }
}

/// Methods for injecting dependencies into a build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum InjectionMethod {
    /// CMAKE_PREFIX_PATH / PKG_CONFIG_PATH style prefix
    PrefixPath,
    /// CMake toolchain file / Meson cross file
    ToolchainFile,
    /// Environment variables (CC, CXX, CFLAGS, etc.)
    EnvVars,
    /// CMake -D defines
    CMakeDefines,
    /// Meson wrap files
    MesonWrap,
    /// Direct include/lib paths (Native builder)
    IncludeLib,
}

impl InjectionMethod {
    /// Get the method name as a string.
    pub fn as_str(&self) -> &'static str {
        match self {
            InjectionMethod::PrefixPath => "prefix_path",
            InjectionMethod::ToolchainFile => "toolchain_file",
            InjectionMethod::EnvVars => "env_vars",
            InjectionMethod::CMakeDefines => "cmake_defines",
            InjectionMethod::MesonWrap => "meson_wrap",
            InjectionMethod::IncludeLib => "include_lib",
        }
    }
}

/// Formats a dependency can be consumed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DependencyFormat {
    /// CMake config files (FooConfig.cmake)
    CMakeConfig,
    /// pkg-config .pc files
    PkgConfig,
    /// Direct include/lib paths
    IncludeLib,
}

/// How transitive dependencies are handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TransitiveHandling {
    /// Dependencies resolved via prefix path (CMake/Meson style)
    ViaPrefix,
    /// Harbour flattens transitive deps explicitly
    #[default]
    FlattenedByHarbour,
}

/// Dependency injection capabilities.
///
/// Capability: what methods are supported.
/// Policy (preferred order): belongs in BackendDefaults.
#[derive(Debug, Clone)]
pub struct DependencyInjection {
    /// Supported injection methods (capability: what's possible)
    pub supported_methods: HashSet<InjectionMethod>,

    /// Consumable dependency formats
    pub consumable_formats: HashSet<DependencyFormat>,

    /// How transitive dependencies are handled
    pub transitive_handling: TransitiveHandling,
}

impl Default for DependencyInjection {
    fn default() -> Self {
        let mut methods = HashSet::new();
        methods.insert(InjectionMethod::IncludeLib);

        let mut formats = HashSet::new();
        formats.insert(DependencyFormat::IncludeLib);

        DependencyInjection {
            supported_methods: methods,
            consumable_formats: formats,
            transitive_handling: TransitiveHandling::FlattenedByHarbour,
        }
    }
}

impl DependencyInjection {
    /// Check if an injection method is supported.
    pub fn supports_method(&self, method: InjectionMethod) -> bool {
        self.supported_methods.contains(&method)
    }

    /// Check if a dependency format is consumable.
    pub fn consumes_format(&self, format: DependencyFormat) -> bool {
        self.consumable_formats.contains(&format)
    }
}

/// Install phase contract.
#[derive(Debug, Clone)]
pub struct InstallContract {
    /// Must run install step before artifacts can be used
    pub requires_install_step: bool,

    /// Can set install prefix (CMAKE_INSTALL_PREFIX, --prefix, etc.)
    pub supports_install_prefix: bool,

    /// Install paths are deterministic across builds
    pub deterministic_install: bool,
}

impl Default for InstallContract {
    fn default() -> Self {
        InstallContract {
            requires_install_step: false,
            supports_install_prefix: true,
            deterministic_install: true,
        }
    }
}

/// Export discovery capability level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ExportDiscovery {
    /// Backend can fully discover all exports
    #[default]
    Full,
    /// Backend can discover exports if available
    Optional,
    /// Backend cannot discover exports; user must provide surface manually
    NotSupported,
}

/// Export discovery contract.
#[derive(Debug, Clone)]
pub struct ExportDiscoveryContract {
    /// Discovery capability level
    pub discovery: ExportDiscovery,

    /// discover_exports() requires install to run first
    pub requires_install: bool,
}

impl Default for ExportDiscoveryContract {
    fn default() -> Self {
        ExportDiscoveryContract {
            discovery: ExportDiscovery::Full,
            requires_install: false,
        }
    }
}

/// Factors that affect build cache validity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CacheInputFactor {
    /// Source file content
    Source,
    /// Toolchain identity (compiler, version)
    Toolchain,
    /// Build profile (debug/release)
    Profile,
    /// Backend options
    Options,
    /// Dependency versions
    Dependencies,
}

/// Caching contract.
#[derive(Debug, Clone)]
pub struct CachingContract {
    /// Factors that affect cache validity
    pub input_factors: HashSet<CacheInputFactor>,

    /// Builds are fully hermetic (no implicit dependencies)
    pub hermetic_builds: bool,

    /// Build directory is separate from source
    pub out_of_tree: bool,

    /// Install output is deterministic
    pub install_determinism: bool,
}

impl Default for CachingContract {
    fn default() -> Self {
        let mut factors = HashSet::new();
        factors.insert(CacheInputFactor::Source);
        factors.insert(CacheInputFactor::Toolchain);
        factors.insert(CacheInputFactor::Profile);
        factors.insert(CacheInputFactor::Options);
        factors.insert(CacheInputFactor::Dependencies);

        CachingContract {
            input_factors: factors,
            hermetic_builds: true,
            out_of_tree: true,
            install_determinism: true,
        }
    }
}

/// Complete backend capabilities.
///
/// This aggregates all capability categories for a backend.
/// Capabilities are hard constraints - facts about what the backend
/// can fundamentally do, not configuration or policy.
#[derive(Debug, Clone)]
pub struct BackendCapabilities {
    /// Backend identity (id, version info)
    pub identity: BackendIdentity,

    /// Build phase support
    pub phases: PhaseCapabilities,

    /// Platform/cross-compile support
    pub platform: PlatformCapabilities,

    /// Artifact generation support
    pub artifacts: ArtifactCapabilities,

    /// Linkage support
    pub linkage: LinkageCapabilities,

    /// Dependency injection support
    pub dependency_injection: DependencyInjection,

    /// Install contract
    pub install: InstallContract,

    /// Export discovery contract
    pub export_discovery: ExportDiscoveryContract,

    /// Caching contract
    pub caching: CachingContract,
}

impl BackendCapabilities {
    /// Create capabilities for a backend with the given identity.
    pub fn new(identity: BackendIdentity) -> Self {
        BackendCapabilities {
            identity,
            phases: PhaseCapabilities::default(),
            platform: PlatformCapabilities::default(),
            artifacts: ArtifactCapabilities::default(),
            linkage: LinkageCapabilities::default(),
            dependency_injection: DependencyInjection::default(),
            install: InstallContract::default(),
            export_discovery: ExportDiscoveryContract::default(),
            caching: CachingContract::default(),
        }
    }

    /// Check if the backend supports a linkage type.
    pub fn supports_linkage(&self, static_link: bool) -> bool {
        if static_link {
            self.linkage.static_linking
        } else {
            self.linkage.shared_linking
        }
    }

    /// Check if the backend supports cross-compilation.
    pub fn supports_cross_compile(&self) -> bool {
        self.platform.cross_compile && !self.platform.host_only
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backend_id_parse() {
        assert_eq!("native".parse::<BackendId>().unwrap(), BackendId::Native);
        assert_eq!("cmake".parse::<BackendId>().unwrap(), BackendId::CMake);
        assert_eq!("CMAKE".parse::<BackendId>().unwrap(), BackendId::CMake);
        assert!("invalid".parse::<BackendId>().is_err());
    }

    #[test]
    fn test_backend_id_display() {
        assert_eq!(BackendId::Native.to_string(), "native");
        assert_eq!(BackendId::CMake.to_string(), "cmake");
    }

    #[test]
    fn test_phase_support() {
        assert!(PhaseSupport::Required.is_supported());
        assert!(PhaseSupport::Required.is_required());
        assert!(PhaseSupport::Optional.is_supported());
        assert!(!PhaseSupport::Optional.is_required());
        assert!(!PhaseSupport::NotSupported.is_supported());
    }

    #[test]
    fn test_dependency_injection() {
        let mut injection = DependencyInjection::default();
        assert!(injection.supports_method(InjectionMethod::IncludeLib));
        assert!(!injection.supports_method(InjectionMethod::CMakeDefines));

        injection
            .supported_methods
            .insert(InjectionMethod::CMakeDefines);
        assert!(injection.supports_method(InjectionMethod::CMakeDefines));
    }

    #[test]
    fn test_backend_capabilities() {
        let identity = BackendIdentity {
            id: BackendId::Native,
            shim_version: semver::Version::new(1, 0, 0),
            backend_version_req: None,
        };
        let caps = BackendCapabilities::new(identity);

        assert!(caps.supports_linkage(true));
        assert!(caps.supports_linkage(false));
        assert!(!caps.supports_cross_compile()); // Default is false
    }
}
