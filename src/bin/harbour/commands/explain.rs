//! `harbour explain` command

use anyhow::Result;

use crate::cli::ExplainArgs;
use harbour::core::abi::TargetTriple;
use harbour::core::Workspace;
use harbour::ops::resolve::resolve_workspace;
use harbour::resolver::Resolve;
use harbour::sources::SourceCache;
use harbour::util::config::load_config;
use harbour::util::InternedString;
use harbour::util::{GlobalContext, VcpkgIntegration};
use harbour::PackageId;

pub fn execute(args: ExplainArgs) -> Result<()> {
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

    // Find the package
    let pkg_name = InternedString::new(&args.package);
    let pkg_id = resolve.get_package_by_name(pkg_name).ok_or_else(|| {
        anyhow::anyhow!(
            "package `{}` not found in dependency graph\n\
             help: Run `harbour tree` to see all dependencies",
            args.package
        )
    })?;

    // Print package info
    println!("{} {}", pkg_id.name(), pkg_id.version());

    // Print reverse dependency chain (target → root)
    print_reverse_chain(&resolve, pkg_id, ws.root_package_id(), 0);

    println!();

    // Show its direct dependencies
    let deps = resolve.deps(pkg_id);
    if !deps.is_empty() {
        println!("Direct dependencies:");
        for dep_id in &deps {
            println!("  → {} v{}", dep_id.name(), dep_id.version());
        }
    }

    Ok(())
}

/// Print the reverse dependency chain from the target package back to root.
fn print_reverse_chain(resolve: &Resolve, pkg_id: PackageId, root_id: PackageId, depth: usize) {
    let dependents = resolve.dependents(pkg_id);

    if dependents.is_empty() {
        // This is the root
        if pkg_id == root_id {
            let indent = "     ".repeat(depth);
            println!("{}└─ (root)", indent);
        }
        return;
    }

    for (i, dep_id) in dependents.iter().enumerate() {
        let indent = "     ".repeat(depth);
        let is_root = *dep_id == root_id;
        let suffix = if is_root { " (root)" } else { "" };

        if i == 0 {
            println!(
                "{}└─ required by: {} {}{}",
                indent,
                dep_id.name(),
                dep_id.version(),
                suffix
            );
        } else {
            println!(
                "{}└─ required by: {} {}{}",
                indent,
                dep_id.name(),
                dep_id.version(),
                suffix
            );
        }

        // Continue the chain if this isn't the root
        if !is_root {
            print_reverse_chain(resolve, *dep_id, root_id, depth + 1);
        }
    }
}
