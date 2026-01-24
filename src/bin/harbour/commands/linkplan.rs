//! `harbour linkplan` command

use anyhow::Result;

use crate::cli::LinkplanArgs;
use harbour::builder::surface_resolver::SurfaceResolver;
use harbour::builder::BuildContext;
use harbour::core::Workspace;
use harbour::ops::resolve::resolve_workspace;
use harbour::sources::SourceCache;
use harbour::util::GlobalContext;

pub fn execute(args: LinkplanArgs) -> Result<()> {
    let ctx = GlobalContext::new()?;

    let manifest_path = ctx.find_manifest()?;

    let ws = Workspace::new(&manifest_path, &ctx)?;
    let mut source_cache = SourceCache::new(ctx.cache_dir());

    let resolve = resolve_workspace(&ws, &mut source_cache)?;

    // Create build context
    let build_ctx = BuildContext::new(&ws, "debug")?;

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

    // Resolve link surface with provenance tracking
    let link_surface = surface_resolver.resolve_link_surface_with_provenance(
        ws.root_package_id(),
        target,
        &build_ctx.deps_dir,
    )?;

    println!("Link order for '{}':", args.target);
    println!();

    let mut index = 1;

    // First, show dependency libraries in link order
    for item in &link_surface.dep_libs {
        println!("  {}. {}", index, item.value.display());
        println!(
            "     Built from: {} (dependency)",
            item.provenance.package_id
        );
        println!();
        index += 1;
    }

    // Show system libraries
    for item in &link_surface.libs {
        for flag in item.value.to_flags() {
            println!("  {}. {}", index, flag);
            println!("     From: {}", item.provenance);
            println!();
            index += 1;
        }
    }

    // Show frameworks
    for item in &link_surface.frameworks {
        println!("  {}. -framework {}", index, item.value);
        println!("     From: {}", item.provenance);
        println!();
        index += 1;
    }

    // Show additional ldflags
    for item in &link_surface.ldflags {
        println!("  {}. {}", index, item.value);
        println!("     From: {}", item.provenance);
        println!();
        index += 1;
    }

    if index == 1 {
        println!("  (no link dependencies)");
    }

    Ok(())
}
