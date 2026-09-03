//! `harbour tree` command

use std::collections::HashSet;

use anyhow::Result;

use crate::cli::TreeArgs;
use harbour::core::target::TargetTriple;
use harbour::core::Workspace;
use harbour::ops::resolve::resolve_workspace;
use harbour::resolver::Resolve;
use harbour::sources::SourceCache;
use harbour::util::config::load_config;
use harbour::util::{GlobalContext, VcpkgIntegration};
use harbour::PackageId;

pub fn execute(args: TreeArgs) -> Result<()> {
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

    // Find root package
    let root_id = ws.root_package_id();

    // Print tree
    let mut seen = HashSet::new();
    print_tree(
        &resolve,
        root_id,
        0,
        "",
        0,
        1,
        args.depth.unwrap_or(usize::MAX),
        &mut seen,
        args.duplicates,
    );

    Ok(())
}

/// The box-drawing connector for a child at `index` of `total` siblings.
///
/// Every sibling but the last gets a tee (`├── `), the last gets an elbow
/// (`└── `). The code this replaces always printed a tee - it never looked
/// at the child's position among its siblings at all - so the last child at
/// any depth rendered as if it had more siblings below it (see the
/// near-identical bug fixed in `explain.rs`'s `chain_connector`, which *did*
/// look at position but checked the wrong end of the list).
fn tree_connector(index: usize, total: usize) -> &'static str {
    if index + 1 >= total {
        "\u{2514}\u{2500}\u{2500} " // "└── "
    } else {
        "\u{251c}\u{2500}\u{2500} " // "├── "
    }
}

/// What a node contributes to its children's indentation.
///
/// A bar if more siblings follow it, blanks if it was the last. Repeating
/// "|   " once per level unconditionally drew a bar underneath a last child,
/// making the tree read as though that branch continued.
fn child_indent(is_last: bool) -> &'static str {
    if is_last {
        "    "
    } else {
        "\u{2502}   "
    }
}

#[allow(clippy::too_many_arguments)]
fn print_tree(
    resolve: &Resolve,
    pkg_id: PackageId,
    depth: usize,
    // The already-rendered indentation for this node's ancestors. Passed down
    // rather than recomputed from `depth`, because whether each ancestor level
    // draws a vertical bar or blank space depends on whether that ancestor was
    // its parent's last child -- information `depth` alone does not carry.
    ancestor_prefix: &str,
    index: usize,
    total_siblings: usize,
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
    let is_last = index + 1 >= total_siblings;
    let prefix = if depth == 0 {
        String::new()
    } else {
        format!(
            "{}{}",
            ancestor_prefix,
            tree_connector(index, total_siblings)
        )
    };

    // What this node contributes to its own children's indentation: a bar if
    // more siblings follow it, blanks if it was the last. Repeating "│   " per
    // level unconditionally drew a bar under a last child, making the tree
    // look as though the branch continued.
    let child_prefix = if depth == 0 {
        String::new()
    } else {
        format!("{}{}", ancestor_prefix, child_indent(is_last))
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
    let total = deps.len();
    for (i, dep_id) in deps.into_iter().enumerate() {
        print_tree(
            resolve,
            dep_id,
            depth + 1,
            &child_prefix,
            i,
            total,
            max_depth,
            seen,
            show_duplicates,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{child_indent, tree_connector};

    const TEE: &str = "\u{251c}\u{2500}\u{2500} ";
    const ELBOW: &str = "\u{2514}\u{2500}\u{2500} ";

    #[test]
    fn last_sibling_gets_an_elbow_and_the_rest_get_tees() {
        assert_eq!(tree_connector(0, 3), TEE);
        assert_eq!(tree_connector(1, 3), TEE);
        assert_eq!(tree_connector(2, 3), ELBOW);
    }

    #[test]
    fn a_last_child_indents_its_own_children_with_blanks() {
        // The bug this guards: a vertical bar drawn under a last child makes
        // the tree look like the branch continues past it.
        assert_eq!(child_indent(true), "    ");
        assert_eq!(child_indent(false), "\u{2502}   ");
        // Both must be the same display width, or deeper levels misalign.
        assert_eq!(
            child_indent(true).chars().count(),
            child_indent(false).chars().count()
        );
    }

    #[test]
    fn a_lone_sibling_gets_an_elbow() {
        assert_eq!(tree_connector(0, 1), ELBOW);
    }

    #[test]
    fn first_of_many_is_a_tee_not_an_elbow() {
        assert_eq!(tree_connector(0, 2), TEE);
    }

    #[test]
    fn an_out_of_range_index_does_not_panic() {
        assert_eq!(tree_connector(5, 0), ELBOW);
    }
}
