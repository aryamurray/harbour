//! `harbour add` command

use anyhow::{bail, Result};

use crate::cli::AddArgs;
use harbour::ops::harbour_add::{add_dependency, AddOptions};
use harbour::util::GlobalContext;

pub fn execute(args: AddArgs) -> Result<()> {
    // Validate arguments
    if args.path.is_none() && args.git.is_none() {
        bail!(
            "must specify either --path or --git for dependency `{}`",
            args.name
        );
    }

    if args.path.is_some() && args.git.is_some() {
        bail!("cannot specify both --path and --git");
    }

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

    eprintln!("      Adding {} to dependencies", args.name);

    Ok(())
}
