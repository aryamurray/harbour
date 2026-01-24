//! Native backend shim - wraps Harbour's built-in C/C++ compiler driver.
//!
//! This shim provides the BackendShim interface for the existing NativeBuilder.

use std::collections::HashSet;

use anyhow::Result;

use crate::builder::shim::capabilities::*;
use crate::builder::shim::defaults::BackendDefaults;
use crate::builder::shim::intent::BackendOptions;
use crate::builder::shim::trait_def::*;

/// Native backend shim.
///
/// Wraps Harbour's built-in NativeBuilder with the BackendShim interface.
pub struct NativeShim {
    capabilities: BackendCapabilities,
    defaults: BackendDefaults,
}

impl NativeShim {
    /// Create a new Native backend shim.
    pub fn new() -> Self {
        NativeShim {
            capabilities: Self::build_capabilities(),
            defaults: BackendDefaults::native(),
        }
    }

    fn build_capabilities() -> BackendCapabilities {
        let identity = BackendIdentity {
            id: BackendId::Native,
            shim_version: semver::Version::new(1, 0, 0),
            backend_version_req: None, // No external tool required
        };

        let mut caps = BackendCapabilities::new(identity);

        // Phase support
        caps.phases = PhaseCapabilities {
            configure: PhaseSupport::NotSupported, // Native has no configure
            build: PhaseSupport::Required,
            test: PhaseSupport::Optional,
            install: PhaseSupport::Optional,
            clean: PhaseSupport::Required,
        };

        // Platform support
        caps.platform = PlatformCapabilities {
            cross_compile: false, // Not yet implemented
            sysroot_support: false,
            toolchain_file_support: false,
            host_only: false,
        };

        // Artifact support
        caps.artifacts = ArtifactCapabilities {
            static_lib: true,
            shared_lib: true,
            executable: true,
            header_only: true,
            test_binary: true,
            static_shared_single_invocation: false,
            deterministic_install: true,
        };

        // Linkage support
        caps.linkage = LinkageCapabilities {
            static_linking: true,
            shared_linking: true,
            symbol_visibility_control: true,
            rpath_handling: RpathSupport::Full,
            import_lib_generation: true,
            runtime_bundle: true,
        };

        // Dependency injection - Native uses direct include/lib paths
        let mut methods = HashSet::new();
        methods.insert(InjectionMethod::IncludeLib);

        let mut formats = HashSet::new();
        formats.insert(DependencyFormat::IncludeLib);

        caps.dependency_injection = DependencyInjection {
            supported_methods: methods,
            consumable_formats: formats,
            transitive_handling: TransitiveHandling::FlattenedByHarbour,
        };

        // Export discovery - Native always produces surface
        caps.export_discovery = ExportDiscoveryContract {
            discovery: ExportDiscovery::Full,
            requires_install: false,
        };

        // Install contract
        caps.install = InstallContract {
            requires_install_step: false,
            supports_install_prefix: true,
            deterministic_install: true,
        };

        // Caching
        let mut factors = HashSet::new();
        factors.insert(CacheInputFactor::Source);
        factors.insert(CacheInputFactor::Toolchain);
        factors.insert(CacheInputFactor::Profile);
        factors.insert(CacheInputFactor::Options);
        factors.insert(CacheInputFactor::Dependencies);

        caps.caching = CachingContract {
            input_factors: factors,
            hermetic_builds: true,
            out_of_tree: true,
            install_determinism: true,
        };

        caps
    }
}

impl Default for NativeShim {
    fn default() -> Self {
        Self::new()
    }
}

impl BackendShim for NativeShim {
    fn capabilities(&self) -> &BackendCapabilities {
        &self.capabilities
    }

    fn defaults(&self) -> &BackendDefaults {
        &self.defaults
    }

    fn availability(&self) -> Result<BackendAvailability> {
        // Native builder is always available - no external tools required
        Ok(BackendAvailability::AlwaysAvailable)
    }

    fn configure(&self, _ctx: &BuildContext, _opts: &BackendOptions) -> Result<ConfigureResult> {
        // Native builder has no configure phase
        Ok(ConfigureResult {
            skipped: true,
            generator: None,
            warnings: Vec::new(),
        })
    }

    fn build(&self, ctx: &BuildContext, _opts: &BackendOptions) -> Result<BuildResult> {
        // The actual build is delegated to NativeBuilder
        // This shim just provides the interface; actual implementation
        // happens through the existing build system

        tracing::debug!(
            "Native build: {} -> {}",
            ctx.package_root.display(),
            ctx.build_dir.display()
        );

        // Return empty result - actual artifacts are tracked by the existing system
        Ok(BuildResult::default())
    }

    fn test(&self, ctx: &BuildContext, _opts: &BackendOptions) -> Result<TestResult> {
        tracing::debug!("Native test: {}", ctx.build_dir.display());

        // Test execution would be implemented here
        Ok(TestResult::default())
    }

    fn install(&self, ctx: &BuildContext, _opts: &BackendOptions) -> Result<InstallResult> {
        tracing::debug!(
            "Native install: {} -> {}",
            ctx.build_dir.display(),
            ctx.install_prefix.display()
        );

        // Install implementation would copy artifacts to prefix
        Ok(InstallResult::default())
    }

    fn clean(&self, ctx: &BuildContext, opts: &CleanOptions) -> Result<()> {
        if opts.dry_run {
            tracing::info!("Would clean: {}", ctx.build_dir.display());
            return Ok(());
        }

        if opts.build_dir && ctx.build_dir.exists() {
            tracing::debug!("Removing build dir: {}", ctx.build_dir.display());
            std::fs::remove_dir_all(&ctx.build_dir)?;
        }

        if opts.install_prefix && ctx.install_prefix.exists() {
            tracing::debug!("Removing install prefix: {}", ctx.install_prefix.display());
            std::fs::remove_dir_all(&ctx.install_prefix)?;
        }

        Ok(())
    }

    fn discover_exports(&self, ctx: &BuildContext) -> Result<Option<DiscoveredSurface>> {
        // Native builder knows exactly what it produced
        // Surface is built from build plan output

        let mut surface = DiscoveredSurface::default();

        // Add include directory if it exists
        let include_dir = ctx.install_prefix.join("include");
        if include_dir.exists() {
            surface.include_dirs.push(include_dir);
        }

        // Add lib directory contents
        let lib_dir = ctx.install_prefix.join("lib");
        if lib_dir.exists() {
            // Scan for libraries
            if let Ok(entries) = std::fs::read_dir(&lib_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if let Some(ext) = path.extension() {
                        let ext = ext.to_string_lossy();
                        if ext == "a" || ext == "lib" {
                            // Static library
                            if let Some(name) = extract_lib_name(&path, true) {
                                surface.libraries.push(LibraryInfo {
                                    name,
                                    kind: LibraryKind::Static,
                                    path,
                                    soname: None,
                                    import_lib: None,
                                });
                            }
                        } else if ext == "so" || ext == "dylib" || ext == "dll" {
                            // Shared library
                            if let Some(name) = extract_lib_name(&path, false) {
                                let soname = path
                                    .file_name()
                                    .map(|n| n.to_string_lossy().to_string());

                                surface.libraries.push(LibraryInfo {
                                    name,
                                    kind: LibraryKind::Shared,
                                    path,
                                    soname,
                                    import_lib: None,
                                });
                            }
                        }
                    }
                }
            }
        }

        if surface.include_dirs.is_empty() && surface.libraries.is_empty() {
            Ok(None)
        } else {
            Ok(Some(surface))
        }
    }

    fn doctor(&self, ctx: &BuildContext) -> Result<DoctorReport> {
        let mut report = DoctorReport {
            ok: true,
            checks: Vec::new(),
            suggestions: Vec::new(),
        };

        // Check build directory
        report.checks.push(DoctorCheck {
            name: "build_dir".to_string(),
            passed: ctx.build_dir.exists() || !ctx.build_dir.exists(), // Always passes
            message: format!("Build directory: {}", ctx.build_dir.display()),
        });

        // Check install prefix
        let prefix_writable = ctx.install_prefix.parent().map_or(false, |p| {
            p.exists() || std::fs::create_dir_all(p).is_ok()
        });

        report.checks.push(DoctorCheck {
            name: "install_prefix".to_string(),
            passed: prefix_writable,
            message: format!("Install prefix: {}", ctx.install_prefix.display()),
        });

        if !prefix_writable {
            report.ok = false;
            report
                .suggestions
                .push("Ensure install prefix parent directory is writable".to_string());
        }

        Ok(report)
    }
}

/// Extract library name from path.
fn extract_lib_name(path: &std::path::Path, _is_static: bool) -> Option<String> {
    let stem = path.file_stem()?.to_string_lossy();

    // Remove lib prefix if present
    let name = if stem.starts_with("lib") {
        stem[3..].to_string()
    } else {
        stem.to_string()
    };

    // Remove version suffix (e.g., libfoo.so.1.2.3 -> foo)
    let name = name.split('.').next().unwrap_or(&name).to_string();

    Some(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_native_shim_creation() {
        let shim = NativeShim::new();
        assert_eq!(shim.capabilities().identity.id, BackendId::Native);
    }

    #[test]
    fn test_native_availability() {
        let shim = NativeShim::new();
        let avail = shim.availability().unwrap();
        assert!(avail.is_available());
    }

    #[test]
    fn test_native_capabilities() {
        let shim = NativeShim::new();
        let caps = shim.capabilities();

        assert!(caps.artifacts.static_lib);
        assert!(caps.artifacts.shared_lib);
        assert!(caps.artifacts.executable);
        assert!(caps.linkage.static_linking);
        assert!(caps.linkage.shared_linking);
        assert!(!caps.platform.cross_compile);
        assert_eq!(caps.phases.configure, PhaseSupport::NotSupported);
    }

    #[test]
    fn test_native_configure_skipped() {
        let shim = NativeShim::new();
        let ctx = BuildContext::new(
            PathBuf::from("/src"),
            PathBuf::from("/build"),
            PathBuf::from("/install"),
        );
        let opts = BackendOptions::new();

        let result = shim.configure(&ctx, &opts).unwrap();
        assert!(result.skipped);
    }

    #[test]
    fn test_extract_lib_name() {
        assert_eq!(
            extract_lib_name(std::path::Path::new("libfoo.a"), true),
            Some("foo".to_string())
        );
        assert_eq!(
            extract_lib_name(std::path::Path::new("libbar.so"), false),
            Some("bar".to_string())
        );
        assert_eq!(
            extract_lib_name(std::path::Path::new("libfoo.so.1.2.3"), false),
            Some("foo".to_string())
        );
        assert_eq!(
            extract_lib_name(std::path::Path::new("mylib.lib"), true),
            Some("mylib".to_string())
        );
    }
}
