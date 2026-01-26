//! CMake backend shim - wraps CMake-based builds.
//!
//! This shim provides the BackendShim interface for CMake projects.

use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::builder::shim::capabilities::{
    BackendCapabilitiesBuilder, BackendId, DependencyFormat, ExportDiscovery, InjectionMethod,
    PhaseSupport, TransitiveHandling, *,
};
use crate::builder::shim::defaults::BackendDefaults;
use crate::builder::shim::intent::BackendOptions;
use crate::builder::shim::trait_def::*;
use crate::builder::util::{detect_tool_version, extract_lib_name};

/// CMake backend shim.
///
/// Wraps CMake-based builds with the BackendShim interface.
pub struct CMakeShim {
    capabilities: BackendCapabilities,
    defaults: BackendDefaults,
    /// Cached availability (lazily computed)
    cached_availability: std::sync::OnceLock<BackendAvailability>,
}

impl CMakeShim {
    /// Create a new CMake backend shim.
    pub fn new() -> Self {
        CMakeShim {
            capabilities: Self::build_capabilities(),
            defaults: BackendDefaults::cmake(),
            cached_availability: std::sync::OnceLock::new(),
        }
    }

    fn build_capabilities() -> BackendCapabilities {
        BackendCapabilitiesBuilder::new(BackendId::CMake, Some(">=3.16"))
            .phases(
                PhaseSupport::Required, // CMake requires configure
                PhaseSupport::Required,
                PhaseSupport::Optional, // via ctest
                PhaseSupport::Optional,
                PhaseSupport::Required,
            )
            .cross_compile(true)
            .static_shared_single_invocation(true)
            .injection_methods(&[
                InjectionMethod::PrefixPath,
                InjectionMethod::CMakeDefines,
                InjectionMethod::ToolchainFile,
                InjectionMethod::EnvVars,
            ])
            .consumable_formats(&[DependencyFormat::CMakeConfig, DependencyFormat::PkgConfig])
            .transitive_handling(TransitiveHandling::ViaPrefix)
            .export_discovery(ExportDiscovery::Optional, true)
            .install_contract(true, true)
            .build()
    }

    /// Detect CMake version.
    fn detect_cmake_version() -> Result<semver::Version> {
        detect_tool_version("cmake", |stdout| {
            // Parse "cmake version 3.20.5"
            for line in stdout.lines() {
                if line.starts_with("cmake version ") {
                    let version_str = line.trim_start_matches("cmake version ").trim();
                    // Handle versions like "3.20.5" or "3.20.5-dirty"
                    let clean_version = version_str.split('-').next().unwrap_or(version_str);
                    return clean_version.parse().ok();
                }
            }
            None
        })
    }

    /// Get the generator to use.
    fn get_generator(&self, opts: &BackendOptions) -> Option<String> {
        // Check if user specified generator in options
        if let Some(gen) = opts.get_string("CMAKE_GENERATOR") {
            return Some(gen.to_string());
        }

        // Use default
        self.defaults.preferred_generator.clone()
    }

    /// Build CMake configure arguments.
    #[allow(clippy::vec_init_then_push)]
    fn build_configure_args(&self, ctx: &BuildContext, opts: &BackendOptions) -> Vec<String> {
        let mut args = Vec::new();

        // Source directory
        args.push("-S".to_string());
        args.push(ctx.package_root.display().to_string());

        // Build directory
        args.push("-B".to_string());
        args.push(ctx.build_dir.display().to_string());

        // Generator
        if let Some(generator) = self.get_generator(opts) {
            args.push("-G".to_string());
            args.push(generator);
        }

        // Build type (for single-config generators)
        let build_type = if ctx.release { "Release" } else { "Debug" };
        args.push(format!("-DCMAKE_BUILD_TYPE={}", build_type));

        // Install prefix
        args.push(format!(
            "-DCMAKE_INSTALL_PREFIX={}",
            ctx.install_prefix.display()
        ));

        // Add user-defined CMake options
        for (key, value) in opts.iter() {
            // Skip internal options
            if key == "CMAKE_GENERATOR" {
                continue;
            }

            // Convert TOML values to CMake defines
            let cmake_value = match value {
                toml::Value::Boolean(b) => if *b { "ON" } else { "OFF" }.to_string(),
                toml::Value::Integer(i) => i.to_string(),
                toml::Value::Float(f) => f.to_string(),
                toml::Value::String(s) => s.clone(),
                toml::Value::Array(arr) => {
                    // Convert array to semicolon-separated list (CMake style)
                    arr.iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join(";")
                }
                _ => continue,
            };

            args.push(format!("-D{}={}", key, cmake_value));
        }

        args
    }
}

impl Default for CMakeShim {
    fn default() -> Self {
        Self::new()
    }
}

impl BackendShim for CMakeShim {
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

        // Detect CMake
        let availability = match Self::detect_cmake_version() {
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
                tool: "cmake".to_string(),
                install_hint: get_cmake_install_hint(),
            },
        };

        // Cache the result
        let _ = self.cached_availability.set(availability.clone());

        Ok(availability)
    }

    fn configure(&self, ctx: &BuildContext, opts: &BackendOptions) -> Result<ConfigureResult> {
        let args = self.build_configure_args(ctx, opts);

        tracing::debug!("CMake configure: cmake {}", args.join(" "));

        let status = Command::new("cmake")
            .args(&args)
            .current_dir(&ctx.package_root)
            .status()
            .context("failed to run cmake configure")?;

        if !status.success() {
            bail!("cmake configure failed with exit code: {:?}", status.code());
        }

        Ok(ConfigureResult {
            skipped: false,
            generator: self.get_generator(opts),
            warnings: Vec::new(),
        })
    }

    fn build(&self, ctx: &BuildContext, opts: &BackendOptions) -> Result<BuildResult> {
        let mut args = vec!["--build".to_string(), ctx.build_dir.display().to_string()];

        // Multi-config generators need --config
        if let Some(ref gen) = self.get_generator(opts) {
            if let Some(config) = self.defaults.generator_configs.get(gen) {
                if config.multi_config {
                    let build_type = if ctx.release { "Release" } else { "Debug" };
                    args.push("--config".to_string());
                    args.push(build_type.to_string());
                }
            }
        }

        // Parallel jobs
        if let Some(jobs) = ctx.jobs {
            args.push("--parallel".to_string());
            args.push(jobs.to_string());
        }

        // Verbose
        if ctx.verbose {
            args.push("--verbose".to_string());
        }

        tracing::debug!("CMake build: cmake {}", args.join(" "));

        let status = Command::new("cmake")
            .args(&args)
            .status()
            .context("failed to run cmake build")?;

        if !status.success() {
            bail!("cmake build failed with exit code: {:?}", status.code());
        }

        Ok(BuildResult::default())
    }

    fn test(&self, ctx: &BuildContext, opts: &BackendOptions) -> Result<TestResult> {
        let mut args = vec![
            "--test-dir".to_string(),
            ctx.build_dir.display().to_string(),
        ];

        // Multi-config generators need -C
        if let Some(ref gen) = self.get_generator(opts) {
            if let Some(config) = self.defaults.generator_configs.get(gen) {
                if config.multi_config {
                    let build_type = if ctx.release { "Release" } else { "Debug" };
                    args.push("-C".to_string());
                    args.push(build_type.to_string());
                }
            }
        }

        // Output on failure
        args.push("--output-on-failure".to_string());

        // Parallel
        if let Some(jobs) = ctx.jobs {
            args.push("-j".to_string());
            args.push(jobs.to_string());
        }

        tracing::debug!("CTest: ctest {}", args.join(" "));

        let output = Command::new("ctest")
            .args(&args)
            .output()
            .context("failed to run ctest")?;

        // Parse ctest output for results
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut result = TestResult::default();

        // Simple parsing of "X tests passed, Y tests failed"
        for line in stdout.lines() {
            if line.contains("tests passed") {
                // Parse test counts
                let parts: Vec<&str> = line.split_whitespace().collect();
                for (i, part) in parts.iter().enumerate() {
                    if let Ok(n) = part.parse::<usize>() {
                        if i + 1 < parts.len() {
                            match parts[i + 1] {
                                "tests" if i + 2 < parts.len() && parts[i + 2] == "passed" => {
                                    result.passed = n;
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
        }

        result.total = result.passed + result.failed + result.skipped;

        if !output.status.success() && result.failed == 0 {
            // ctest failed but we couldn't parse failure count
            result.failed = 1;
        }

        Ok(result)
    }

    fn install(&self, ctx: &BuildContext, opts: &BackendOptions) -> Result<InstallResult> {
        let mut args = vec!["--install".to_string(), ctx.build_dir.display().to_string()];

        // Multi-config generators need --config
        if let Some(ref gen) = self.get_generator(opts) {
            if let Some(config) = self.defaults.generator_configs.get(gen) {
                if config.multi_config {
                    let build_type = if ctx.release { "Release" } else { "Debug" };
                    args.push("--config".to_string());
                    args.push(build_type.to_string());
                }
            }
        }

        // Prefix (in case it's different from configure time)
        args.push("--prefix".to_string());
        args.push(ctx.install_prefix.display().to_string());

        tracing::debug!("CMake install: cmake {}", args.join(" "));

        let status = Command::new("cmake")
            .args(&args)
            .status()
            .context("failed to run cmake install")?;

        if !status.success() {
            bail!("cmake install failed with exit code: {:?}", status.code());
        }

        Ok(InstallResult::default())
    }

    fn clean(&self, ctx: &BuildContext, opts: &CleanOptions) -> Result<()> {
        if opts.dry_run {
            tracing::info!("Would clean: {}", ctx.build_dir.display());
            return Ok(());
        }

        if opts.build_dir && ctx.build_dir.exists() {
            // Try cmake --build --target clean first
            let status = Command::new("cmake")
                .args([
                    "--build",
                    &ctx.build_dir.display().to_string(),
                    "--target",
                    "clean",
                ])
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

        // Look for CMake config files
        let cmake_dir = ctx.install_prefix.join("lib").join("cmake");
        let has_cmake_config = cmake_dir.exists();

        // Add include directories
        let include_dir = ctx.install_prefix.join("include");
        if include_dir.exists() {
            surface.include_dirs.push(include_dir);
        }

        // Scan for libraries
        for lib_subdir in ["lib", "lib64"] {
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

        if surface.include_dirs.is_empty() && surface.libraries.is_empty() && !has_cmake_config {
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

        // Check CMake availability
        let cmake_check = match Self::detect_cmake_version() {
            Ok(version) => {
                let req = self.capabilities.identity.backend_version_req.as_ref();
                let passed = req.map_or(true, |r| r.matches(&version));

                if !passed {
                    report.ok = false;
                    report.suggestions.push(format!(
                        "Upgrade CMake to version {} or higher",
                        req.unwrap()
                    ));
                }

                DoctorCheck {
                    name: "cmake".to_string(),
                    passed,
                    message: format!("CMake version {}", version),
                }
            }
            Err(e) => {
                report.ok = false;
                report.suggestions.push(get_cmake_install_hint());

                DoctorCheck {
                    name: "cmake".to_string(),
                    passed: false,
                    message: format!("CMake not found: {}", e),
                }
            }
        };
        report.checks.push(cmake_check);

        // Check for Ninja (preferred generator)
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
                report.suggestions.push(
                    "Consider installing Ninja for faster builds: https://ninja-build.org/"
                        .to_string(),
                );
                DoctorCheck {
                    name: "ninja".to_string(),
                    passed: true, // Not required, just recommended
                    message: "Ninja not found (optional, but recommended)".to_string(),
                }
            }
        };
        report.checks.push(ninja_check);

        // Check build directory
        if ctx.build_dir.exists() {
            let cmake_cache = ctx.build_dir.join("CMakeCache.txt");
            report.checks.push(DoctorCheck {
                name: "cmake_cache".to_string(),
                passed: cmake_cache.exists(),
                message: if cmake_cache.exists() {
                    "CMake cache found".to_string()
                } else {
                    "Build directory exists but not configured".to_string()
                },
            });
        }

        Ok(report)
    }
}

/// Get platform-specific CMake install hint.
fn get_cmake_install_hint() -> String {
    #[cfg(target_os = "linux")]
    {
        "Install CMake: apt install cmake, dnf install cmake, or https://cmake.org/download/"
            .to_string()
    }
    #[cfg(target_os = "macos")]
    {
        "Install CMake: brew install cmake or https://cmake.org/download/".to_string()
    }
    #[cfg(target_os = "windows")]
    {
        "Install CMake: winget install cmake or https://cmake.org/download/".to_string()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        "Install CMake from https://cmake.org/download/".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_cmake_shim_creation() {
        let shim = CMakeShim::new();
        assert_eq!(shim.capabilities().identity.id, BackendId::CMake);
    }

    #[test]
    fn test_cmake_capabilities() {
        let shim = CMakeShim::new();
        let caps = shim.capabilities();

        assert!(caps.artifacts.static_lib);
        assert!(caps.artifacts.shared_lib);
        assert!(caps.platform.cross_compile);
        assert!(caps.platform.toolchain_file_support);
        assert_eq!(caps.phases.configure, PhaseSupport::Required);
    }

    #[test]
    fn test_cmake_configure_args() {
        let shim = CMakeShim::new();
        let ctx = BuildContext::new(
            PathBuf::from("/src"),
            PathBuf::from("/build"),
            PathBuf::from("/install"),
        )
        .with_release(true);

        let opts = BackendOptions::new();
        let args = shim.build_configure_args(&ctx, &opts);

        assert!(args.contains(&"-S".to_string()));
        assert!(args.contains(&"-DCMAKE_BUILD_TYPE=Release".to_string()));
    }

    #[test]
    fn test_cmake_defaults() {
        let shim = CMakeShim::new();
        let defaults = shim.defaults();

        assert_eq!(defaults.preferred_generator, Some("Ninja".to_string()));
        assert!(defaults.generator_configs.contains_key("Ninja"));
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
    }
}
