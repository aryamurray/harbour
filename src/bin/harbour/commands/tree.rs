//! `harbour tree` command

use std::collections::HashSet;

use anyhow::Result;

use crate::cli::TreeArgs;
use harbour::core::Workspace;
use harbour::ops::resolve::resolve_workspace;
use harbour::resolver::Resolve;
use harbour::sources::SourceCache;
use harbour::util::GlobalContext;
use harbour::PackageId;

pub fn execute(args: TreeArgs) -> Result<()> {
    let ctx = GlobalContext::new()?;

    let manifest_path = ctx.find_manifest().ok_or_else(|| {
        anyhow::anyhow!(
            "could not find Harbor.toml in {} or any parent directory\n\
             help: Run `harbour init` to create a new project",
            ctx.cwd().display()
        )
    })?;

    let ws = Workspace::new(&manifest_path, &ctx)?;
    let mut source_cache = SourceCache::new(ctx.cache_dir());

    let resolve = resolve_workspace(&ws, &mut source_cache)?;

    // Find root package
    let root_id = ws.root_package_id();

    // Print tree
    let mut seen = HashSet::new();
    print_tree(&resolve, root_id, 0, args.depth.unwrap_or(usize::MAX), &mut seen, args.duplicates);

    Ok(())
}

fn print_tree(
    resolve: &Resolve,
    pkg_id: PackageId,
    depth: usize,
    max_depth: usize,
    seen: &mut HashSet<PackageId>,
    show_duplicates: bool,
) {
    if depth > max_depth {
        return;
    }

    let is_duplicate = seen.contains(&pkg_id);
    seen.insert(pkg_id);

    // Print package
    let prefix = if depth == 0 {
        String::new()
    } else {
        format!("{}├── ", "│   ".repeat(depth - 1))
    };

    let dup_marker = if is_duplicate && !show_duplicates {
        " (*)"
    } else {
        ""
    };

    println!(
        "{}{} v{}{}",
        prefix,
        pkg_id.name(),
        pkg_id.version(),
        dup_marker
    );

    // Don't recurse into duplicates unless explicitly requested
    if is_duplicate && !show_duplicates {
        return;
    }

    // Print dependencies
    let deps = resolve.deps(pkg_id);
    for dep_id in deps {
        print_tree(resolve, dep_id, depth + 1, max_depth, seen, show_duplicates);
    }
}
