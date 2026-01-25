//! `harbour add` command

use anyhow::{bail, Result};

use crate::cli::AddArgs;
use harbour::ops::harbour_add::{add_dependency, AddOptions};
use harbour::util::GlobalContext;

pub fn execute(args: AddArgs) -> Result<()> {
    // Validate arguments - can't specify both path and git
    if args.path.is_some() && args.git.is_some() {
        bail!("cannot specify both --path and --git");
    }

    // If neither path nor git specified, it's a registry dependency
    // The version defaults to "*" if not specified

    let ctx = GlobalContext::new()?;

    let manifest_path = ctx.find_manifest()?;

    let opts = AddOptions {
        name: args.name.clone(),
        path: args.path,
        git: args.git,
        branch: args.branch,
        tag: args.tag,
        rev: args.rev,
        version: args.version,
        optional: args.optional,
    };

    add_dependency(&manifest_path, &opts)?;

    let source = if opts.path.is_some() {
        "path"
    } else if opts.git.is_some() {
        "git"
    } else {
        "registry"
    };

    eprintln!("      Adding {} ({}) to dependencies", args.name, source);

    Ok(())
}
