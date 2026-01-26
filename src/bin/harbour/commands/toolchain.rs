//! `harbour toolchain` command

use std::path::PathBuf;

use anyhow::{bail, Context, Result};

use crate::cli::{ToolchainArgs, ToolchainCommands, ToolchainOverrideArgs};
use harbour::core::abi::TargetTriple;
use harbour::util::config::{
    global_toolchain_config_path, load_toolchain_config, project_toolchain_config_path,
    ToolchainConfig,
};
use harbour::util::process::{find_ar, find_c_compiler};

pub fn execute(args: ToolchainArgs) -> Result<()> {
    match args.command {
        ToolchainCommands::Show => show_toolchain(),
        ToolchainCommands::Override(override_args) => toolchain_override(override_args),
    }
}

fn toolchain_override(args: ToolchainOverrideArgs) -> Result<()> {
    // Determine which config file to use
    let config_path = if args.global {
        global_toolchain_config_path()
            .ok_or_else(|| anyhow::anyhow!("could not determine home directory"))?
    } else {
        // Use project-local config in current directory
        let cwd = std::env::current_dir().context("failed to get current directory")?;
        project_toolchain_config_path(&cwd)
    };

    // Handle --clear flag
    if args.clear {
        if config_path.exists() {
            std::fs::remove_file(&config_path)
                .with_context(|| format!("failed to remove config file: {}", config_path.display()))?;
            println!("Cleared toolchain overrides from {}", config_path.display());
        } else {
            println!("No toolchain overrides to clear at {}", config_path.display());
        }
        return Ok(());
    }

    // Check if any options were provided
    let has_options = args.cc.is_some()
        || args.cxx.is_some()
        || args.ar.is_some()
        || args.target.is_some()
        || !args.cflags.is_empty()
        || !args.cxxflags.is_empty()
        || !args.ldflags.is_empty();

    if !has_options {
        // Show current overrides and usage
        show_current_overrides(&config_path)?;
        println!();
        println!("Usage: harbour toolchain override [OPTIONS]");
        println!();
        println!("Options:");
        println!("  --cc <PATH>         Set C compiler path");
        println!("  --cxx <PATH>        Set C++ compiler path");
        println!("  --ar <PATH>         Set archiver path");
        println!("  --target <TRIPLE>   Set target triple for cross-compilation");
        println!("  --cflag <FLAG>      Add C compiler flag (can be repeated)");
        println!("  --cxxflag <FLAG>    Add C++ compiler flag (can be repeated)");
        println!("  --ldflag <FLAG>     Add linker flag (can be repeated)");
        println!("  --global            Save to global config (~/.harbour/toolchain.toml)");
        println!("  --clear             Clear all toolchain overrides");
        println!();
        println!("Examples:");
        println!("  harbour toolchain override --cc /usr/bin/clang --ar /usr/bin/llvm-ar");
        println!("  harbour toolchain override --cflag -Wall --cflag -Wextra");
        println!("  harbour toolchain override --target x86_64-unknown-linux-gnu");
        return Ok(());
    }

    // Load existing config or create new one
    let mut config = if config_path.exists() {
        ToolchainConfig::load(&config_path).unwrap_or_default()
    } else {
        ToolchainConfig::default()
    };

    // Update config with provided options
    if let Some(cc) = args.cc {
        // Validate the path exists
        if !cc.exists() {
            bail!("C compiler not found at: {}", cc.display());
        }
        config.toolchain.cc = Some(cc);
    }

    if let Some(cxx) = args.cxx {
        if !cxx.exists() {
            bail!("C++ compiler not found at: {}", cxx.display());
        }
        config.toolchain.cxx = Some(cxx);
    }

    if let Some(ar) = args.ar {
        if !ar.exists() {
            bail!("Archiver not found at: {}", ar.display());
        }
        config.toolchain.ar = Some(ar);
    }

    if let Some(target) = args.target {
        config.toolchain.target = Some(target);
    }

    if !args.cflags.is_empty() {
        config.toolchain.cflags = args.cflags;
    }

    if !args.cxxflags.is_empty() {
        config.toolchain.cxxflags = args.cxxflags;
    }

    if !args.ldflags.is_empty() {
        config.toolchain.ldflags = args.ldflags;
    }

    // Save the config
    config.save(&config_path)?;

    println!("Toolchain configuration saved to {}", config_path.display());
    println!();
    show_config_contents(&config);

    Ok(())
}

fn show_current_overrides(config_path: &PathBuf) -> Result<()> {
    if config_path.exists() {
        let config = ToolchainConfig::load(config_path)?;
        if config.has_overrides() {
            println!("Current toolchain overrides ({}):", config_path.display());
            println!();
            show_config_contents(&config);
            return Ok(());
        }
    }
    println!("No toolchain overrides configured.");
    Ok(())
}

fn show_config_contents(config: &ToolchainConfig) {
    let tc = &config.toolchain;

    if let Some(ref cc) = tc.cc {
        println!("  cc  = {}", cc.display());
    }
    if let Some(ref cxx) = tc.cxx {
        println!("  cxx = {}", cxx.display());
    }
    if let Some(ref ar) = tc.ar {
        println!("  ar  = {}", ar.display());
    }
    if let Some(ref target) = tc.target {
        println!("  target = {}", target);
    }
    if !tc.cflags.is_empty() {
        println!("  cflags = {:?}", tc.cflags);
    }
    if !tc.cxxflags.is_empty() {
        println!("  cxxflags = {:?}", tc.cxxflags);
    }
    if !tc.ldflags.is_empty() {
        println!("  ldflags = {:?}", tc.ldflags);
    }
}

fn show_toolchain() -> Result<()> {
    // Load toolchain config
    let cwd = std::env::current_dir().context("failed to get current directory")?;
    let global_path = global_toolchain_config_path();
    let project_path = project_toolchain_config_path(&cwd);

    let toolchain_config = if let Some(ref global) = global_path {
        load_toolchain_config(global, &project_path)
    } else {
        load_toolchain_config(&PathBuf::new(), &project_path)
    };

    // Show config file overrides first if any
    if toolchain_config.has_overrides() {
        println!("Toolchain Overrides:");
        println!();
        show_config_contents(&toolchain_config);
        println!();
    }

    println!("Effective Toolchain:");
    println!();

    // C compiler - check config first, then env, then detect
    let effective_cc = toolchain_config
        .toolchain
        .cc
        .clone()
        .or_else(|| std::env::var("CC").ok().map(PathBuf::from))
        .or_else(find_c_compiler);

    if let Some(ref cc) = effective_cc {
        println!("  CC:     {}", cc.display());

        // Try to get version
        let output = std::process::Command::new(cc).arg("--version").output();

        if let Ok(output) = output {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Some(first_line) = stdout.lines().next() {
                println!("          {}", first_line.trim());
            }
        }
    } else {
        println!("  CC:     not found");
    }

    // C++ compiler
    let effective_cxx = toolchain_config
        .toolchain
        .cxx
        .clone()
        .or_else(|| std::env::var("CXX").ok().map(PathBuf::from));

    if let Some(ref cxx) = effective_cxx {
        println!("  CXX:    {}", cxx.display());
    }

    // Archiver - check config first, then env, then detect
    let effective_ar = toolchain_config
        .toolchain
        .ar
        .clone()
        .or_else(|| std::env::var("AR").ok().map(PathBuf::from))
        .or_else(find_ar);

    if let Some(ref ar) = effective_ar {
        println!("  AR:     {}", ar.display());
    } else {
        println!("  AR:     not found");
    }

    println!();

    // Target
    let target = if let Some(ref t) = toolchain_config.toolchain.target {
        println!("  Target: {} (from config)", t);
        t.clone()
    } else {
        let host = TargetTriple::host();
        println!("  Target: {} (host)", host);
        host.to_string()
    };

    // Parse and show target details
    if let Some(target_triple) = TargetTriple::parse(&target) {
        println!("    Arch: {}", target_triple.arch);
        println!("    OS:   {}", target_triple.os);
        if let Some(ref env) = target_triple.env {
            println!("    Env:  {}", env);
        }
    }

    println!();

    // Show configured flags
    if !toolchain_config.toolchain.cflags.is_empty() {
        println!("  CFLAGS:   {}", toolchain_config.toolchain.cflags.join(" "));
    }
    if !toolchain_config.toolchain.cxxflags.is_empty() {
        println!("  CXXFLAGS: {}", toolchain_config.toolchain.cxxflags.join(" "));
    }
    if !toolchain_config.toolchain.ldflags.is_empty() {
        println!("  LDFLAGS:  {}", toolchain_config.toolchain.ldflags.join(" "));
    }

    println!();

    // Environment variables
    println!("Environment Variables:");
    let mut has_env = false;
    if let Ok(cc) = std::env::var("CC") {
        println!("  CC={}", cc);
        has_env = true;
    }
    if let Ok(cxx) = std::env::var("CXX") {
        println!("  CXX={}", cxx);
        has_env = true;
    }
    if let Ok(ar) = std::env::var("AR") {
        println!("  AR={}", ar);
        has_env = true;
    }
    if let Ok(cflags) = std::env::var("CFLAGS") {
        println!("  CFLAGS={}", cflags);
        has_env = true;
    }
    if let Ok(cxxflags) = std::env::var("CXXFLAGS") {
        println!("  CXXFLAGS={}", cxxflags);
        has_env = true;
    }
    if let Ok(ldflags) = std::env::var("LDFLAGS") {
        println!("  LDFLAGS={}", ldflags);
        has_env = true;
    }
    if !has_env {
        println!("  (none set)");
    }

    // Show config file locations
    println!();
    println!("Config Files:");
    if let Some(ref global) = global_path {
        if global.exists() {
            println!("  Global:  {} (active)", global.display());
        } else {
            println!("  Global:  {} (not found)", global.display());
        }
    }
    if project_path.exists() {
        println!("  Project: {} (active)", project_path.display());
    } else {
        println!("  Project: {} (not found)", project_path.display());
    }

    Ok(())
}
