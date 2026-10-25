//! `harbour linkplan` command

use anyhow::Result;

use crate::cli::LinkplanArgs;
use harbour::builder::plan::BuildPlan;
use harbour::builder::surface_resolver::SurfaceResolver;
use harbour::builder::BuildContext;
use harbour::core::workspace::workspace_manifest_for;
use harbour::core::Workspace;
use harbour::ops::resolve::resolve_workspace;
use harbour::sources::SourceCache;
use harbour::util::config::load_config;
use harbour::util::GlobalContext;

pub fn execute(args: LinkplanArgs) -> Result<()> {
    let ctx = GlobalContext::new()?;

    let (manifest_path, member) = workspace_manifest_for(&ctx.find_manifest()?)?;

    let ws = Workspace::new(&manifest_path, &ctx)?;
    // The flags speak for the default members (what a bare `harbour build`
    // builds) and for the package being inspected, so this prints what
    // `harbour build -p <package>` would use.
    let selection = harbour::ops::harbour_build::feature_selection_for(
        &ws,
        &args.feature_args.features,
        args.feature_args.no_default_features,
        args.feature_args.all_features,
        &[],
        Some(ws.subject_package(member).name().as_str()),
    )?;
    let ws = ws.with_feature_selection(selection);

    let config = load_config(
        &ctx.config_path(),
        &ctx.project_harbour_dir().join("config.toml"),
    );
    let vcpkg = harbour::builder::resolution_vcpkg(&config.vcpkg, None, false);
    let mut source_cache = SourceCache::new_with_vcpkg(ctx.cache_dir(), vcpkg)
        .with_default_registry(ctx.default_registry_url().as_str());

    let resolve = resolve_workspace(&ws, &mut source_cache)?;

    // Create build context
    let build_ctx = BuildContext::new_with_vcpkg(&ws, "debug", &config.vcpkg, None)?;

    // Create surface resolver
    let mut surface_resolver = SurfaceResolver::new(&resolve, &build_ctx.platform)
        .with_feature_selection(build_ctx.feature_selection.clone());
    surface_resolver.load_packages(&mut source_cache)?;

    // Find the target
    let root_pkg = ws.subject_package(member);
    let target = root_pkg.target(&args.target).ok_or_else(|| {
        anyhow::anyhow!(
            "target `{}` not found\n\
             help: Run `harbour tree` to see available targets",
            args.target
        )
    })?;

    // Resolve link surface with provenance tracking
    let subject_id = ws.subject_package_id(member);
    let build_roots = harbour::ops::harbour_build::build_roots(
        &harbour::ops::harbour_build::select_packages(&ws, &[])?,
        &resolve,
    )?;
    let link_surface = surface_resolver.resolve_link_surface_with_provenance(
        subject_id,
        target,
        &build_ctx.deps_dir,
        // Where this package's own archives go, for a target that depends on
        // a sibling. The root set is the one a bare `harbour build` plans from,
        // and the directory comes from the function the plan uses, so the
        // path printed here is the path the build writes to -- including in a
        // workspace with several roots, where each writes to `output_dir/<pkg>`.
        &harbour::builder::plan::package_output_dir(&build_ctx, subject_id, &build_roots)
            .join("lib"),
    )?;

    // The authoritative answer comes from the same build plan the builder
    // executes, not from a second walk of the surface. `harbour linkplan`
    // reporting one thing while the linker receives another is exactly how
    // declared-but-unpassed `frameworks` stayed hidden: the surface listed
    // them, `LinkStep` had no field for them, and the two never had to
    // agree. Deriving the line from `LinkStep` means they cannot disagree.
    let plan = BuildPlan::with_root_packages(
        &build_ctx,
        &resolve,
        &mut source_cache,
        &build_roots,
        Some(std::slice::from_ref(&args.target)),
    )?;
    let link_step = plan.link_steps.iter().find(|s| s.target == args.target);

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

    // What the linker actually receives, in order.
    match link_step {
        Some(step) => {
            println!();
            println!("Link line (what the linker receives, in order):");
            for obj in &step.objects {
                println!("  {}", obj.display());
            }
            for dir in &step.lib_dirs {
                println!("  -L{}", dir.display());
            }
            for lib in &step.libs {
                println!("  {lib}");
            }
            for framework in &step.frameworks {
                println!("  -framework {framework}");
            }
            for flag in &step.ldflags {
                println!("  {flag}");
            }
        }
        None => {
            println!();
            println!(
                "Link line: none -- `{}` is a {:?} and is archived, not linked.",
                args.target, target.kind
            );
        }
    }

    Ok(())
}
