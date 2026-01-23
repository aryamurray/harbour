//! `harbour clean` command

use anyhow::Result;

use crate::cli::CleanArgs;
use harbour::util::fs::remove_dir_all_if_exists;
use harbour::util::GlobalContext;

pub fn execute(args: CleanArgs) -> Result<()> {
    let ctx = GlobalContext::new()?;

    let harbour_dir = ctx.project_harbour_dir();

    if args.all {
        // Remove entire .harbour directory
        remove_dir_all_if_exists(&harbour_dir)?;
        eprintln!("     Removed {}", harbour_dir.display());
    } else if args.target {
        // Only remove target directory
        let target_dir = ctx.target_dir();
        remove_dir_all_if_exists(&target_dir)?;
        eprintln!("     Removed {}", target_dir.display());
    } else {
        // Default: remove target directory
        let target_dir = ctx.target_dir();
        remove_dir_all_if_exists(&target_dir)?;
        eprintln!("     Removed {}", target_dir.display());
    }

    Ok(())
}
