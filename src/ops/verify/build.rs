//! Native and CMake build logic for verification.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use toml::Value;

use super::types::{VerifyContext, VerifyLinkage, VerifyOptions};
use crate::core::target::TargetTriple;
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

    // Anything other than `native` or `cmake` is refused here rather than
    // falling through. The fall-through built the package *natively* while
    // writing a `[targets.X.backend]` table into the generated manifest that
    // nothing reads -- a successful build of the wrong thing, which is the
    // worst available outcome. `cmake` is intercepted above; `meson` and
    // `custom` have no path through `harbour verify` at all.
    if let Some(backend) = verify_ctx
        .shim
        .build
        .as_ref()
        .and_then(|b| b.backend.as_ref())
    {
        if backend != "native" {
            bail!(
                "shim for `{}` declares `backend = \"{backend}\"`, which \
                 `harbour verify` cannot build\n\
                 hint: only `native` and `cmake` are dispatched. Verifying \
                 this package used to build it natively regardless, ignoring \
                 the backend, which produced a green verification of an \
                 artifact the package's own build system never made.\n\
                 tracking: https://github.com/aryamurray/harbour/issues/107",
                verify_ctx.shim.package.name
            );
        }
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
    // `release` is chosen by `ws.with_profile(profile)` above, which is now
    // the single place the profile comes from -- `BuildOptions` no longer
    // carries a second copy that could disagree with it.
    let build_opts = crate::ops::harbour_build::BuildOptions {
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
        target_triple: options
            .target_triple
            .as_ref()
            .map(|s| TargetTriple::parse(s)),
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

    let is_native = backend_name.as_ref().is_none_or(|b| b == "native");

    // This used to write a `[targets.X.backend]` table here, which implied
    // the table is read back on build. It is not -- `harbour build`
    // dispatches on `recipe` -- so the table was inert and the package was
    // built natively anyway. `backend` is now a hard error in the manifest
    // schema (issue #107), so writing one would make `harbour verify` fail
    // to parse its own generated manifest.
    //
    // Non-native shims never get here: `cmake` is intercepted by
    // `build_package`, and everything else is refused by it. The guard is
    // kept rather than assumed, because "unreachable" held for `meson` right
    // up until it did not.
    if !is_native {
        bail!(
            "internal: tried to generate a native manifest for `{}`, whose \
             shim declares `backend = \"{}\"`. `build_package` is supposed \
             to have dispatched or refused it before reaching here.",
            shim.package.name,
            backend_name.as_deref().unwrap_or("?")
        );
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

#[cfg(test)]
mod tests {
    use super::*;

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

        // A cmake shim must never reach the native manifest generator. It
        // used to, and the manifest it produced carried a
        // `[targets.X.backend]` table that nothing reads -- so the package
        // was built natively, from a `sources` list a cmake project does not
        // have, and `harbour verify` called that a pass. `build_package`
        // dispatches cmake to `CMakeBuilder` before getting here; this
        // asserts the generator refuses rather than inventing a manifest.
        let err = generate_manifest_toml(&shim)
            .expect_err("a cmake shim has no native manifest")
            .to_string();
        assert!(
            err.contains("cmake") && err.contains("libuv"),
            "the error must name the backend and the package: {err}"
        );
    }
}
