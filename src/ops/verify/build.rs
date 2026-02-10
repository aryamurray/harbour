//! Native and CMake build logic for verification.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use toml::Value;

use super::types::{VerifyContext, VerifyLinkage, VerifyOptions};
use crate::builder::shim::intent::TargetTriple;
use crate::core::workspace::{MANIFEST_ALIAS, MANIFEST_NAME};
use crate::core::Workspace;
use crate::sources::registry::Shim;
use crate::sources::SourceCache;
use crate::util::config::VcpkgConfig;
use crate::util::context::GlobalContext;

/// Build the package.
///
/// For CMake projects, uses CMakeBuilder directly since harbour_build::build()
/// doesn't yet dispatch to CMake. For native projects, uses the standard build.
pub(crate) fn build_package(
    verify_ctx: &VerifyContext,
    source_dir: &Path,
    options: &VerifyOptions,
    global_ctx: &GlobalContext,
) -> Result<Vec<PathBuf>> {
    // Check if this is a CMake project
    let is_cmake = verify_ctx
        .shim
        .build
        .as_ref()
        .and_then(|b| b.backend.as_ref())
        .map(|b| b == "cmake")
        .unwrap_or(false);

    if is_cmake {
        return build_with_cmake(verify_ctx, source_dir, options);
    }

    // Native backend - generate Harbour.toml and use standard build
    let manifest_content = generate_manifest_toml(&verify_ctx.shim)?;

    // Write to Harbour.toml (canonical manifest name)
    let manifest_path = source_dir.join(MANIFEST_NAME);
    let alias_path = source_dir.join(MANIFEST_ALIAS);

    // Back up existing manifest if present
    let existing_manifest = if manifest_path.exists() {
        Some(manifest_path.clone())
    } else if alias_path.exists() {
        Some(alias_path.clone())
    } else {
        None
    };

    if let Some(existing_manifest) = existing_manifest {
        let backup_path = existing_manifest.with_extension("toml.bak");
        tracing::warn!(
            "Source has existing manifest - backing up to {}",
            backup_path.display()
        );
        std::fs::rename(&existing_manifest, &backup_path)
            .context("failed to back up existing manifest")?;
    }

    tracing::debug!("Generated manifest:\n{}", manifest_content);
    std::fs::write(&manifest_path, &manifest_content)
        .context("failed to write verification manifest")?;

    // Set up build context with cwd pointing to source directory
    let build_global_ctx = GlobalContext::with_cwd(source_dir.to_path_buf())?;
    let profile = "release";
    let ws = Workspace::new(&manifest_path, &build_global_ctx)?.with_profile(profile);

    let mut source_cache = SourceCache::new(verify_ctx.cache_dir.clone());

    // Determine linkage - warn if Both is used since we only test one at a time
    let linkage = match options.linkage {
        VerifyLinkage::Auto | VerifyLinkage::Static => {
            crate::builder::shim::LinkagePreference::static_()
        }
        VerifyLinkage::Shared => crate::builder::shim::LinkagePreference::shared(),
        VerifyLinkage::Both => {
            tracing::warn!(
                "VerifyLinkage::Both currently only tests static linkage. \
                 Run separately with --linkage=shared to test shared linkage."
            );
            crate::builder::shim::LinkagePreference::static_()
        }
    };

    // Build options for native backend
    let build_opts = crate::ops::harbour_build::BuildOptions {
        release: true,
        packages: vec![verify_ctx.shim.package.name.clone()],
        targets: vec![],
        emit_compile_commands: false,
        emit_plan: false,
        jobs: None,
        verbose: options.verbose || global_ctx.is_verbose(),
        cpp_std: None,
        backend: None, // Native
        linkage,
        ffi: false,
        target_triple: options.target_triple.as_ref().map(|s| TargetTriple::new(s)),
        locked: false,
        vcpkg: VcpkgConfig::default(),
    };

    // Run the build using standard infrastructure
    let build_result = crate::ops::harbour_build::build(&ws, &mut source_cache, &build_opts)
        .map_err(|e| {
            tracing::error!("Native build error: {:?}", e);
            e
        })
        .context("native build failed")?;

    // Collect artifact paths from build result
    let artifacts: Vec<PathBuf> = build_result
        .artifacts
        .iter()
        .map(|a| a.path.clone())
        .collect();

    if artifacts.is_empty() {
        bail!("build produced no artifacts");
    }

    Ok(artifacts)
}

/// Build a CMake project directly.
///
/// This directly invokes CMake since harbour_build::build() doesn't yet
/// dispatch to the CMake backend.
fn build_with_cmake(
    verify_ctx: &VerifyContext,
    source_dir: &Path,
    options: &VerifyOptions,
) -> Result<Vec<PathBuf>> {
    tracing::info!("Building with CMake backend");

    let build_dir = verify_ctx.temp_dir.path().join("cmake-build");
    std::fs::create_dir_all(&build_dir)?;

    // Collect CMake arguments from shim
    let mut cmake_args: Vec<String> = vec![
        "-S".to_string(),
        source_dir.to_string_lossy().to_string(),
        "-B".to_string(),
        build_dir.to_string_lossy().to_string(),
        "-DCMAKE_BUILD_TYPE=Release".to_string(),
        "-DCMAKE_POSITION_INDEPENDENT_CODE=ON".to_string(),
    ];

    // Add shim-specified options
    if let Some(build) = &verify_ctx.shim.build {
        if let Some(cmake) = &build.cmake {
            for opt in &cmake.options {
                cmake_args.push(opt.clone());
            }
        }
    }

    // Configure
    tracing::info!("Configuring CMake project");
    if options.verbose {
        tracing::debug!("CMake args: {:?}", cmake_args);
    }

    let output = Command::new("cmake")
        .args(&cmake_args)
        .current_dir(source_dir)
        .output()
        .context("failed to run cmake configure")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        bail!(
            "CMake configure failed:\nstdout:\n{}\nstderr:\n{}",
            stdout,
            stderr
        );
    }

    // Build
    tracing::info!("Building CMake project");
    let output = Command::new("cmake")
        .arg("--build")
        .arg(&build_dir)
        .arg("--config")
        .arg("Release")
        .arg("--parallel")
        .current_dir(source_dir)
        .output()
        .context("failed to run cmake build")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        bail!(
            "CMake build failed:\nstdout:\n{}\nstderr:\n{}",
            stdout,
            stderr
        );
    }

    // Find built artifacts
    let mut artifacts = Vec::new();

    // Determine library extensions based on OS
    #[cfg(target_os = "windows")]
    let lib_extensions: &[&str] = &["lib", "dll"];
    #[cfg(target_os = "macos")]
    let lib_extensions: &[&str] = &["a", "dylib"];
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let lib_extensions: &[&str] = &["a", "so"];

    // Search for libraries in build directory
    for entry in walkdir::WalkDir::new(&build_dir)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            if lib_extensions.contains(&ext) {
                // Skip CMake internal files
                let filename = path.file_name().unwrap_or_default().to_string_lossy();
                if !filename.starts_with("cmake") && !filename.contains("CMake") {
                    artifacts.push(path.to_path_buf());
                }
            }
        }
    }

    if artifacts.is_empty() {
        bail!(
            "CMake build produced no library artifacts in {}",
            build_dir.display()
        );
    }

    if options.verbose {
        for artifact in &artifacts {
            tracing::info!("Built artifact: {}", artifact.display());
        }
    }

    Ok(artifacts)
}

/// Generate a Harbour.toml manifest from a shim.
///
/// Uses the `toml` crate for safe serialization, ensuring proper escaping
/// and formatting of all values.
pub(crate) fn generate_manifest_toml(shim: &Shim) -> Result<String> {
    let mut doc = toml::Table::new();
    let pkg_name = &shim.package.name;

    // [package]
    let mut package = toml::Table::new();
    package.insert("name".into(), Value::String(pkg_name.clone()));
    package.insert(
        "version".into(),
        Value::String(shim.package.version.clone()),
    );
    doc.insert("package".into(), Value::Table(package));

    // [targets.NAME]
    let mut targets = toml::Table::new();
    let mut target = toml::Table::new();
    target.insert("kind".into(), Value::String("staticlib".into()));

    // Check for build configuration (cmake, meson, etc.)
    let backend_name = shim
        .build
        .as_ref()
        .and_then(|b| b.backend.as_ref())
        .cloned();

    let is_native = backend_name.as_ref().map_or(true, |b| b == "native");

    // Backend configuration
    if let Some(ref backend) = backend_name {
        if backend != "native" {
            let mut backend_table = toml::Table::new();
            backend_table.insert("backend".into(), Value::String(backend.clone()));

            // Parse and convert CMake options
            if backend == "cmake" {
                if let Some(build) = &shim.build {
                    if let Some(cmake) = &build.cmake {
                        let options = parse_cmake_options(&cmake.options)?;
                        if !options.is_empty() {
                            backend_table.insert("options".into(), Value::Table(options));
                        }
                    }
                }
            }

            target.insert("backend".into(), Value::Table(backend_table));
        }
    }

    // Sources (native backend only)
    if is_native {
        let sources = shim
            .effective_surface_override()
            .and_then(|s| {
                if s.sources.is_empty() {
                    None
                } else {
                    Some(s.sources.clone())
                }
            })
            .unwrap_or_else(|| vec!["*.c".into(), "src/*.c".into()]);

        target.insert(
            "sources".into(),
            Value::Array(sources.iter().map(|s| Value::String(s.clone())).collect()),
        );
    }

    // Surface configuration
    if let Some(surface) = shim.effective_surface_override() {
        if let Some(compile) = &surface.compile {
            if let Some(public) = &compile.public {
                if !public.include_dirs.is_empty() || !public.defines.is_empty() {
                    let mut surface_table = toml::Table::new();
                    let mut compile_table = toml::Table::new();
                    let mut public_table = toml::Table::new();

                    if !public.include_dirs.is_empty() {
                        public_table.insert(
                            "include_dirs".into(),
                            Value::Array(
                                public
                                    .include_dirs
                                    .iter()
                                    .map(|d| Value::String(d.clone()))
                                    .collect(),
                            ),
                        );
                    }

                    if !public.defines.is_empty() {
                        public_table.insert(
                            "defines".into(),
                            Value::Array(
                                public
                                    .defines
                                    .iter()
                                    .map(|d| Value::String(d.clone()))
                                    .collect(),
                            ),
                        );
                    }

                    compile_table.insert("public".into(), Value::Table(public_table));
                    surface_table.insert("compile".into(), Value::Table(compile_table));
                    target.insert("surface".into(), Value::Table(surface_table));
                }
            }
        }
    }

    targets.insert(pkg_name.clone(), Value::Table(target));
    doc.insert("targets".into(), Value::Table(targets));

    toml::to_string_pretty(&doc).context("failed to serialize manifest")
}

/// Parse CMake options from shim format (-DKEY=VALUE) into a TOML table.
///
/// Handles:
/// - `-DKEY=VALUE` -> key = value
/// - `-DKEY:TYPE=VALUE` -> key = value (type annotation stripped)
/// - `-G Generator` -> CMAKE_GENERATOR = "Generator"
/// - Boolean values: ON/OFF/TRUE/FALSE/YES/NO/1/0
///
/// Warns about malformed options that cannot be parsed.
pub(crate) fn parse_cmake_options(opts: &[String]) -> Result<toml::Table> {
    let mut table = toml::Table::new();

    for opt in opts {
        if let Some(stripped) = opt.strip_prefix("-D") {
            // Handle -DKEY:TYPE=VALUE or -DKEY=VALUE
            if let Some((key_part, value)) = stripped.split_once('=') {
                // Strip type annotation if present: KEY:BOOL -> KEY
                let key = key_part.split(':').next().unwrap_or(key_part);
                let parsed_value = parse_cmake_value(value);
                table.insert(key.to_string(), parsed_value);
            } else {
                // Malformed: -DKEY without value
                tracing::warn!(
                    "Skipping malformed CMake option '{}': expected -DKEY=VALUE format",
                    opt
                );
            }
        } else if let Some(generator) = opt.strip_prefix("-G") {
            // -G Generator or -GGenerator
            let gen = generator.trim();
            if !gen.is_empty() {
                table.insert("CMAKE_GENERATOR".into(), Value::String(gen.to_string()));
            } else {
                tracing::warn!(
                    "Skipping malformed CMake option '{}': -G requires a generator name",
                    opt
                );
            }
        } else if opt.starts_with('-') {
            // Unknown flag - warn but don't fail
            tracing::debug!(
                "Ignoring unrecognized CMake option '{}': only -D and -G flags are converted",
                opt
            );
        }
        // Non-flag options are silently ignored (shouldn't happen in well-formed shims)
    }

    Ok(table)
}

/// Parse a CMake value string into an appropriate TOML value.
///
/// - ON/TRUE/YES/1 -> true
/// - OFF/FALSE/NO/0 -> false
/// - Integer strings -> integer
/// - Everything else -> string
pub(crate) fn parse_cmake_value(v: &str) -> Value {
    match v.to_uppercase().as_str() {
        "ON" | "TRUE" | "YES" | "1" => Value::Boolean(true),
        "OFF" | "FALSE" | "NO" | "0" => Value::Boolean(false),
        _ => {
            // Try integer
            if let Ok(i) = v.parse::<i64>() {
                Value::Integer(i)
            } else {
                Value::String(v.to_string())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cmake_option_parsing() {
        let opts = vec![
            "-DFOO=ON".into(),
            "-DBAR:BOOL=OFF".into(),
            "-DBAZ=some_string".into(),
            "-DCOUNT=42".into(),
            "-G Ninja".into(),
        ];
        let table = parse_cmake_options(&opts).unwrap();

        assert_eq!(table.get("FOO"), Some(&Value::Boolean(true)));
        assert_eq!(table.get("BAR"), Some(&Value::Boolean(false)));
        assert_eq!(table.get("BAZ"), Some(&Value::String("some_string".into())));
        assert_eq!(table.get("COUNT"), Some(&Value::Integer(42)));
        assert_eq!(
            table.get("CMAKE_GENERATOR"),
            Some(&Value::String("Ninja".into()))
        );
    }

    #[test]
    fn test_cmake_option_parsing_malformed() {
        // Malformed options should be skipped (with warnings logged)
        let opts = vec![
            "-DFOO=ON".into(),    // Valid
            "-DMALFORMED".into(), // Invalid: no value
            "-G".into(),          // Invalid: no generator name
            "-DBAR=OFF".into(),   // Valid
            "-WUNKNOWN".into(),   // Unknown flag (ignored)
        ];
        let table = parse_cmake_options(&opts).unwrap();

        // Only valid options should be in the table
        assert_eq!(table.len(), 2);
        assert_eq!(table.get("FOO"), Some(&Value::Boolean(true)));
        assert_eq!(table.get("BAR"), Some(&Value::Boolean(false)));

        // Malformed options should NOT be in the table
        assert!(table.get("MALFORMED").is_none());
        assert!(table.get("CMAKE_GENERATOR").is_none());
    }

    #[test]
    fn test_cmake_value_parsing() {
        // Boolean true values
        assert_eq!(parse_cmake_value("ON"), Value::Boolean(true));
        assert_eq!(parse_cmake_value("TRUE"), Value::Boolean(true));
        assert_eq!(parse_cmake_value("YES"), Value::Boolean(true));
        assert_eq!(parse_cmake_value("1"), Value::Boolean(true));

        // Boolean false values
        assert_eq!(parse_cmake_value("OFF"), Value::Boolean(false));
        assert_eq!(parse_cmake_value("FALSE"), Value::Boolean(false));
        assert_eq!(parse_cmake_value("NO"), Value::Boolean(false));
        assert_eq!(parse_cmake_value("0"), Value::Boolean(false));

        // Integer values
        assert_eq!(parse_cmake_value("42"), Value::Integer(42));
        assert_eq!(parse_cmake_value("-10"), Value::Integer(-10));

        // String values
        assert_eq!(
            parse_cmake_value("some_value"),
            Value::String("some_value".into())
        );
    }

    #[test]
    fn test_generate_manifest_native() {
        use crate::sources::registry::shim::{
            CompileSurfacePublic, Shim, ShimPackage, ShimSource, ShimSurfaceOverride,
            SurfaceOverrideCompile,
        };

        let shim = Shim {
            package: ShimPackage {
                name: "mylib".to_string(),
                version: "1.0.0".to_string(),
            },
            source: ShimSource {
                git: Some(crate::sources::registry::shim::GitSource {
                    url: "https://example.com/repo".to_string(),
                    rev: "a".repeat(40),
                    checksum: None,
                }),
                tarball: None,
            },
            patches: vec![],
            metadata: None,
            features: None,
            surface_override: Some(ShimSurfaceOverride {
                compile: Some(SurfaceOverrideCompile {
                    public: Some(CompileSurfacePublic {
                        include_dirs: vec!["include".to_string()],
                        defines: vec!["MYLIB_API".to_string()],
                    }),
                    private: None,
                }),
                link: None,
                sources: vec!["src/*.c".to_string()],
            }),
            surface: None,
            build: None,
        };

        let manifest = generate_manifest_toml(&shim).unwrap();

        // Verify it parses correctly
        let parsed: toml::Table = toml::from_str(&manifest).unwrap();
        assert!(parsed.contains_key("package"));
        assert!(parsed.contains_key("targets"));

        let targets = parsed.get("targets").unwrap().as_table().unwrap();
        let target = targets.get("mylib").unwrap().as_table().unwrap();
        assert_eq!(target.get("kind").unwrap().as_str().unwrap(), "staticlib");

        // Check sources are included for native backend
        let sources = target.get("sources").unwrap().as_array().unwrap();
        assert!(!sources.is_empty());
    }

    #[test]
    fn test_generate_manifest_cmake() {
        use crate::sources::registry::shim::{
            CompileSurfacePublic, Shim, ShimBuildConfig, ShimCMakeConfig, ShimPackage, ShimSource,
            ShimSurfaceOverride, SurfaceOverrideCompile,
        };

        let shim = Shim {
            package: ShimPackage {
                name: "libuv".to_string(),
                version: "1.51.0".to_string(),
            },
            source: ShimSource {
                git: Some(crate::sources::registry::shim::GitSource {
                    url: "https://github.com/libuv/libuv".to_string(),
                    rev: "a".repeat(40),
                    checksum: None,
                }),
                tarball: None,
            },
            patches: vec![],
            metadata: None,
            features: None,
            surface_override: Some(ShimSurfaceOverride {
                compile: Some(SurfaceOverrideCompile {
                    public: Some(CompileSurfacePublic {
                        include_dirs: vec!["include".to_string()],
                        defines: vec![],
                    }),
                    private: None,
                }),
                link: None,
                sources: vec![], // CMake doesn't need sources
            }),
            surface: None,
            build: Some(ShimBuildConfig {
                backend: Some("cmake".to_string()),
                cmake: Some(ShimCMakeConfig {
                    options: vec![
                        "-DLIBUV_BUILD_TESTS=OFF".to_string(),
                        "-DLIBUV_BUILD_BENCH=OFF".to_string(),
                        "-G Ninja".to_string(),
                    ],
                }),
            }),
        };

        let manifest = generate_manifest_toml(&shim).unwrap();

        // Verify it parses correctly
        let parsed: toml::Table = toml::from_str(&manifest).unwrap();
        assert!(parsed.contains_key("package"));
        assert!(parsed.contains_key("targets"));

        let targets = parsed.get("targets").unwrap().as_table().unwrap();
        let target = targets.get("libuv").unwrap().as_table().unwrap();

        // Check backend is present
        let backend = target.get("backend").unwrap().as_table().unwrap();
        assert_eq!(backend.get("backend").unwrap().as_str().unwrap(), "cmake");

        // Check options are converted
        let options = backend.get("options").unwrap().as_table().unwrap();
        assert_eq!(
            options.get("LIBUV_BUILD_TESTS"),
            Some(&Value::Boolean(false))
        );
        assert_eq!(
            options.get("LIBUV_BUILD_BENCH"),
            Some(&Value::Boolean(false))
        );
        assert_eq!(
            options.get("CMAKE_GENERATOR"),
            Some(&Value::String("Ninja".into()))
        );

        // CMake packages should NOT have sources in the target
        assert!(target.get("sources").is_none());
    }
}
