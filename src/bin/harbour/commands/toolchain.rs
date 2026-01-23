//! `harbour toolchain` command

use anyhow::Result;

use crate::cli::{ToolchainArgs, ToolchainCommands};
use harbour::core::abi::TargetTriple;
use harbour::util::process::{find_ar, find_c_compiler};

pub fn execute(args: ToolchainArgs) -> Result<()> {
    match args.command {
        ToolchainCommands::Show => show_toolchain(),
        ToolchainCommands::Override(override_args) => {
            eprintln!("Toolchain override not yet implemented");
            eprintln!();
            eprintln!("Specify CC and AR environment variables instead:");
            if let Some(cc) = override_args.cc {
                eprintln!("  export CC={}", cc.display());
            }
            if let Some(ar) = override_args.ar {
                eprintln!("  export AR={}", ar.display());
            }
            Ok(())
        }
    }
}

fn show_toolchain() -> Result<()> {
    println!("Toolchain:");
    println!();

    // C compiler
    if let Some(cc) = find_c_compiler() {
        println!("  CC:     {}", cc.display());

        // Try to get version
        let output = std::process::Command::new(&cc)
            .arg("--version")
            .output();

        if let Ok(output) = output {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Some(first_line) = stdout.lines().next() {
                println!("          {}", first_line.trim());
            }
        }
    } else {
        println!("  CC:     not found");
    }

    // Archiver
    if let Some(ar) = find_ar() {
        println!("  AR:     {}", ar.display());
    } else {
        println!("  AR:     not found");
    }

    println!();

    // Target
    let target = TargetTriple::host();
    println!("  Target: {}", target);
    println!("    Arch: {}", target.arch);
    println!("    OS:   {}", target.os);
    if let Some(ref env) = target.env {
        println!("    Env:  {}", env);
    }

    println!();

    // Environment variables
    println!("Environment:");
    if let Ok(cc) = std::env::var("CC") {
        println!("  CC={}", cc);
    }
    if let Ok(ar) = std::env::var("AR") {
        println!("  AR={}", ar);
    }
    if let Ok(cflags) = std::env::var("CFLAGS") {
        println!("  CFLAGS={}", cflags);
    }
    if let Ok(ldflags) = std::env::var("LDFLAGS") {
        println!("  LDFLAGS={}", ldflags);
    }

    Ok(())
}
