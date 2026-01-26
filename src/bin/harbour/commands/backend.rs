//! `harbour backend` command
//!
//! List, show, and check build backends.

use anyhow::Result;

use crate::cli::{BackendArgs, BackendCommands};
use harbour::builder::shim::{
    get_backend_summaries, BackendAvailability, BackendId, BackendRegistry,
};

pub fn execute(args: BackendArgs) -> Result<()> {
    match args.command {
        BackendCommands::List => list_backends(),
        BackendCommands::Show(show_args) => show_backend(&show_args.backend),
        BackendCommands::Check(check_args) => check_backend(&check_args.backend),
    }
}

fn list_backends() -> Result<()> {
    let registry = BackendRegistry::new();
    let summaries = get_backend_summaries(&registry);

    println!("Build Backends:");
    println!();

    for summary in summaries {
        let status = match &summary.availability {
            BackendAvailability::Available { version } => {
                format!("available ({})", version)
            }
            BackendAvailability::AlwaysAvailable => "built-in".to_string(),
            BackendAvailability::NotInstalled { .. } => "not installed".to_string(),
            BackendAvailability::VersionTooOld { found, required } => {
                format!("outdated ({} < {})", found, required)
            }
        };

        let cross = if summary.cross_compile { "yes" } else { "no" };
        let configure = if summary.requires_configure {
            "yes"
        } else {
            "no"
        };

        println!("  {} - {}", summary.id, summary.description);
        println!("    Status:     {}", status);
        println!("    Cross:      {}", cross);
        println!("    Configure:  {}", configure);
        println!();
    }

    Ok(())
}

fn show_backend(name: &str) -> Result<()> {
    let id: BackendId = name.parse().map_err(|e| anyhow::anyhow!("{}", e))?;
    let registry = BackendRegistry::new();

    let shim = registry
        .get(id)
        .ok_or_else(|| anyhow::anyhow!("backend '{}' not found in registry", name))?;

    let caps = shim.capabilities();
    let defaults = shim.defaults();

    println!("Backend: {}", caps.identity.id);
    println!();

    // Version info
    println!("Version:");
    println!("  Shim version:   {}", caps.identity.shim_version);
    if let Some(ref req) = caps.identity.backend_version_req {
        println!("  Required:       {}", req);
    }
    println!();

    // Phase support
    println!("Phase Support:");
    println!("  Configure: {:?}", caps.phases.configure);
    println!("  Build:     {:?}", caps.phases.build);
    println!("  Test:      {:?}", caps.phases.test);
    println!("  Install:   {:?}", caps.phases.install);
    println!("  Clean:     {:?}", caps.phases.clean);
    println!();

    // Platform support
    println!("Platform:");
    println!("  Cross-compile:   {}", caps.platform.cross_compile);
    println!("  Sysroot:         {}", caps.platform.sysroot_support);
    println!(
        "  Toolchain file:  {}",
        caps.platform.toolchain_file_support
    );
    println!("  Host-only:       {}", caps.platform.host_only);
    println!();

    // Artifact support
    println!("Artifacts:");
    println!("  Static lib:      {}", caps.artifacts.static_lib);
    println!("  Shared lib:      {}", caps.artifacts.shared_lib);
    println!("  Executable:      {}", caps.artifacts.executable);
    println!("  Header-only:     {}", caps.artifacts.header_only);
    println!();

    // Linkage
    println!("Linkage:");
    println!("  Static linking:  {}", caps.linkage.static_linking);
    println!("  Shared linking:  {}", caps.linkage.shared_linking);
    println!(
        "  Visibility:      {}",
        caps.linkage.symbol_visibility_control
    );
    println!("  RPATH:           {:?}", caps.linkage.rpath_handling);
    println!("  Runtime bundle:  {}", caps.linkage.runtime_bundle);
    println!();

    // Dependency injection
    println!("Dependency Injection:");
    println!(
        "  Methods:  {:?}",
        caps.dependency_injection.supported_methods
    );
    println!(
        "  Formats:  {:?}",
        caps.dependency_injection.consumable_formats
    );
    println!(
        "  Transitive: {:?}",
        caps.dependency_injection.transitive_handling
    );
    println!();

    // Export discovery
    println!("Export Discovery:");
    println!("  Discovery:        {:?}", caps.export_discovery.discovery);
    println!(
        "  Requires install: {}",
        caps.export_discovery.requires_install
    );
    println!();

    // Defaults
    println!("Defaults:");
    if let Some(ref gen) = defaults.preferred_generator {
        println!("  Generator:  {}", gen);
    }
    if !defaults.injection_order.is_empty() {
        println!("  Injection order: {:?}", defaults.injection_order);
    }
    println!();

    Ok(())
}

fn check_backend(name: &str) -> Result<()> {
    let id: BackendId = name.parse().map_err(|e| anyhow::anyhow!("{}", e))?;
    let registry = BackendRegistry::new();

    let shim = registry
        .get(id)
        .ok_or_else(|| anyhow::anyhow!("backend '{}' not found in registry", name))?;

    println!("Checking backend: {}", id);
    println!();

    match shim.availability()? {
        BackendAvailability::Available { version } => {
            println!("  Status: available");
            println!("  Version: {}", version);
            println!();
            println!("Backend '{}' is ready to use.", id);
        }
        BackendAvailability::AlwaysAvailable => {
            println!("  Status: built-in");
            println!();
            println!(
                "Backend '{}' is always available (no external tool required).",
                id
            );
        }
        BackendAvailability::NotInstalled { tool, install_hint } => {
            println!("  Status: not installed");
            println!("  Tool: {}", tool);
            println!();
            println!("To install: {}", install_hint);
            return Err(anyhow::anyhow!("{} not found", tool));
        }
        BackendAvailability::VersionTooOld { found, required } => {
            println!("  Status: version too old");
            println!("  Found: {}", found);
            println!("  Required: {}", required);
            println!();
            println!("Please upgrade {} to version {} or higher.", id, required);
            return Err(anyhow::anyhow!(
                "version {} found, but {} required",
                found,
                required
            ));
        }
    }

    Ok(())
}
