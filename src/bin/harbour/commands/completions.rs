//! `harbour completions` command
//!
//! Generates shell completions for various shells.

use std::io;

use anyhow::Result;
use clap::CommandFactory;
use clap_complete::generate;

use crate::cli::{Cli, CompletionsArgs};

pub fn execute(args: CompletionsArgs) -> Result<()> {
    let mut cmd = Cli::command();
    let name = cmd.get_name().to_string();

    generate(args.shell, &mut cmd, name, &mut io::stdout());

    Ok(())
}
