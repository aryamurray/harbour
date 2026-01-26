//! Backend shim abstraction system.
//!
//! This module provides a capability-based contract system where build backends
//! (cmake, meson, native, custom) declare what they can do, and Harbour validates
//! user requests against those capabilities before attempting builds.
//!
//! # Architecture
//!
//! ```text
//!                       ┌─────────────────┐
//!                       │   BuildIntent   │ (user wants)
//!                       └────────┬────────┘
//!                                │
//!          ┌─────────────────────┼─────────────────────┐
//!          ▼                     ▼                     ▼
//!  ┌───────────────┐    ┌───────────────┐    ┌───────────────┐
//!  │ Backend       │    │ Toolchain     │    │ Package       │
//!  │ Validator     │    │ Validator     │    │ Validator     │
//!  └───────┬───────┘    └───────┬───────┘    └───────┬───────┘
//!          └─────────────────────┼─────────────────────┘
//!                                ▼
//!               ┌────────────────┼────────────────┐
//!               ▼                ▼                ▼
//!       ┌───────────────┐ ┌───────────────┐ ┌───────────────┐
//!       │  NativeShim   │ │  CMakeShim    │ │  CustomShim   │
//!       └───────────────┘ └───────────────┘ └───────────────┘
//! ```
//!
//! # Key Concepts
//!
//! - **Capabilities** - Hard constraints on what a backend can do (in `capabilities.rs`)
//! - **Defaults** - Policy/preferences that can change without affecting capabilities (in `defaults.rs`)
//! - **BuildIntent** - What the user wants (abstract, in `intent.rs`)
//! - **BackendOptions** - Opaque backend-specific configuration (in `intent.rs`)
//! - **BackendShim** - Trait for backend implementations (in `trait_def.rs`)
//! - **Validation** - Three-stage validation system (in `validation.rs`)
//! - **Registry** - Lazy backend discovery and management (in `registry.rs`)
//!
//! # Usage
//!
//! ```ignore
//! use harbour::builder::shim::{BackendRegistry, BuildIntent, validate_build};
//!
//! // Registry always succeeds (no I/O)
//! let registry = BackendRegistry::new();
//!
//! // Check availability lazily
//! let cmake = registry.get(BackendId::CMake)?;
//! match cmake.availability()? {
//!     BackendAvailability::Available { version } => println!("CMake {}", version),
//!     BackendAvailability::NotInstalled { tool, install_hint } => {
//!         eprintln!("{} not found. {}", tool, install_hint);
//!     }
//!     _ => {}
//! }
//!
//! // Validate before building
//! let intent = BuildIntent::new().with_ffi(true);
//! validate_build(&intent, cmake, toolchain, package, opts)?;
//! ```

pub mod capabilities;
pub mod cmake_shim;
pub mod custom_shim;
pub mod defaults;
pub mod intent;
pub mod native_shim;
pub mod registry;
pub mod trait_def;
pub mod validation;

// Re-export commonly used types
pub use capabilities::{
    ArtifactCapabilities, BackendCapabilities, BackendId, BackendIdParseError, BackendIdentity,
    CacheInputFactor, CachingContract, DependencyFormat, DependencyInjection, ExportDiscovery,
    ExportDiscoveryContract, InjectionMethod, InstallContract, LinkageCapabilities,
    PhaseCapabilities, PhaseSupport, PlatformCapabilities, RpathSupport, TransitiveHandling,
};

pub use defaults::{
    profile_from_kind, BackendDefaults, GeneratorConfig, ProfileKind, SanitizerKind,
};

pub use intent::{
    ArtifactKind, BackendOptions, BuildIntent, LinkagePreference, LinkagePreferenceParseError,
    TargetTriple, ToolchainPin,
};

pub use trait_def::{
    Artifact, ArtifactType, BackendAvailability, BackendShim, BuildContext, BuildResult,
    CleanOptions, ConfigureResult, Define, DiscoveredSurface, DoctorCheck, DoctorReport,
    InstallResult, InstalledFile, InstalledFileKind, LibraryInfo, LibraryKind, TestCase,
    TestResult, TestStatus, ValidationResult,
};

pub use validation::{
    validate_backend_and_toolchain, validate_build, BackendValidator, PackageValidator,
    ToolchainCapabilities, ToolchainValidator, ValidationError,
};

pub use registry::{get_backend_summaries, BackendRegistry, BackendSummary};

pub use cmake_shim::CMakeShim;
pub use custom_shim::CustomShim;
pub use native_shim::NativeShim;
