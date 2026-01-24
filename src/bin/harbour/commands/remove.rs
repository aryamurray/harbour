//! `harbour remove` command

use anyhow::Result;

use crate::cli::RemoveArgs;
use harbour::ops::harbour_add::remove_dependency;
use harbour::util::GlobalContext;

pub fn execute(args: RemoveArgs) -> Result<()> {
    let ctx = GlobalContext::new()?;

    let manifest_path = ctx.find_manifest()?;

    remove_dependency(&manifest_path, &args.name)?;

    eprintln!("    Removing {} from dependencies", args.name);

    Ok(())
}
