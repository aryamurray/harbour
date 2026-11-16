//! `harbour init` command

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

use crate::cli::InitArgs;
use harbour::ops::harbour_new::{init_project, NewOptions};

/// Determines the package name from the arguments or directory.
///
/// This is extracted for testability.
pub fn determine_package_name(name: &Option<String>, path: &Path) -> String {
    name.clone().unwrap_or_else(|| {
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unnamed")
            .to_string()
    })
}

/// Validates a package name for common issues.
///
/// Returns Ok(()) if the name is valid, otherwise returns an error message.
pub fn validate_package_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() {
        return Err("package name cannot be empty");
    }

    if name.starts_with('-') || name.starts_with('_') {
        return Err("package name cannot start with a hyphen or underscore");
    }

    if name.starts_with('.') {
        return Err("package name cannot start with a dot");
    }

    // Check for invalid characters
    for c in name.chars() {
        if !c.is_alphanumeric() && c != '-' && c != '_' {
            return Err("package name contains invalid characters");
        }
    }

    Ok(())
}

pub fn execute(args: InitArgs) -> Result<()> {
    let path = args.path.unwrap_or_else(|| PathBuf::from("."));

    let name = determine_package_name(&args.name, &path);

    if let Err(msg) = validate_package_name(&name) {
        bail!(msg);
    }

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
