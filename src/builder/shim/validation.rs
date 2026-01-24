//! Three-stage validation system for build requests.
//!
//! Validation is split into three stages:
//! 1. Backend validation - checks against backend capabilities
//! 2. Toolchain validation - checks against detected toolchain
//! 3. Package validation - checks against package definition

use thiserror::Error;

use crate::builder::shim::capabilities::{
    BackendCapabilities, ExportDiscovery, PhaseSupport,
};
use crate::builder::shim::intent::{BackendOptions, BuildIntent, LinkagePreference};
use crate::builder::shim::trait_def::{BackendShim, ValidationResult};
use crate::builder::toolchain::Toolchain;
use crate::core::package::Package;
use crate::core::target::{CppStandard, TargetKind};

/// Validation error types.
#[derive(Debug, Error)]
pub enum ValidationError {
    #[error("linkage not supported: requested {requested}, backend supports: {supported:?}")]
    UnsupportedLinkage {
        requested: String,
        supported: Vec<String>,
    },

    #[error("FFI requires shared library but backend only supports static linking")]
    FfiRequiresShared {
        reason: String,
        suggestions: Vec<String>,
    },

    #[error("cross-compilation not supported by this backend")]
    CrossCompileNotSupported,

    #[error("build phase `{phase}` not supported by this backend")]
    PhaseNotSupported { phase: String },

    #[error("configure phase required but not supported")]
    ConfigureRequired,

    #[error("artifact type `{artifact}` not supported by this backend")]
    ArtifactNotSupported { artifact: String },

    #[error("C++ standard `{requested}` not supported by compiler, maximum: {max}")]
    UnsupportedCppStandard {
        requested: CppStandard,
        max: CppStandard,
    },

    #[error("toolchain platform `{requested}` not available, have: {available}")]
    ToolchainMismatch { requested: String, available: String },

    #[error("export discovery required for library target but backend doesn't support it")]
    ExportDiscoveryRequired { target: String },

    #[error("target kind `{kind}` not found in package")]
    TargetKindMissing { kind: String },

    #[error("backend option validation failed: {message}")]
    BackendOptionInvalid { message: String },

    #[error("multiple validation errors")]
    Multiple(Vec<ValidationError>),
}

impl ValidationError {
    /// Create a multiple error from a vec of errors.
    pub fn multiple(errors: Vec<ValidationError>) -> Self {
        if errors.len() == 1 {
            errors.into_iter().next().unwrap()
        } else {
            ValidationError::Multiple(errors)
        }
    }

    /// Check if this contains any errors.
    pub fn has_errors(&self) -> bool {
        match self {
            ValidationError::Multiple(errors) => !errors.is_empty(),
            _ => true,
        }
    }

    /// Get all error messages.
    pub fn messages(&self) -> Vec<String> {
        match self {
            ValidationError::Multiple(errors) => {
                errors.iter().map(|e| e.to_string()).collect()
            }
            e => vec![e.to_string()],
        }
    }
}

/// Stage 1: Backend validator.
///
/// Checks intent against backend capabilities (phases, linkage, injection).
pub struct BackendValidator<'a> {
    capabilities: &'a BackendCapabilities,
}

impl<'a> BackendValidator<'a> {
    /// Create a new backend validator.
    pub fn new(capabilities: &'a BackendCapabilities) -> Self {
        BackendValidator { capabilities }
    }

    /// Validate the build intent against backend capabilities.
    pub fn validate(&self, intent: &BuildIntent) -> Result<(), ValidationError> {
        self.validate_phases(intent)?;
        self.validate_linkage(intent)?;
        self.validate_ffi(intent)?;
        self.validate_cross_compile(intent)?;
        self.validate_artifacts(intent)?;
        Ok(())
    }

    fn validate_phases(&self, intent: &BuildIntent) -> Result<(), ValidationError> {
        // Check if configure is required but not supported
        if self.capabilities.phases.configure == PhaseSupport::Required {
            // Configure is required - this is fine
        }

        // Check if tests are requested but not supported
        if intent.with_tests && !self.capabilities.phases.test.is_supported() {
            return Err(ValidationError::PhaseNotSupported {
                phase: "test".to_string(),
            });
        }

        Ok(())
    }

    fn validate_linkage(&self, intent: &BuildIntent) -> Result<(), ValidationError> {
        let caps = &self.capabilities.linkage;
        let mut supported = Vec::new();

        if caps.static_linking {
            supported.push("static".to_string());
        }
        if caps.shared_linking {
            supported.push("shared".to_string());
        }

        match &intent.linkage {
            LinkagePreference::Static if !caps.static_linking => {
                return Err(ValidationError::UnsupportedLinkage {
                    requested: "static".to_string(),
                    supported,
                });
            }
            LinkagePreference::Shared if !caps.shared_linking => {
                return Err(ValidationError::UnsupportedLinkage {
                    requested: "shared".to_string(),
                    supported,
                });
            }
            LinkagePreference::Auto { prefer } => {
                // Check if at least one preferred option is supported
                let any_supported = prefer.iter().any(|kind| match kind {
                    crate::builder::shim::intent::ArtifactKind::Static => caps.static_linking,
                    crate::builder::shim::intent::ArtifactKind::Shared => caps.shared_linking,
                });
                if !any_supported {
                    return Err(ValidationError::UnsupportedLinkage {
                        requested: "auto".to_string(),
                        supported,
                    });
                }
            }
            _ => {}
        }

        Ok(())
    }

    fn validate_ffi(&self, intent: &BuildIntent) -> Result<(), ValidationError> {
        if intent.ffi {
            // FFI requires shared library support
            if !self.capabilities.linkage.shared_linking {
                return Err(ValidationError::FfiRequiresShared {
                    reason: "FFI builds require shared library output".to_string(),
                    suggestions: vec![
                        "Use --backend=cmake or --backend=native which support shared libraries".to_string(),
                        "Build without the --ffi flag if static libraries are acceptable".to_string(),
                    ],
                });
            }

            // FFI benefits from runtime bundle support
            if !self.capabilities.linkage.runtime_bundle {
                // This is a warning, not an error - log it
                tracing::warn!(
                    "Backend does not support runtime bundling; \
                     FFI consumers may need to handle dependencies manually"
                );
            }
        }

        Ok(())
    }

    fn validate_cross_compile(&self, intent: &BuildIntent) -> Result<(), ValidationError> {
        if intent.is_cross_compile() {
            if !self.capabilities.supports_cross_compile() {
                return Err(ValidationError::CrossCompileNotSupported);
            }
        }

        Ok(())
    }

    fn validate_artifacts(&self, intent: &BuildIntent) -> Result<(), ValidationError> {
        let caps = &self.capabilities.artifacts;

        for output in &intent.outputs {
            let supported = match output {
                TargetKind::StaticLib => caps.static_lib,
                TargetKind::SharedLib => caps.shared_lib,
                TargetKind::Exe => caps.executable,
                TargetKind::HeaderOnly => caps.header_only,
            };

            if !supported {
                return Err(ValidationError::ArtifactNotSupported {
                    artifact: format!("{:?}", output),
                });
            }
        }

        Ok(())
    }
}

/// Toolchain capabilities derived at runtime.
#[derive(Debug, Clone)]
pub struct ToolchainCapabilities {
    /// Maximum supported C++ standard
    pub max_cpp_standard: CppStandard,

    /// Supports LTO
    pub supports_lto: bool,

    /// Supported sanitizers
    pub supported_sanitizers: Vec<String>,
}

impl Default for ToolchainCapabilities {
    fn default() -> Self {
        ToolchainCapabilities {
            max_cpp_standard: CppStandard::Cpp17,
            supports_lto: true,
            supported_sanitizers: vec![
                "address".to_string(),
                "thread".to_string(),
                "undefined".to_string(),
            ],
        }
    }
}

/// Stage 2: Toolchain validator.
///
/// Checks intent against detected toolchain capabilities.
pub struct ToolchainValidator<'a> {
    toolchain: &'a dyn Toolchain,
    capabilities: ToolchainCapabilities,
}

impl<'a> ToolchainValidator<'a> {
    /// Create a new toolchain validator.
    pub fn new(toolchain: &'a dyn Toolchain) -> Self {
        // Derive capabilities from toolchain
        let capabilities = Self::derive_capabilities(toolchain);
        ToolchainValidator {
            toolchain,
            capabilities,
        }
    }

    /// Create with explicit capabilities (for testing).
    pub fn with_capabilities(
        toolchain: &'a dyn Toolchain,
        capabilities: ToolchainCapabilities,
    ) -> Self {
        ToolchainValidator {
            toolchain,
            capabilities,
        }
    }

    fn derive_capabilities(toolchain: &dyn Toolchain) -> ToolchainCapabilities {
        use crate::builder::toolchain::ToolchainPlatform;

        // Derive max C++ standard from compiler platform
        let max_cpp_standard = match toolchain.platform() {
            ToolchainPlatform::Msvc => CppStandard::Cpp23, // MSVC 2022+
            ToolchainPlatform::Clang | ToolchainPlatform::AppleClang => CppStandard::Cpp23,
            ToolchainPlatform::Gcc => CppStandard::Cpp23, // GCC 13+
        };

        // Sanitizer support varies by platform
        let supported_sanitizers = match toolchain.platform() {
            ToolchainPlatform::Msvc => vec!["address".to_string()], // MSVC has limited sanitizer support
            _ => vec![
                "address".to_string(),
                "thread".to_string(),
                "undefined".to_string(),
                "leak".to_string(),
            ],
        };

        ToolchainCapabilities {
            max_cpp_standard,
            supports_lto: true,
            supported_sanitizers,
        }
    }

    /// Validate the build intent against toolchain capabilities.
    pub fn validate(&self, intent: &BuildIntent) -> Result<(), ValidationError> {
        self.validate_toolchain_pin(intent)?;
        self.validate_cpp_standard(intent)?;
        Ok(())
    }

    fn validate_toolchain_pin(&self, intent: &BuildIntent) -> Result<(), ValidationError> {
        if let Some(ref pin) = intent.toolchain_pin {
            let actual = self.toolchain.platform();
            if pin.compiler != actual {
                return Err(ValidationError::ToolchainMismatch {
                    requested: pin.compiler.as_str().to_string(),
                    available: actual.as_str().to_string(),
                });
            }
            // Version check would require detecting compiler version
            // which is done at toolchain detection time
        }
        Ok(())
    }

    fn validate_cpp_standard(&self, intent: &BuildIntent) -> Result<(), ValidationError> {
        if let Some(requested) = intent.cpp_std {
            if requested > self.capabilities.max_cpp_standard {
                return Err(ValidationError::UnsupportedCppStandard {
                    requested,
                    max: self.capabilities.max_cpp_standard,
                });
            }
        }
        Ok(())
    }

    /// Get the toolchain capabilities.
    pub fn capabilities(&self) -> &ToolchainCapabilities {
        &self.capabilities
    }
}

/// Stage 3: Package validator.
///
/// Checks intent against package definition (target kinds, exports).
pub struct PackageValidator<'a> {
    package: &'a Package,
    export_discovery: ExportDiscovery,
}

impl<'a> PackageValidator<'a> {
    /// Create a new package validator.
    pub fn new(package: &'a Package, export_discovery: ExportDiscovery) -> Self {
        PackageValidator {
            package,
            export_discovery,
        }
    }

    /// Validate the build intent against package definition.
    pub fn validate(&self, intent: &BuildIntent) -> Result<(), ValidationError> {
        self.validate_target_kinds(intent)?;
        self.validate_exports_per_target()?;
        Ok(())
    }

    fn validate_target_kinds(&self, intent: &BuildIntent) -> Result<(), ValidationError> {
        // If specific targets requested, check they exist
        if let Some(ref target_names) = intent.targets {
            for name in target_names {
                if !self.package.targets().iter().any(|t| t.name.as_str() == name) {
                    return Err(ValidationError::TargetKindMissing {
                        kind: name.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    fn validate_exports_per_target(&self) -> Result<(), ValidationError> {
        // Only library targets that could be dependencies require discoverable exports
        // header_only and exe don't need this
        for target in self.package.targets().iter() {
            if target.kind.is_linkable() {
                // This target produces a linkable artifact
                // If it's likely to be used as a dependency, we need export discovery
                if self.export_discovery == ExportDiscovery::NotSupported {
                    // Check if the target has an explicit surface defined
                    // If so, we don't need automatic discovery
                    let has_surface = !target.surface.compile.public.include_dirs.is_empty()
                        || !target.surface.link.public.libs.is_empty();

                    if !has_surface {
                        return Err(ValidationError::ExportDiscoveryRequired {
                            target: target.name.to_string(),
                        });
                    }
                }
            }
        }
        Ok(())
    }
}

/// Unified validation function.
///
/// Runs all three validation stages and collects errors.
pub fn validate_build(
    intent: &BuildIntent,
    backend: &dyn BackendShim,
    toolchain: &dyn Toolchain,
    package: &Package,
    opts: &BackendOptions,
) -> Result<ValidationResult, ValidationError> {
    let mut errors = Vec::new();

    // Stage 1: Backend validation
    let backend_validator = BackendValidator::new(backend.capabilities());
    if let Err(e) = backend_validator.validate(intent) {
        errors.push(e);
    }

    // Stage 2: Toolchain validation
    let toolchain_validator = ToolchainValidator::new(toolchain);
    if let Err(e) = toolchain_validator.validate(intent) {
        errors.push(e);
    }

    // Stage 3: Package validation
    let export_discovery = backend.capabilities().export_discovery.discovery;
    let package_validator = PackageValidator::new(package, export_discovery);
    if let Err(e) = package_validator.validate(intent) {
        errors.push(e);
    }

    // Stage 4: Backend-specific extra validation
    if let Err(e) = backend.validate_extra(intent, opts) {
        errors.push(e);
    }

    if errors.is_empty() {
        Ok(ValidationResult::default())
    } else {
        Err(ValidationError::multiple(errors))
    }
}

/// Quick validation without package (for early checks).
pub fn validate_backend_and_toolchain(
    intent: &BuildIntent,
    backend: &dyn BackendShim,
    toolchain: &dyn Toolchain,
) -> Result<(), ValidationError> {
    let mut errors = Vec::new();

    let backend_validator = BackendValidator::new(backend.capabilities());
    if let Err(e) = backend_validator.validate(intent) {
        errors.push(e);
    }

    let toolchain_validator = ToolchainValidator::new(toolchain);
    if let Err(e) = toolchain_validator.validate(intent) {
        errors.push(e);
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationError::multiple(errors))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::shim::capabilities::*;

    fn mock_capabilities() -> BackendCapabilities {
        BackendCapabilities::new(BackendIdentity {
            id: BackendId::Native,
            shim_version: semver::Version::new(1, 0, 0),
            backend_version_req: None,
        })
    }

    #[test]
    fn test_backend_validator_linkage() {
        let mut caps = mock_capabilities();
        caps.linkage.shared_linking = false;

        let validator = BackendValidator::new(&caps);
        let intent = BuildIntent::new().with_linkage(LinkagePreference::Shared);

        let result = validator.validate(&intent);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ValidationError::UnsupportedLinkage { .. }
        ));
    }

    #[test]
    fn test_backend_validator_ffi() {
        // When FFI is enabled and shared linking is disabled, we should get
        // an UnsupportedLinkage error because with_ffi(true) sets linkage to Shared
        let mut caps = mock_capabilities();
        caps.linkage.shared_linking = false;

        let validator = BackendValidator::new(&caps);
        let intent = BuildIntent::new().with_ffi(true);

        // with_ffi(true) sets linkage to Shared, which fails linkage validation
        let result = validator.validate(&intent);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ValidationError::UnsupportedLinkage { .. }
        ));
    }

    #[test]
    fn test_backend_validator_cross_compile() {
        let mut caps = mock_capabilities();
        caps.platform.cross_compile = false;

        let validator = BackendValidator::new(&caps);
        let intent = BuildIntent::new()
            .with_target_triple(crate::builder::shim::intent::TargetTriple::new(
                "aarch64-unknown-linux-gnu",
            ));

        // This test depends on the host platform
        // On non-aarch64-linux, this should fail
        #[cfg(not(all(target_arch = "aarch64", target_os = "linux")))]
        {
            let result = validator.validate(&intent);
            assert!(result.is_err());
        }
    }

    #[test]
    fn test_validation_error_multiple() {
        let errors = vec![
            ValidationError::CrossCompileNotSupported,
            ValidationError::PhaseNotSupported {
                phase: "test".to_string(),
            },
        ];

        let err = ValidationError::multiple(errors);
        assert!(matches!(err, ValidationError::Multiple(_)));

        let messages = err.messages();
        assert_eq!(messages.len(), 2);
    }

    #[test]
    fn test_validation_error_single() {
        let errors = vec![ValidationError::CrossCompileNotSupported];
        let err = ValidationError::multiple(errors);
        assert!(matches!(err, ValidationError::CrossCompileNotSupported));
    }
}
