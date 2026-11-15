//! `harbour completions` command
//!
//! Generates shell completions for various shells.

use std::io::{self, Write};

use anyhow::Result;
use clap::CommandFactory;
use clap_complete::generate;

use crate::cli::{Cli, CompletionsArgs};

pub fn execute(args: CompletionsArgs) -> Result<()> {
    let mut cmd = Cli::command();
    let name = cmd.get_name().to_string();

    // `clap_complete::generate` `expect`s on every write, so a closed pipe
    // (`harbour completions bash | head -1`) panics with a backtrace. Generate
    // into memory, where a write cannot fail, and write to stdout ourselves.
    let mut script = Vec::new();
    generate(args.shell, &mut cmd, name, &mut script);

    let mut stdout = io::stdout().lock();
    match stdout.write_all(&script).and_then(|()| stdout.flush()) {
        // The reader stopped early: that is what `head` is for, not an error.
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        other => Ok(other?),
    }
}
