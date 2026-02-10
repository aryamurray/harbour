//! `harbour flags` command

use anyhow::Result;

use crate::cli::FlagsArgs;
use harbour::builder::surface_resolver::SurfaceResolver;
use harbour::builder::BuildContext;
use harbour::core::abi::TargetTriple;
use harbour::core::Workspace;
use harbour::ops::resolve::resolve_workspace;
use harbour::sources::SourceCache;
use harbour::util::config::load_config;
use harbour::util::GlobalContext;
use harbour::util::VcpkgIntegration;

pub fn execute(args: FlagsArgs) -> Result<()> {
    let ctx = GlobalContext::new()?;

    let manifest_path = ctx.find_manifest()?;

    let ws = Workspace::new(&manifest_path, &ctx)?;

    let config = load_config(
        &ctx.config_path(),
        &ctx.project_harbour_dir().join("config.toml"),
    );
    let vcpkg = VcpkgIntegration::from_config(&config.vcpkg, &TargetTriple::host(), false);
    let mut source_cache = SourceCache::new_with_vcpkg(ctx.cache_dir(), vcpkg);

    let resolve = resolve_workspace(&ws, &mut source_cache)?;

    // Create build context
    let build_ctx = BuildContext::new_with_vcpkg(&ws, "debug", &config.vcpkg)?;

    // Create surface resolver
    let mut surface_resolver = SurfaceResolver::new(&resolve, &build_ctx.platform);
    surface_resolver.load_packages(&mut source_cache)?;

    // Find the target
    let root_pkg = ws.root_package();
    let target = root_pkg.target(&args.target).ok_or_else(|| {
        anyhow::anyhow!(
            "target `{}` not found\n\
             help: Run `harbour tree` to see available targets",
            args.target
        )
    })?;

    // Resolve surfaces with provenance tracking
    let compile_surface =
        surface_resolver.resolve_compile_surface_with_provenance(ws.root_package_id(), target)?;
    let link_surface = surface_resolver.resolve_link_surface_with_provenance(
        ws.root_package_id(),
        target,
        &build_ctx.deps_dir,
    )?;

    // Print compile flags with provenance
    if !args.link {
        println!("# Compile flags for `{}`:", args.target);

        for item in &compile_surface.include_dirs {
            println!(
                "  -I{}    # from: {}",
                item.value.display(),
                item.provenance
            );
        }

        for item in &compile_surface.defines {
            println!("  {}    # from: {}", item.value.to_flag(), item.provenance);
        }

        for item in &compile_surface.cflags {
            println!("  {}    # from: {}", item.value, item.provenance);
        }

        if let Some(vcpkg) = build_ctx.vcpkg() {
            for dir in &vcpkg.include_dirs {
                println!("  -I{}    # from: vcpkg", dir.display());
            }
        }
    }

    if !args.compile && !args.link {
        println!();
    }

    // Print link flags with provenance
    if !args.compile {
        println!("# Link flags for `{}`:", args.target);

        for item in &link_surface.lib_dirs {
            println!(
                "  -L{}    # from: {}",
                item.value.display(),
                item.provenance
            );
        }

        for item in &link_surface.dep_libs {
            println!("  {}    # from: {}", item.value.display(), item.provenance);
        }

        for item in &link_surface.libs {
            for flag in item.value.to_flags() {
                println!("  {}    # from: {}", flag, item.provenance);
            }
        }

        for item in &link_surface.frameworks {
            println!("  -framework {}    # from: {}", item.value, item.provenance);
        }

        for item in &link_surface.ldflags {
            println!("  {}    # from: {}", item.value, item.provenance);
        }

        if let Some(vcpkg) = build_ctx.vcpkg() {
            for dir in &vcpkg.lib_dirs {
                println!("  -L{}    # from: vcpkg", dir.display());
            }
        }
    }

    Ok(())
}
