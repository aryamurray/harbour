//! Harbour CLI - A Cargo-like package manager for C

use std::sync::Arc;

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

use harbour::util::{ColorChoice, Shell};

mod cli;
mod commands;

use cli::{Cli, Commands, MessageFormat};

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {:#}", e);
        std::process::exit(1);
    }
}

/// Global CLI options that are passed to commands.
#[derive(Debug, Clone)]
pub struct GlobalOptions {
    pub shell: Arc<Shell>,
    pub offline: bool,
    pub locked: bool,
}

fn run() -> Result<()> {
    // Parse CLI
    let cli = Cli::parse();

    // Set up logging (only in non-quiet mode)
    if !cli.quiet {
        let filter = if cli.verbose {
            EnvFilter::new("harbour=debug")
        } else {
            EnvFilter::new("harbour=info")
        };

        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(false)
            .without_time()
            .init();
    }

    // Parse color choice
    let color_choice = cli
        .color
        .parse::<ColorChoice>()
        .unwrap_or(ColorChoice::Auto);

    // Determine if JSON mode is requested (only for build command)
    let json_mode =
        matches!(&cli.command, Commands::Build(args) if args.message_format == MessageFormat::Json);

    // Create shell with appropriate mode
    let shell = Arc::new(Shell::from_flags(
        cli.quiet,
        cli.verbose,
        color_choice,
        json_mode,
    ));

    // Create global options
    let global_opts = GlobalOptions {
        shell,
        offline: cli.offline,
        locked: cli.locked,
    };

    // Execute command
    match cli.command {
        Commands::New(args) => commands::new::execute(args),
        Commands::Init(args) => commands::init::execute(args),
        Commands::Build(args) => commands::build::execute(args, &global_opts),
        Commands::Add(args) => commands::add::execute(args, &global_opts),
        Commands::Remove(args) => commands::remove::execute(args, &global_opts),
        Commands::Update(args) => commands::update::execute(args, &global_opts),
        Commands::Clean(args) => commands::clean::execute(args),
        Commands::Cache(args) => commands::cache::execute(args),
        Commands::Tree(args) => commands::tree::execute(args),
        Commands::Flags(args) => commands::flags::execute(args),
        Commands::Explain(args) => commands::explain::execute(args),
        Commands::Linkplan(args) => commands::linkplan::execute(args),
        Commands::Test(args) => commands::test::execute(args),
        Commands::Toolchain(args) => commands::toolchain::execute(args),
        Commands::Backend(args) => commands::backend::execute(args),
        Commands::Ffi(args) => commands::ffi::execute(args),
        Commands::Doctor(args) => commands::doctor::execute(args, cli.verbose),
        Commands::Verify(args) => commands::verify::execute(args, cli.verbose),
        Commands::Completions(args) => commands::completions::execute(args),
        Commands::Search(args) => commands::search::execute(args),
    }
}
