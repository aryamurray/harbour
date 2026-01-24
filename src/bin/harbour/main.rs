//! Harbour CLI - A Cargo-like package manager for C

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

mod cli;
mod commands;

use cli::{Cli, Commands};

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {:#}", e);
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    // Parse CLI
    let cli = Cli::parse();

    // Set up logging
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

    // Execute command
    match cli.command {
        Commands::New(args) => commands::new::execute(args),
        Commands::Init(args) => commands::init::execute(args),
        Commands::Build(args) => commands::build::execute(args),
        Commands::Add(args) => commands::add::execute(args),
        Commands::Remove(args) => commands::remove::execute(args),
        Commands::Update(args) => commands::update::execute(args),
        Commands::Clean(args) => commands::clean::execute(args),
        Commands::Tree(args) => commands::tree::execute(args),
        Commands::Flags(args) => commands::flags::execute(args),
        Commands::Explain(args) => commands::explain::execute(args),
        Commands::Linkplan(args) => commands::linkplan::execute(args),
        Commands::Test(args) => commands::test::execute(args),
        Commands::Toolchain(args) => commands::toolchain::execute(args),
        Commands::Backend(args) => commands::backend::execute(args),
        Commands::Ffi(args) => commands::ffi::execute(args),
        Commands::Completions(args) => commands::completions::execute(args),
    }
}
