//! `harbour new` command

use std::path::PathBuf;

use anyhow::{bail, Result};

use super::init::validate_package_name;
use crate::cli::NewArgs;
use harbour::ops::harbour_new::{new_project, NewOptions};

/// Determines the output path for a new project.
///
/// If a path is explicitly specified, uses that. Otherwise, creates a directory
/// with the same name as the project.
pub fn determine_project_path(name: &str, path: &Option<PathBuf>) -> PathBuf {
    path.clone().unwrap_or_else(|| PathBuf::from(name))
}

/// Validates that the target directory doesn't already exist or is empty.
///
/// Returns Ok(()) if the path is suitable for a new project.
pub fn validate_project_path(path: &PathBuf) -> Result<(), String> {
    if path.exists() {
        if path.is_file() {
            return Err(format!(
                "destination `{}` already exists and is a file",
                path.display()
            ));
        }

        // Check if directory is non-empty
        if let Ok(entries) = std::fs::read_dir(path) {
            if entries.count() > 0 {
                return Err(format!(
                    "destination `{}` already exists and is not empty",
                    path.display()
                ));
            }
        }
    }

    Ok(())
}

pub fn execute(args: NewArgs) -> Result<()> {
    let path = determine_project_path(&args.name, &args.path);

    if let Err(msg) = validate_package_name(&args.name) {
        bail!(msg);
    }

    if let Err(msg) = validate_project_path(&path) {
        bail!(msg);
    }

    let opts = NewOptions {
        name: args.name.clone(),
        lib: args.lib,
        init: false,
    };

    new_project(&path, &opts)?;

    let kind = if args.lib { "library" } else { "binary" };
    eprintln!("     Created {} `{}` package", kind, args.name);

    Ok(())
}
