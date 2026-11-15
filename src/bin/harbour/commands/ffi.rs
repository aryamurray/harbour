//! `harbour ffi` command
//!
//! FFI bundling operations.

use anyhow::{bail, Result};

use crate::cli::{FfiArgs, FfiCommands};
use harbour::core::workspace::{workspace_manifest_for, Workspace};
use harbour::ops::harbour_build::select_subject;
use harbour::ops::{
    create_ffi_bundle, discover_built_libraries, BundleOptions, DiscoveredSurface, LibraryInfo,
    LibraryKind,
};
use harbour::util::context::GlobalContext;

pub fn execute(args: FfiArgs) -> Result<()> {
    match args.command {
        FfiCommands::Bundle(bundle_args) => bundle(bundle_args),
    }
}

fn bundle(args: crate::cli::FfiBundleArgs) -> Result<()> {
    // Find and load the workspace. The nearest manifest is not enough: from a
    // member it would load that member as a workspace of one, rooted at its
    // own `.harbour/`, while the build it bundles lives at the workspace root.
    let ctx = GlobalContext::new()?;
    let (manifest_path, containing_member) = workspace_manifest_for(&ctx.find_manifest()?)?;
    let ws = Workspace::new(&manifest_path, &ctx)?;
    let pkg = select_subject(&ws, containing_member, args.package.as_deref())?;

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

    // The native builder's output directory for the profile.
    let install_prefix = ws
        .target_dir()
        .join(if args.release { "release" } else { "debug" });

    // What the build left in `include/` and `lib/`; failing that, a wider
    // scan that also looks in `lib64/` and `bin/`.
    let surface = match discover_built_libraries(&install_prefix) {
        Some(s) => s,
        None => scan_install_prefix(&install_prefix)?,
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
        .filter(|l| l.kind == LibraryKind::Shared)
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
        println!(
            "  Runtime dependencies: {} files",
            result.runtime_deps.len()
        );
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

/// Scan install prefix for libraries when discovery isn't available.
fn scan_install_prefix(prefix: &std::path::Path) -> Result<DiscoveredSurface> {
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
                    let is_static =
                        ext == "a" || (ext == "lib" && !path.to_string_lossy().contains(".dll."));
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
