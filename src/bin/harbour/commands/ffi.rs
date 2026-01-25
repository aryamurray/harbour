//! `harbour ffi` command
//!
//! FFI bundling and binding generation operations.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};

use crate::cli::{FfiArgs, FfiCommands, FfiGenerateArgs};
use harbour::builder::bindings::{HeaderParser, ParsedHeader, TypeScriptGenerator};
use harbour::builder::shim::{BackendRegistry, BuildContext, DiscoveredSurface};
use harbour::core::target::{FfiBundler, FfiLanguage};
use harbour::core::workspace::{find_manifest, Workspace};
use harbour::ops::{create_ffi_bundle, BundleOptions};
use harbour::util::context::GlobalContext;

pub fn execute(args: FfiArgs) -> Result<()> {
    match args.command {
        FfiCommands::Bundle(bundle_args) => bundle(bundle_args),
        FfiCommands::Generate(gen_args) => generate(gen_args),
    }
}

fn bundle(args: crate::cli::FfiBundleArgs) -> Result<()> {
    // Find and load workspace
    let cwd = std::env::current_dir()?;
    let manifest_path = find_manifest(&cwd)?;
    let ctx = GlobalContext::default();
    let ws = Workspace::new(&manifest_path, &ctx)?;
    let pkg = ws.root_package();

    // Determine output directory
    let output_dir = args
        .output
        .unwrap_or_else(|| ws.target_dir().join("ffi_bundle"));

    // Create bundle options
    let opts = BundleOptions::new(&output_dir)
        .with_transitive(!args.no_transitive)
        .with_rpath_rewrite(!args.no_rpath)
        .with_dry_run(args.dry_run);

    println!("Creating FFI bundle...");
    println!("  Package: {}", pkg.name());
    println!("  Output:  {}", output_dir.display());
    println!();

    // Try to discover exports from the install prefix
    let install_prefix = ws.target_dir().join(if args.release { "release" } else { "debug" });
    let build_dir = install_prefix.clone();

    // Create a minimal build context
    let ctx = BuildContext::new(pkg.root().to_path_buf(), build_dir, install_prefix.clone())
        .with_release(args.release);

    // Get the appropriate backend shim to discover exports
    let registry = BackendRegistry::new();

    // Default to native for now - could be improved to detect from recipe
    let shim = registry
        .get(harbour::builder::shim::BackendId::Native)
        .context("native backend not found")?;

    // Try to discover exports
    let surface = match shim.discover_exports(&ctx)? {
        Some(s) => s,
        None => {
            // If no discovery available, create a minimal surface by scanning install prefix
            scan_install_prefix(&install_prefix)?
        }
    };

    if surface.libraries.is_empty() {
        bail!(
            "no libraries found in {}.\n\n\
             To create an FFI bundle, first build your project with shared library output:\n\
             \n\
                 harbour build --ffi\n\
             \n\
             Or explicitly request shared linkage:\n\
             \n\
                 harbour build --linkage=shared",
            install_prefix.display()
        );
    }

    // Check that we have at least one shared library
    let shared_libs: Vec<_> = surface
        .libraries
        .iter()
        .filter(|l| l.kind == harbour::builder::shim::LibraryKind::Shared)
        .collect();

    if shared_libs.is_empty() {
        let static_count = surface.libraries.len();
        bail!(
            "found {} static libraries but no shared libraries.\n\n\
             FFI bundles require shared libraries (.so, .dylib, .dll).\n\
             Rebuild with shared library output:\n\
             \n\
                 harbour build --ffi\n\
             \n\
             Or explicitly request shared linkage:\n\
             \n\
                 harbour build --linkage=shared",
            static_count
        );
    }

    println!("Found {} shared libraries:", shared_libs.len());
    for lib in &shared_libs {
        println!("  - {} ({})", lib.name, lib.path.display());
    }
    println!();

    // Create the bundle
    let result = create_ffi_bundle(&surface, &opts)?;

    if args.dry_run {
        println!("[dry-run] Would create bundle with:");
    } else {
        println!("Bundle created:");
    }

    println!("  Primary library: {}", result.primary_lib.display());
    if !result.runtime_deps.is_empty() {
        println!("  Runtime dependencies: {} files", result.runtime_deps.len());
        for dep in &result.runtime_deps {
            println!("    - {}", dep.display());
        }
    }
    println!("  Total size: {} bytes", result.total_size);

    if !args.dry_run {
        println!();
        println!("Bundle ready at: {}", output_dir.display());
    }

    Ok(())
}

fn generate(args: FfiGenerateArgs) -> Result<()> {
    // Parse the target language
    let language: FfiLanguage = args.lang.parse().map_err(|e| {
        anyhow::anyhow!(
            "invalid language '{}': {}",
            args.lang,
            e
        )
    })?;

    // Parse the bundler (default based on language)
    let bundler: FfiBundler = if let Some(ref bundler_str) = args.bundler {
        bundler_str.parse().map_err(|e| {
            anyhow::anyhow!(
                "invalid bundler '{}': {}",
                bundler_str,
                e
            )
        })?
    } else {
        // Default bundler based on language
        match language {
            FfiLanguage::TypeScript => FfiBundler::Koffi,
            FfiLanguage::Python => FfiBundler::Ctypes,
            FfiLanguage::CSharp => FfiBundler::PInvoke,
            FfiLanguage::Rust => FfiBundler::Koffi, // Rust doesn't really need a bundler
        }
    };

    // Find and load workspace
    let cwd = std::env::current_dir()?;
    let manifest_path = find_manifest(&cwd)?;
    let ctx = GlobalContext::default();
    let ws = Workspace::new(&manifest_path, &ctx)?;
    let pkg = ws.root_package();

    // Determine output directory
    let output_dir = args
        .output
        .clone()
        .unwrap_or_else(|| ws.root().join("bindings"));

    println!("Generating FFI bindings...");
    println!("  Package:  {}", pkg.name());
    println!("  Language: {}", language);
    println!("  Bundler:  {}", bundler);
    println!("  Output:   {}", output_dir.display());
    println!();

    // Find header files to parse
    let header_files: Vec<PathBuf> = if !args.header.is_empty() {
        // Use explicitly specified headers
        args.header.clone()
    } else {
        // Look for target's public_headers or ffi.header_files config
        let target_name = args.target.as_deref().unwrap_or(pkg.name().as_str());
        let target = pkg.manifest().target(target_name).with_context(|| {
            format!("target '{}' not found", target_name)
        })?;

        // Check if target has FFI config with header_files
        let header_patterns = if let Some(ref ffi_config) = target.ffi {
            if !ffi_config.header_files.is_empty() {
                ffi_config.header_files.clone()
            } else {
                target.public_headers.clone()
            }
        } else {
            target.public_headers.clone()
        };

        // Expand glob patterns
        expand_header_patterns(pkg.root(), &header_patterns)?
    };

    if header_files.is_empty() {
        bail!(
            "no header files found.\n\n\
             Specify headers with --header or configure public_headers in Harbour.toml:\n\
             \n\
                 [targets.{}]\n\
                 public_headers = [\"include/**/*.h\"]\n\
             \n\
             Or configure FFI-specific headers:\n\
             \n\
                 [targets.{}.ffi]\n\
                 header_files = [\"include/mylib.h\"]",
            pkg.name(),
            pkg.name()
        );
    }

    println!("Parsing {} header file(s):", header_files.len());
    for h in &header_files {
        println!("  - {}", h.display());
    }
    println!();

    // Create the parser with configuration
    let mut parser = HeaderParser::new();
    if let Some(ref prefix) = args.strip_prefix {
        parser = parser.with_strip_prefix(Some(prefix.clone()));
    }

    // Parse all headers and merge results
    let mut combined_header = ParsedHeader::default();
    for header_path in &header_files {
        match parser.parse_file(header_path) {
            Ok(parsed) => {
                println!(
                    "  Parsed {}: {} functions, {} structs, {} enums",
                    header_path.file_name().unwrap_or_default().to_string_lossy(),
                    parsed.functions.len(),
                    parsed.structs.len(),
                    parsed.enums.len()
                );
                combined_header.merge(parsed);
            }
            Err(e) => {
                eprintln!("  Warning: failed to parse {}: {}", header_path.display(), e);
            }
        }
    }

    println!();
    println!(
        "Total: {} functions, {} structs, {} enums, {} typedefs",
        combined_header.functions.len(),
        combined_header.structs.len(),
        combined_header.enums.len(),
        combined_header.typedefs.len()
    );
    println!();

    // Generate bindings based on language
    match language {
        FfiLanguage::TypeScript => {
            generate_typescript(
                &combined_header,
                pkg.name().as_str(),
                &output_dir,
                bundler,
                args.async_wrappers,
                args.lib_path.as_deref(),
            )?;
        }
        FfiLanguage::Python => {
            println!("Python binding generation is not yet implemented.");
            println!("Contributions welcome!");
        }
        FfiLanguage::CSharp => {
            println!("C# binding generation is not yet implemented.");
            println!("Contributions welcome!");
        }
        FfiLanguage::Rust => {
            println!("Rust binding generation is not yet implemented.");
            println!("Contributions welcome!");
        }
    }

    Ok(())
}

/// Generate TypeScript bindings.
fn generate_typescript(
    header: &ParsedHeader,
    lib_name: &str,
    output_dir: &std::path::Path,
    bundler: FfiBundler,
    async_wrappers: bool,
    lib_path: Option<&str>,
) -> Result<()> {
    let mut generator = TypeScriptGenerator::new(lib_name)
        .with_bundler(bundler)
        .with_async_wrappers(async_wrappers);

    if let Some(path) = lib_path {
        generator = generator.with_lib_path(path);
    }

    // Generate to file
    let output_file = output_dir.join(format!("{}.ts", lib_name));
    generator.generate_to_file(header, &output_file)?;

    println!("Generated TypeScript bindings: {}", output_file.display());
    println!();
    println!("To use the bindings:");
    println!("  1. Install koffi: npm install koffi");
    println!("  2. Build your library with: harbour build --ffi --release");
    println!("  3. Copy the shared library to the bindings directory");
    println!("  4. Import and use: import {{ functionName }} from './{}.js'", lib_name);

    Ok(())
}

/// Expand header patterns to file paths.
fn expand_header_patterns(root: &std::path::Path, patterns: &[String]) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();

    for pattern in patterns {
        let full_pattern = root.join(pattern);
        let pattern_str = full_pattern.to_string_lossy();

        for entry in glob::glob(&pattern_str)? {
            match entry {
                Ok(path) => {
                    if path.is_file() {
                        files.push(path);
                    }
                }
                Err(e) => {
                    tracing::warn!("glob error: {}", e);
                }
            }
        }
    }

    Ok(files)
}

/// Scan install prefix for libraries when discovery isn't available.
fn scan_install_prefix(prefix: &std::path::Path) -> Result<DiscoveredSurface> {
    use harbour::builder::shim::{LibraryInfo, LibraryKind};

    let mut surface = DiscoveredSurface::default();

    // Check for include dir
    let include_dir = prefix.join("include");
    if include_dir.exists() {
        surface.include_dirs.push(include_dir);
    }

    // Scan lib directories
    for lib_subdir in ["lib", "lib64", "bin"] {
        let lib_dir = prefix.join(lib_subdir);
        if !lib_dir.exists() {
            continue;
        }

        if let Ok(entries) = std::fs::read_dir(&lib_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }

                if let Some(ext) = path.extension() {
                    let ext = ext.to_string_lossy();
                    let is_static = ext == "a" || (ext == "lib" && !path.to_string_lossy().contains(".dll."));
                    let is_shared = ext == "so" || ext == "dylib" || ext == "dll";

                    if is_static || is_shared {
                        let name = extract_lib_name(&path).unwrap_or_default();
                        if !name.is_empty() {
                            surface.libraries.push(LibraryInfo {
                                name,
                                kind: if is_shared {
                                    LibraryKind::Shared
                                } else {
                                    LibraryKind::Static
                                },
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

    Ok(surface)
}

/// Extract library name from path.
fn extract_lib_name(path: &std::path::Path) -> Option<String> {
    let stem = path.file_stem()?.to_string_lossy();

    // Remove lib prefix if present
    let name = if let Some(stripped) = stem.strip_prefix("lib") {
        stripped.to_string()
    } else {
        stem.to_string()
    };

    // Remove version suffix (e.g., libfoo.so.1.2.3 -> foo)
    let name = name.split('.').next().unwrap_or(&name).to_string();

    Some(name)
}
