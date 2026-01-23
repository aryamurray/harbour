//! `harbour new` command

use std::path::PathBuf;

use anyhow::Result;

use crate::cli::NewArgs;
use harbour::ops::harbour_new::{new_project, NewOptions};

pub fn execute(args: NewArgs) -> Result<()> {
    let path = args.path.unwrap_or_else(|| PathBuf::from(&args.name));

    let opts = NewOptions {
        name: args.name.clone(),
        lib: args.lib,
        init: false,
    };

    new_project(&path, &opts)?;

    let kind = if args.lib { "library" } else { "binary" };
    eprintln!("     Created {} `{}` package", kind, args.name);

    Ok(())
}
