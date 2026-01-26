//! Custom backend shim - wraps user-defined build commands.
//!
//! This shim provides the BackendShim interface for custom build scripts.

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::builder::shim::capabilities::*;
use crate::builder::shim::defaults::BackendDefaults;
use crate::builder::shim::intent::BackendOptions;
use crate::builder::shim::intent::BuildIntent;
use crate::builder::shim::trait_def::*;
use crate::builder::shim::validation::ValidationError;

/// Custom backend shim.
///
/// Wraps user-defined build commands with the BackendShim interface.
/// This is the most flexible but least capable backend - users must
/// provide their own surface definitions.
pub struct CustomShim {
    capabilities: BackendCapabilities,
    defaults: BackendDefaults,
}

impl CustomShim {
    /// Create a new Custom backend shim.
    pub fn new() -> Self {
        CustomShim {
            capabilities: Self::build_capabilities(),
            defaults: BackendDefaults::custom(),
        }
    }

    fn build_capabilities() -> BackendCapabilities {
        let identity = BackendIdentity {
            id: BackendId::Custom,
            shim_version: semver::Version::new(1, 0, 0),
            backend_version_req: None, // No external tool - users provide everything
        };

        let mut caps = BackendCapabilities::new(identity);

        // Phase support - all optional for custom
        caps.phases = PhaseCapabilities {
            configure: PhaseSupport::Optional,
            build: PhaseSupport::Required, // Must have build
            test: PhaseSupport::Optional,
            install: PhaseSupport::Optional,
            clean: PhaseSupport::Optional,
        };

        // Platform support - conservative defaults
        caps.platform = PlatformCapabilities {
            cross_compile: false, // Unknown, assume host-only
            sysroot_support: false,
            toolchain_file_support: false,
            host_only: true, // Safe default
        };

        // Artifact support - allow all (user controls this)
        caps.artifacts = ArtifactCapabilities {
            static_lib: true,
            shared_lib: true,
            executable: true,
            header_only: true,
            test_binary: true,
            static_shared_single_invocation: false,
            deterministic_install: false, // Unknown
        };

        // Linkage support
        caps.linkage = LinkageCapabilities {
            static_linking: true,
            shared_linking: true,
            symbol_visibility_control: false,   // Unknown
            rpath_handling: RpathSupport::None, // Unknown
            import_lib_generation: false,       // Unknown
            runtime_bundle: false,              // Not automatically supported
        };

        // Dependency injection - primarily via environment
        let mut methods = HashSet::new();
        methods.insert(InjectionMethod::EnvVars);

        let mut formats = HashSet::new();
        formats.insert(DependencyFormat::IncludeLib);

        caps.dependency_injection = DependencyInjection {
            supported_methods: methods,
            consumable_formats: formats,
            transitive_handling: TransitiveHandling::FlattenedByHarbour,
        };

        // Export discovery - NOT supported, user must provide surface
        caps.export_discovery = ExportDiscoveryContract {
            discovery: ExportDiscovery::NotSupported,
            requires_install: false,
        };

        // Install contract
        caps.install = InstallContract {
            requires_install_step: false,
            supports_install_prefix: false, // Unknown
            deterministic_install: false,
        };

        // Caching - limited, since we don't know build semantics
        let mut factors = HashSet::new();
        factors.insert(CacheInputFactor::Source);
        factors.insert(CacheInputFactor::Options);

        caps.caching = CachingContract {
            input_factors: factors,
            hermetic_builds: false, // Unknown
            out_of_tree: false,     // Unknown
            install_determinism: false,
        };

        caps
    }

    /// Execute a custom command.
    fn execute_command(
        &self,
        program: &str,
        args: &[String],
        cwd: Option<&PathBuf>,
        env: &[(String, String)],
        ctx: &BuildContext,
    ) -> Result<()> {
        let mut cmd = Command::new(program);
        cmd.args(args);

        // Set working directory
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        } else {
            cmd.current_dir(&ctx.package_root);
        }

        // Set environment variables
        for (key, value) in env {
            cmd.env(key, value);
        }

        // Standard environment
        cmd.env("HARBOUR_BUILD_DIR", &ctx.build_dir);
        cmd.env("HARBOUR_INSTALL_PREFIX", &ctx.install_prefix);
        cmd.env("HARBOUR_PACKAGE_ROOT", &ctx.package_root);

        if ctx.release {
            cmd.env("HARBOUR_RELEASE", "1");
        }

        if let Some(jobs) = ctx.jobs {
            cmd.env("HARBOUR_JOBS", jobs.to_string());
        }

        tracing::debug!("Custom command: {} {:?}", program, args);

        let status = cmd
            .status()
            .with_context(|| format!("failed to execute custom command: {}", program))?;

        if !status.success() {
            bail!(
                "custom command '{}' failed with exit code: {:?}",
                program,
                status.code()
            );
        }

        Ok(())
    }

    /// Get commands from backend options.
    fn get_commands(&self, opts: &BackendOptions, phase: &str) -> Vec<CustomCommandSpec> {
        let mut commands = Vec::new();

        // Look for phase-specific commands
        // Format: configure = [{ program = "...", args = [...] }]
        // Or: configure_program = "...", configure_args = [...]
        if let Some(arr) = opts.get_array(phase) {
            for item in arr {
                if let Some(table) = item.as_table() {
                    if let Some(program) = table.get("program").and_then(|v| v.as_str()) {
                        let args = table
                            .get("args")
                            .and_then(|v| v.as_array())
                            .map(|a| {
                                a.iter()
                                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                    .collect()
                            })
                            .unwrap_or_default();

                        let cwd = table.get("cwd").and_then(|v| v.as_str()).map(PathBuf::from);

                        let env = table
                            .get("env")
                            .and_then(|v| v.as_table())
                            .map(|t| {
                                t.iter()
                                    .filter_map(|(k, v)| {
                                        v.as_str().map(|s| (k.clone(), s.to_string()))
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();

                        commands.push(CustomCommandSpec {
                            program: program.to_string(),
                            args,
                            cwd,
                            env,
                        });
                    }
                }
            }
        }

        // Also support simple form: build_program = "make", build_args = ["-j4"]
        let program_key = format!("{}_program", phase);
        let args_key = format!("{}_args", phase);

        if let Some(program) = opts.get_string(&program_key) {
            let args = opts
                .get_array(&args_key)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();

            commands.push(CustomCommandSpec {
                program: program.to_string(),
                args,
                cwd: None,
                env: Vec::new(),
            });
        }

        commands
    }
}

/// Specification for a custom command.
struct CustomCommandSpec {
    program: String,
    args: Vec<String>,
    cwd: Option<PathBuf>,
    env: Vec<(String, String)>,
}

impl Default for CustomShim {
    fn default() -> Self {
        Self::new()
    }
}

impl BackendShim for CustomShim {
    fn capabilities(&self) -> &BackendCapabilities {
        &self.capabilities
    }

    fn defaults(&self) -> &BackendDefaults {
        &self.defaults
    }

    fn availability(&self) -> Result<BackendAvailability> {
        // Custom backend is always available - users provide everything
        Ok(BackendAvailability::AlwaysAvailable)
    }

    fn configure(&self, ctx: &BuildContext, opts: &BackendOptions) -> Result<ConfigureResult> {
        let commands = self.get_commands(opts, "configure");

        if commands.is_empty() {
            return Ok(ConfigureResult {
                skipped: true,
                generator: None,
                warnings: Vec::new(),
            });
        }

        for cmd in &commands {
            self.execute_command(&cmd.program, &cmd.args, cmd.cwd.as_ref(), &cmd.env, ctx)?;
        }

        Ok(ConfigureResult {
            skipped: false,
            generator: Some("custom".to_string()),
            warnings: Vec::new(),
        })
    }

    fn build(&self, ctx: &BuildContext, opts: &BackendOptions) -> Result<BuildResult> {
        let commands = self.get_commands(opts, "build");

        if commands.is_empty() {
            bail!(
                "custom backend requires build commands. Add [targets.NAME.backend.options.build] \
                 with program and args, or set build_program/build_args."
            );
        }

        for cmd in &commands {
            self.execute_command(&cmd.program, &cmd.args, cmd.cwd.as_ref(), &cmd.env, ctx)?;
        }

        Ok(BuildResult::default())
    }

    fn test(&self, ctx: &BuildContext, opts: &BackendOptions) -> Result<TestResult> {
        let commands = self.get_commands(opts, "test");

        if commands.is_empty() {
            return Ok(TestResult::default());
        }

        for cmd in &commands {
            self.execute_command(&cmd.program, &cmd.args, cmd.cwd.as_ref(), &cmd.env, ctx)?;
        }

        // We can't parse test results from arbitrary commands
        Ok(TestResult::default())
    }

    fn install(&self, ctx: &BuildContext, opts: &BackendOptions) -> Result<InstallResult> {
        let commands = self.get_commands(opts, "install");

        if commands.is_empty() {
            return Ok(InstallResult::default());
        }

        for cmd in &commands {
            self.execute_command(&cmd.program, &cmd.args, cmd.cwd.as_ref(), &cmd.env, ctx)?;
        }

        Ok(InstallResult::default())
    }

    fn clean(&self, ctx: &BuildContext, opts: &CleanOptions) -> Result<()> {
        if opts.dry_run {
            tracing::info!("Would clean custom build");
            return Ok(());
        }

        // For custom backends, we just remove the build directory
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

    fn discover_exports(&self, _ctx: &BuildContext) -> Result<Option<DiscoveredSurface>> {
        // Custom backends don't support automatic export discovery
        // Users must provide surface definitions in their manifest
        Ok(None)
    }

    fn doctor(&self, ctx: &BuildContext) -> Result<DoctorReport> {
        let mut report = DoctorReport {
            ok: true,
            checks: Vec::new(),
            suggestions: Vec::new(),
        };

        // Check that build commands are defined
        // (This would need access to the target's backend options)
        report.checks.push(DoctorCheck {
            name: "custom_backend".to_string(),
            passed: true,
            message: "Custom backend ready".to_string(),
        });

        report.suggestions.push(
            "Custom backends require explicit build commands. \
             Ensure you have defined build_program or build commands."
                .to_string(),
        );

        report.suggestions.push(
            "Custom backends don't support automatic surface discovery. \
             Define [targets.NAME.surface] manually."
                .to_string(),
        );

        // Check package root exists
        report.checks.push(DoctorCheck {
            name: "package_root".to_string(),
            passed: ctx.package_root.exists(),
            message: format!("Package root: {}", ctx.package_root.display()),
        });

        if !ctx.package_root.exists() {
            report.ok = false;
        }

        Ok(report)
    }

    fn validate_extra(
        &self,
        _intent: &BuildIntent,
        opts: &BackendOptions,
    ) -> Result<(), ValidationError> {
        // Validate that build commands are defined
        let build_commands = self.get_commands(opts, "build");

        if build_commands.is_empty() {
            return Err(ValidationError::BackendOptionInvalid {
                message: "custom backend requires build commands. \
                          Define [targets.NAME.backend.options.build] or build_program."
                    .to_string(),
            });
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_custom_shim_creation() {
        let shim = CustomShim::new();
        assert_eq!(shim.capabilities().identity.id, BackendId::Custom);
    }

    #[test]
    fn test_custom_availability() {
        let shim = CustomShim::new();
        let avail = shim.availability().unwrap();
        assert!(avail.is_available());
    }

    #[test]
    fn test_custom_capabilities() {
        let shim = CustomShim::new();
        let caps = shim.capabilities();

        // Custom has limited capabilities
        assert!(!caps.platform.cross_compile);
        assert!(caps.platform.host_only);
        assert_eq!(
            caps.export_discovery.discovery,
            ExportDiscovery::NotSupported
        );
        assert_eq!(caps.phases.configure, PhaseSupport::Optional);
    }

    #[test]
    fn test_custom_get_commands() {
        let shim = CustomShim::new();
        let mut opts = BackendOptions::new();

        // Set up build commands using simple form
        opts.set_string("build_program", "make");

        let commands = shim.get_commands(&opts, "build");
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].program, "make");
    }

    #[test]
    fn test_custom_validate_extra() {
        let shim = CustomShim::new();
        let intent = BuildIntent::new();

        // Empty options should fail validation
        let opts = BackendOptions::new();
        let result = shim.validate_extra(&intent, &opts);
        assert!(result.is_err());

        // With build program should pass
        let mut opts = BackendOptions::new();
        opts.set_string("build_program", "make");
        let result = shim.validate_extra(&intent, &opts);
        assert!(result.is_ok());
    }
}
