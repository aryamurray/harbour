//! Implementation of `harbour new` and `harbour init`.

use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::core::manifest::{generate_exe_manifest, generate_lib_manifest};
use crate::core::workspace::{MANIFEST_ALIAS, MANIFEST_NAME};

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

    // Check for existing manifest
    let manifest_path = path.join(MANIFEST_NAME);
    let alias_path = path.join(MANIFEST_ALIAS);
    if manifest_path.exists() || alias_path.exists() {
        bail!(
            "manifest already exists in `{}` ({} or {})",
            path.display(),
            MANIFEST_NAME,
            MANIFEST_ALIAS
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
        .with_context(|| format!("failed to write {}", MANIFEST_NAME))?;

    // Create source directories
    let src_dir = path.join("src");
    fs::create_dir_all(&src_dir).with_context(|| "failed to create src directory")?;

    // Create initial source file
    if opts.lib {
        // Create include directory for libraries
        let include_dir = path.join("include").join(&opts.name);
        fs::create_dir_all(&include_dir).with_context(|| "failed to create include directory")?;

        // The package name may contain `-` (`my-lib` is a fine package name and
        // a fine directory), which no C compiler accepts in a macro or function
        // name. Everything C-shaped uses the identifier form; the include path
        // keeps the real name.
        let ident = c_identifier(&opts.name);

        // Write header file
        let header_content = format!(
            r#"#ifndef {ident_upper}_H
#define {ident_upper}_H

/**
 * Initialize the {name} library.
 */
void {ident}_init(void);

#endif /* {ident_upper}_H */
"#,
            name = opts.name,
            ident = ident,
            ident_upper = ident.to_uppercase()
        );
        fs::write(include_dir.join(format!("{}.h", opts.name)), header_content)?;

        // Write source file
        let source_content = format!(
            r#"#include "{name}/{name}.h"

void {ident}_init(void) {{
    // Initialize the library
}}
"#,
            name = opts.name,
            ident = ident
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

/// `name` as a C identifier: every character that is not an ASCII letter, digit
/// or underscore becomes `_`, and a leading digit gets a `_` in front.
///
/// `harbour new --lib base-lib` used to write `void base-lib_init(void);` and
/// `#ifndef BASE-LIB_H`, which no compiler accepts. GCC and clang only warn about
/// the guard before failing on the function; MSVC fails on the guard itself
/// (C2008). Found by building the generated library on a Windows runner, and
/// reproduced on macOS.
fn c_identifier(name: &str) -> String {
    let mut ident: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if ident.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        ident.insert(0, '_');
    }
    ident
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_package_name_becomes_a_valid_c_identifier() {
        assert_eq!(c_identifier("base-lib"), "base_lib");
        assert_eq!(c_identifier("my.lib+x"), "my_lib_x");
        assert_eq!(c_identifier("zlib"), "zlib");
        assert_eq!(c_identifier("already_ok1"), "already_ok1");
        assert_eq!(c_identifier("2fast"), "_2fast");
    }

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

        assert!(project_dir.join(MANIFEST_NAME).exists());
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

        assert!(project_dir.join(MANIFEST_NAME).exists());
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

        assert!(tmp.path().join(MANIFEST_NAME).exists());
    }
}
