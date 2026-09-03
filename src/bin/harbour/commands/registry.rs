//! `harbour registry` - inspect and maintain Harbour registries.

use anyhow::{Context, Result};

use crate::cli::{RegistryArgs, RegistryCommands, RegistryIndexArgs};
use harbour::util::GlobalContext;

pub fn execute(args: RegistryArgs) -> Result<()> {
    match args.command {
        RegistryCommands::Index(args) => index(args),
        RegistryCommands::List => list(),
    }
}

/// Rebuild a registry's package index from its shims.
///
/// Resolution reads the index rather than the shim files, so an unindexed
/// registry resolves nothing -- a missing index record is indistinguishable
/// from a missing package. This is the command that closes that gap for a
/// hand-maintained registry.
fn index(args: RegistryIndexArgs) -> Result<()> {
    let ctx = GlobalContext::new()?;

    let registry_root = match args.path {
        Some(path) => path,
        None => std::env::current_dir().context("failed to get current directory")?,
    };

    let config_path = registry_root.join("config.toml");
    if !config_path.exists() {
        anyhow::bail!(
            "`{}` does not look like a Harbour registry: no config.toml found\n\
             hint: pass the path to the registry checkout, or run this from inside it",
            registry_root.display()
        );
    }

    harbour::sources::registry::generate_index(&registry_root, &ctx.cache_dir())
        .with_context(|| format!("failed to index registry at {}", registry_root.display()))?;

    println!("Indexed {}", registry_root.display());
    println!("Commit the generated files under `index/` so consumers resolve the same content.");

    Ok(())
}

/// List the registries this project resolves against, in priority order.
fn list() -> Result<()> {
    let ctx = GlobalContext::new()?;
    let registries: Vec<_> = ctx.registries().enabled().collect();

    if registries.is_empty() {
        println!("No registries configured.");
        return Ok(());
    }

    for entry in registries {
        println!(
            "{:<16} {}  (priority {})",
            entry.name, entry.url, entry.priority
        );
    }

    Ok(())
}
