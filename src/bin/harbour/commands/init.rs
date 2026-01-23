//! `harbour init` command

use std::path::PathBuf;

use anyhow::Result;

use crate::cli::InitArgs;
use harbour::ops::harbour_new::{init_project, NewOptions};

pub fn execute(args: InitArgs) -> Result<()> {
    let path = args.path.unwrap_or_else(|| PathBuf::from("."));

    let name = args.name.unwrap_or_else(|| {
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unnamed")
            .to_string()
    });

    let opts = NewOptions {
        name: name.clone(),
        lib: args.lib,
        init: true,
    };

    init_project(&path, &opts)?;

    let kind = if args.lib { "library" } else { "binary" };
    eprintln!("     Initialized {} `{}` package", kind, name);

    Ok(())
}
