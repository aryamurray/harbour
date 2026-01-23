//! Implementation of `harbour new` and `harbour init`.

use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::core::manifest::{generate_exe_manifest, generate_lib_manifest};

/// Options for creating a new project.
#[derive(Debug, Clone)]
pub struct NewOptions {
    /// Project name
    pub name: String,

    /// Create a library project
    pub lib: bool,

    /// Initialize in existing directory
    pub init: bool,
}

/// Create a new Harbour project.
pub fn new_project(path: &Path, opts: &NewOptions) -> Result<()> {
    // Check if directory already exists
    if path.exists() && !opts.init {
        bail!(
            "destination `{}` already exists\n\
             \n\
             Use `harbour init` to initialize an existing directory.",
            path.display()
        );
    }

    // Create directory if needed
    if !path.exists() {
        fs::create_dir_all(path)
            .with_context(|| format!("failed to create directory: {}", path.display()))?;
    }

    // Check for existing Harbor.toml
    let manifest_path = path.join("Harbor.toml");
    if manifest_path.exists() {
        bail!(
            "`Harbor.toml` already exists in `{}`",
            path.display()
        );
    }

    // Generate manifest
    let manifest_content = if opts.lib {
        generate_lib_manifest(&opts.name)
    } else {
        generate_exe_manifest(&opts.name)
    };

    // Write manifest
    fs::write(&manifest_path, &manifest_content)
        .with_context(|| "failed to write Harbor.toml")?;

    // Create source directories
    let src_dir = path.join("src");
    fs::create_dir_all(&src_dir).with_context(|| "failed to create src directory")?;

    // Create initial source file
    if opts.lib {
        // Create include directory for libraries
        let include_dir = path.join("include").join(&opts.name);
        fs::create_dir_all(&include_dir)
            .with_context(|| "failed to create include directory")?;

        // Write header file
        let header_content = format!(
            r#"#ifndef {name_upper}_H
#define {name_upper}_H

/**
 * Initialize the {name} library.
 */
void {name}_init(void);

#endif /* {name_upper}_H */
"#,
            name = opts.name,
            name_upper = opts.name.to_uppercase()
        );
        fs::write(include_dir.join(format!("{}.h", opts.name)), header_content)?;

        // Write source file
        let source_content = format!(
            r#"#include "{name}/{name}.h"

void {name}_init(void) {{
    // Initialize the library
}}
"#,
            name = opts.name
        );
        fs::write(src_dir.join("lib.c"), source_content)?;
    } else {
        // Write main.c for executables
        let main_content = r#"#include <stdio.h>

int main(int argc, char *argv[]) {
    printf("Hello, Harbour!\n");
    return 0;
}
"#;
        fs::write(src_dir.join("main.c"), main_content)?;
    }

    // Create .gitignore
    let gitignore = r#"# Harbour build artifacts
.harbour/

# Editor files
*.swp
*~
.vscode/
.idea/
"#;
    fs::write(path.join(".gitignore"), gitignore)?;

    Ok(())
}

/// Initialize a Harbour project in an existing directory.
pub fn init_project(path: &Path, opts: &NewOptions) -> Result<()> {
    let mut opts = opts.clone();
    opts.init = true;
    new_project(path, &opts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_new_project_lib() {
        let tmp = TempDir::new().unwrap();
        let project_dir = tmp.path().join("mylib");

        let opts = NewOptions {
            name: "mylib".to_string(),
            lib: true,
            init: false,
        };

        new_project(&project_dir, &opts).unwrap();

        assert!(project_dir.join("Harbor.toml").exists());
        assert!(project_dir.join("src/lib.c").exists());
        assert!(project_dir.join("include/mylib/mylib.h").exists());
    }

    #[test]
    fn test_new_project_exe() {
        let tmp = TempDir::new().unwrap();
        let project_dir = tmp.path().join("myapp");

        let opts = NewOptions {
            name: "myapp".to_string(),
            lib: false,
            init: false,
        };

        new_project(&project_dir, &opts).unwrap();

        assert!(project_dir.join("Harbor.toml").exists());
        assert!(project_dir.join("src/main.c").exists());
    }

    #[test]
    fn test_init_existing_dir() {
        let tmp = TempDir::new().unwrap();

        let opts = NewOptions {
            name: "existing".to_string(),
            lib: false,
            init: true,
        };

        init_project(tmp.path(), &opts).unwrap();

        assert!(tmp.path().join("Harbor.toml").exists());
    }
}
