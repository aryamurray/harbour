//! Meson backend shim - wraps Meson-based builds.
//!
//! This shim provides the BackendShim interface for Meson projects.

use std::collections::HashSet;
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::builder::shim::capabilities::*;
use crate::builder::shim::defaults::BackendDefaults;
use crate::builder::shim::intent::BackendOptions;
use crate::builder::shim::trait_def::*;

/// Meson backend shim.
///
/// Wraps Meson-based builds with the BackendShim interface.
pub struct MesonShim {
    capabilities: BackendCapabilities,
    defaults: BackendDefaults,
    /// Cached availability (lazily computed)
    cached_availability: std::sync::OnceLock<BackendAvailability>,
}

impl MesonShim {
    /// Create a new Meson backend shim.
    pub fn new() -> Self {
        MesonShim {
            capabilities: Self::build_capabilities(),
            defaults: BackendDefaults::meson(),
            cached_availability: std::sync::OnceLock::new(),
        }
    }

    fn build_capabilities() -> BackendCapabilities {
        let identity = BackendIdentity {
            id: BackendId::Meson,
            shim_version: semver::Version::new(1, 0, 0),
            backend_version_req: Some(semver::VersionReq::parse(">=0.50").unwrap()),
        };

        let mut caps = BackendCapabilities::new(identity);

        // Phase support
        caps.phases = PhaseCapabilities {
            configure: PhaseSupport::Required, // Meson requires configure (meson setup)
            build: PhaseSupport::Required,
            test: PhaseSupport::Optional, // via meson test
            install: PhaseSupport::Optional,
            clean: PhaseSupport::Required,
        };

        // Platform support - Meson excels at cross-compilation
        caps.platform = PlatformCapabilities {
            cross_compile: true,
            sysroot_support: true,
            toolchain_file_support: true, // Meson cross files
            host_only: false,
        };

        // Artifact support
        caps.artifacts = ArtifactCapabilities {
            static_lib: true,
            shared_lib: true,
            executable: true,
            header_only: true,
            test_binary: true,
            static_shared_single_invocation: true, // Meson can do both
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

        // Dependency injection
        let mut methods = HashSet::new();
        methods.insert(InjectionMethod::PrefixPath);
        methods.insert(InjectionMethod::MesonWrap);
        methods.insert(InjectionMethod::EnvVars);

        let mut formats = HashSet::new();
        formats.insert(DependencyFormat::PkgConfig);
        formats.insert(DependencyFormat::CMakeConfig);

        caps.dependency_injection = DependencyInjection {
            supported_methods: methods,
            consumable_formats: formats,
            transitive_handling: TransitiveHandling::ViaPrefix,
        };

        // Export discovery - Meson may have pkg-config files
        caps.export_discovery = ExportDiscoveryContract {
            discovery: ExportDiscovery::Optional,
            requires_install: true, // Need install to get pkg-config files
        };

        // Install contract
        caps.install = InstallContract {
            requires_install_step: true,
            supports_install_prefix: true, // --prefix
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

    /// Detect Meson version.
    fn detect_meson_version() -> Result<semver::Version> {
        let output = Command::new("meson")
            .arg("--version")
            .output()
            .context("failed to run meson --version")?;

        if !output.status.success() {
            bail!("meson --version failed");
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        // Meson outputs just the version number, e.g., "1.3.0"
        let version_str = stdout.trim();
        // Handle versions like "1.3.0" or "1.3.0.dev1"
        let clean_version = version_str
            .split(|c: char| !c.is_ascii_digit() && c != '.')
            .next()
            .unwrap_or(version_str);

        // Parse the version - handle versions with less than 3 parts
        let parts: Vec<&str> = clean_version.split('.').collect();
        let major = parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
        let minor = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        let patch = parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);

        Ok(semver::Version::new(major, minor, patch))
    }

    /// Build Meson configure arguments.
    #[allow(clippy::vec_init_then_push)]
    fn build_configure_args(&self, ctx: &BuildContext, opts: &BackendOptions) -> Vec<String> {
        let mut args = Vec::new();

        // Meson setup command: meson setup <builddir> <sourcedir>
        args.push("setup".to_string());
        args.push(ctx.build_dir.display().to_string());
        args.push(ctx.package_root.display().to_string());

        // Build type
        let build_type = if ctx.release { "release" } else { "debug" };
        args.push(format!("--buildtype={}", build_type));

        // Install prefix
        args.push(format!("--prefix={}", ctx.install_prefix.display()));

        // Add user-defined Meson options
        for (key, value) in opts.iter() {
            // Convert TOML values to Meson -D options
            let meson_value = match value {
                toml::Value::Boolean(b) => if *b { "true" } else { "false" }.to_string(),
                toml::Value::Integer(i) => i.to_string(),
                toml::Value::Float(f) => f.to_string(),
                toml::Value::String(s) => s.clone(),
                toml::Value::Array(arr) => {
                    // Convert array to Meson array format: ['item1', 'item2']
                    let items: Vec<String> = arr
                        .iter()
                        .filter_map(|v| v.as_str())
                        .map(|s| format!("'{}'", s))
                        .collect();
                    format!("[{}]", items.join(", "))
                }
                _ => continue,
            };

            args.push(format!("-D{}={}", key, meson_value));
        }

        args
    }
}

impl Default for MesonShim {
    fn default() -> Self {
        Self::new()
    }
}

impl BackendShim for MesonShim {
    fn capabilities(&self) -> &BackendCapabilities {
        &self.capabilities
    }

    fn defaults(&self) -> &BackendDefaults {
        &self.defaults
    }

    fn availability(&self) -> Result<BackendAvailability> {
        // Return cached result if available
        if let Some(avail) = self.cached_availability.get() {
            return Ok(avail.clone());
        }

        // Detect Meson
        let availability = match Self::detect_meson_version() {
            Ok(version) => {
                // Check version requirement
                if let Some(ref req) = self.capabilities.identity.backend_version_req {
                    if !req.matches(&version) {
                        BackendAvailability::VersionTooOld {
                            found: version,
                            required: req.clone(),
                        }
                    } else {
                        BackendAvailability::Available { version }
                    }
                } else {
                    BackendAvailability::Available { version }
                }
            }
            Err(_) => BackendAvailability::NotInstalled {
                tool: "meson".to_string(),
                install_hint: get_meson_install_hint(),
            },
        };

        // Cache the result
        let _ = self.cached_availability.set(availability.clone());

        Ok(availability)
    }

    fn configure(&self, ctx: &BuildContext, opts: &BackendOptions) -> Result<ConfigureResult> {
        let args = self.build_configure_args(ctx, opts);

        tracing::debug!("Meson configure: meson {}", args.join(" "));

        let status = Command::new("meson")
            .args(&args)
            .current_dir(&ctx.package_root)
            .status()
            .context("failed to run meson setup")?;

        if !status.success() {
            bail!("meson setup failed with exit code: {:?}", status.code());
        }

        Ok(ConfigureResult {
            skipped: false,
            generator: Some("ninja".to_string()), // Meson always uses Ninja by default
            warnings: Vec::new(),
        })
    }

    fn build(&self, ctx: &BuildContext, opts: &BackendOptions) -> Result<BuildResult> {
        let mut args = vec!["compile".to_string(), "-C".to_string(), ctx.build_dir.display().to_string()];

        // Parallel jobs
        if let Some(jobs) = ctx.jobs {
            args.push("-j".to_string());
            args.push(jobs.to_string());
        }

        // Verbose
        if ctx.verbose {
            args.push("-v".to_string());
        }

        // Check if there are specific targets to build from options
        if let Some(targets) = opts.options.get("targets") {
            if let Some(arr) = targets.as_array() {
                for target in arr {
                    if let Some(t) = target.as_str() {
                        args.push(t.to_string());
                    }
                }
            }
        }

        tracing::debug!("Meson build: meson {}", args.join(" "));

        let status = Command::new("meson")
            .args(&args)
            .status()
            .context("failed to run meson compile")?;

        if !status.success() {
            bail!("meson compile failed with exit code: {:?}", status.code());
        }

        Ok(BuildResult::default())
    }

    fn test(&self, ctx: &BuildContext, _opts: &BackendOptions) -> Result<TestResult> {
        let mut args = vec![
            "test".to_string(),
            "-C".to_string(),
            ctx.build_dir.display().to_string(),
        ];

        // Parallel
        if let Some(jobs) = ctx.jobs {
            args.push("--num-processes".to_string());
            args.push(jobs.to_string());
        }

        // Verbose
        if ctx.verbose {
            args.push("-v".to_string());
        }

        tracing::debug!("Meson test: meson {}", args.join(" "));

        let output = Command::new("meson")
            .args(&args)
            .output()
            .context("failed to run meson test")?;

        // Parse meson test output for results
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut result = TestResult::default();

        // Meson outputs "Ok: X" and "Fail: Y" in its summary
        for line in stdout.lines() {
            let line = line.trim();
            if line.starts_with("Ok:") {
                if let Some(count) = line.strip_prefix("Ok:").and_then(|s| s.trim().parse().ok()) {
                    result.passed = count;
                }
            } else if line.starts_with("Fail:") {
                if let Some(count) = line.strip_prefix("Fail:").and_then(|s| s.trim().parse().ok())
                {
                    result.failed = count;
                }
            } else if line.starts_with("Skip:") {
                if let Some(count) = line.strip_prefix("Skip:").and_then(|s| s.trim().parse().ok())
                {
                    result.skipped = count;
                }
            }
        }

        result.total = result.passed + result.failed + result.skipped;

        if !output.status.success() && result.failed == 0 {
            // meson test failed but we couldn't parse failure count
            result.failed = 1;
        }

        Ok(result)
    }

    fn install(&self, ctx: &BuildContext, _opts: &BackendOptions) -> Result<InstallResult> {
        let args = vec![
            "install".to_string(),
            "-C".to_string(),
            ctx.build_dir.display().to_string(),
            "--destdir".to_string(),
            ctx.install_prefix.display().to_string(),
        ];

        tracing::debug!("Meson install: meson {}", args.join(" "));

        let status = Command::new("meson")
            .args(&args)
            .status()
            .context("failed to run meson install")?;

        if !status.success() {
            bail!(
                "meson install failed with exit code: {:?}",
                status.code()
            );
        }

        Ok(InstallResult::default())
    }

    fn clean(&self, ctx: &BuildContext, opts: &CleanOptions) -> Result<()> {
        if opts.dry_run {
            tracing::info!("Would clean: {}", ctx.build_dir.display());
            return Ok(());
        }

        if opts.build_dir && ctx.build_dir.exists() {
            // Try ninja -C <builddir> clean first (Meson uses Ninja by default)
            let status = Command::new("ninja")
                .args(["-C", &ctx.build_dir.display().to_string(), "clean"])
                .status();

            if status.is_err() || !status.unwrap().success() {
                // Fall back to removing the directory
                tracing::debug!("Removing build dir: {}", ctx.build_dir.display());
                std::fs::remove_dir_all(&ctx.build_dir)?;
            }
        }

        if opts.install_prefix && ctx.install_prefix.exists() {
            tracing::debug!("Removing install prefix: {}", ctx.install_prefix.display());
            std::fs::remove_dir_all(&ctx.install_prefix)?;
        }

        Ok(())
    }

    fn discover_exports(&self, ctx: &BuildContext) -> Result<Option<DiscoveredSurface>> {
        let mut surface = DiscoveredSurface::default();

        // Add include directories
        let include_dir = ctx.install_prefix.join("include");
        if include_dir.exists() {
            surface.include_dirs.push(include_dir);
        }

        // Scan for libraries
        for lib_subdir in ["lib", "lib64", "lib/x86_64-linux-gnu"] {
            let lib_dir = ctx.install_prefix.join(lib_subdir);
            if lib_dir.exists() {
                if let Ok(entries) = std::fs::read_dir(&lib_dir) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.is_file() {
                            if let Some(ext) = path.extension() {
                                let ext = ext.to_string_lossy();
                                if ext == "a"
                                    || ext == "lib"
                                    || ext == "so"
                                    || ext == "dylib"
                                    || ext == "dll"
                                {
                                    if let Some(name) = extract_lib_name(&path) {
                                        let kind = if ext == "a" || ext == "lib" {
                                            LibraryKind::Static
                                        } else {
                                            LibraryKind::Shared
                                        };

                                        surface.libraries.push(LibraryInfo {
                                            name,
                                            kind,
                                            path,
                                            soname: None,
                                            import_lib: None,
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Look for pkg-config files
        let pkgconfig_dir = ctx.install_prefix.join("lib").join("pkgconfig");
        if pkgconfig_dir.exists() {
            tracing::debug!("Found pkg-config files in {}", pkgconfig_dir.display());
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

        // Check Meson availability
        let meson_check = match Self::detect_meson_version() {
            Ok(version) => {
                let req = self.capabilities.identity.backend_version_req.as_ref();
                let passed = req.map_or(true, |r| r.matches(&version));

                if !passed {
                    report.ok = false;
                    report.suggestions.push(format!(
                        "Upgrade Meson to version {} or higher",
                        req.unwrap()
                    ));
                }

                DoctorCheck {
                    name: "meson".to_string(),
                    passed,
                    message: format!("Meson version {}", version),
                }
            }
            Err(e) => {
                report.ok = false;
                report.suggestions.push(get_meson_install_hint());

                DoctorCheck {
                    name: "meson".to_string(),
                    passed: false,
                    message: format!("Meson not found: {}", e),
                }
            }
        };
        report.checks.push(meson_check);

        // Check for Ninja (required by Meson)
        let ninja_check = match Command::new("ninja").arg("--version").output() {
            Ok(output) if output.status.success() => {
                let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
                DoctorCheck {
                    name: "ninja".to_string(),
                    passed: true,
                    message: format!("Ninja version {}", version),
                }
            }
            _ => {
                report.ok = false;
                report.suggestions.push(
                    "Install Ninja (required by Meson): https://ninja-build.org/".to_string(),
                );
                DoctorCheck {
                    name: "ninja".to_string(),
                    passed: false,
                    message: "Ninja not found (required by Meson)".to_string(),
                }
            }
        };
        report.checks.push(ninja_check);

        // Check build directory
        if ctx.build_dir.exists() {
            let meson_build = ctx.build_dir.join("meson-private");
            report.checks.push(DoctorCheck {
                name: "meson_build".to_string(),
                passed: meson_build.exists(),
                message: if meson_build.exists() {
                    "Meson build directory configured".to_string()
                } else {
                    "Build directory exists but not configured".to_string()
                },
            });
        }

        Ok(report)
    }
}

/// Extract library name from path.
fn extract_lib_name(path: &std::path::Path) -> Option<String> {
    let stem = path.file_stem()?.to_string_lossy();

    // Remove lib prefix if present
    let name = stem
        .strip_prefix("lib")
        .map(|s| s.to_string())
        .unwrap_or_else(|| stem.to_string());

    // Remove version suffixes (e.g., libfoo.so.1.2.3 -> foo)
    let name = name.split('.').next().unwrap_or(&name).to_string();

    Some(name)
}

/// Get platform-specific Meson install hint.
fn get_meson_install_hint() -> String {
    #[cfg(target_os = "linux")]
    {
        "Install Meson: pip install meson, apt install meson, or https://mesonbuild.com/Getting-meson.html".to_string()
    }
    #[cfg(target_os = "macos")]
    {
        "Install Meson: brew install meson, pip install meson, or https://mesonbuild.com/Getting-meson.html".to_string()
    }
    #[cfg(target_os = "windows")]
    {
        "Install Meson: pip install meson, winget install meson, or https://mesonbuild.com/Getting-meson.html".to_string()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        "Install Meson from https://mesonbuild.com/Getting-meson.html".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_meson_shim_creation() {
        let shim = MesonShim::new();
        assert_eq!(shim.capabilities().identity.id, BackendId::Meson);
    }

    #[test]
    fn test_meson_capabilities() {
        let shim = MesonShim::new();
        let caps = shim.capabilities();

        assert!(caps.artifacts.static_lib);
        assert!(caps.artifacts.shared_lib);
        assert!(caps.platform.cross_compile);
        assert!(caps.platform.toolchain_file_support);
        assert_eq!(caps.phases.configure, PhaseSupport::Required);
    }

    #[test]
    fn test_meson_configure_args() {
        let shim = MesonShim::new();
        let ctx = BuildContext::new(
            PathBuf::from("/src"),
            PathBuf::from("/build"),
            PathBuf::from("/install"),
        )
        .with_release(true);

        let opts = BackendOptions::new();
        let args = shim.build_configure_args(&ctx, &opts);

        assert!(args.contains(&"setup".to_string()));
        assert!(args.contains(&"--buildtype=release".to_string()));
    }

    #[test]
    fn test_meson_defaults() {
        let shim = MesonShim::new();
        let defaults = shim.defaults();

        assert_eq!(
            defaults.injection_order[0],
            InjectionMethod::PrefixPath
        );
    }

    #[test]
    fn test_extract_lib_name() {
        assert_eq!(
            extract_lib_name(std::path::Path::new("libfoo.a")),
            Some("foo".to_string())
        );
        assert_eq!(
            extract_lib_name(std::path::Path::new("bar.lib")),
            Some("bar".to_string())
        );
        assert_eq!(
            extract_lib_name(std::path::Path::new("libfoo.so.1.2.3")),
            Some("foo".to_string())
        );
    }
}
