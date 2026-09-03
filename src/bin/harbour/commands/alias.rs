//! `harbour alias` - create or remove the `harbor` spelling alias.
//!
//! Harbour previously shipped the alias as a second `[[bin]]` target pointing
//! at the same source. That compiled and tested the whole binary twice for a
//! spelling convenience: about a third more space in `target/debug/deps`, and
//! every unit test reported twice.
//!
//! An alias is a filesystem concern, so it is created as one. On Unix that is
//! a symlink. On Windows it is a small `.cmd` shim rather than a symlink,
//! because creating a symlink there needs either administrator rights or
//! Developer Mode, and requiring elevation to get a second spelling of a
//! command would be a worse trade than a two-line batch file.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::cli::AliasArgs;

/// File extension for the alias. Empty on Unix, where the alias is a symlink.
#[cfg(windows)]
const ALIAS_EXT: &str = "cmd";
#[cfg(not(windows))]
const ALIAS_EXT: &str = "";

pub fn execute(args: AliasArgs) -> Result<()> {
    let exe = std::env::current_exe().context("could not determine the path of this executable")?;

    let dir = match args.dir {
        Some(dir) => dir,
        None => exe
            .parent()
            .context("the running executable has no parent directory")?
            .to_path_buf(),
    };

    let alias_path = alias_path(&dir, &args.name);

    if args.remove {
        return remove_alias(&alias_path);
    }

    create_alias(&exe, &alias_path, args.force)
}

/// Where the alias file lives, including the platform-specific extension.
fn alias_path(dir: &Path, name: &str) -> PathBuf {
    let mut path = dir.join(name);
    if !ALIAS_EXT.is_empty() {
        path.set_extension(ALIAS_EXT);
    }
    path
}

/// Body of the Windows `.cmd` shim.
///
/// Compiled on Windows, where it is used, and in test builds on every
/// platform, so the shim's contents stay under test from a Unix machine --
/// which is where this project is actually developed, and therefore the only
/// place the assertions will normally run.
///
/// `%~dp0` expands to the directory of the shim itself including a trailing
/// backslash, so the shim keeps working if the install directory is moved or
/// renamed. `%*` forwards every argument.
#[cfg(any(windows, test))]
fn cmd_shim_body(target_file_name: &str) -> String {
    format!("@\"%~dp0{target_file_name}\" %*\r\n")
}

fn create_alias(exe: &Path, alias_path: &Path, force: bool) -> Result<()> {
    if alias_path.exists() {
        if !force {
            bail!(
                "{} already exists (pass --force to replace it)",
                alias_path.display()
            );
        }
        std::fs::remove_file(alias_path)
            .with_context(|| format!("failed to replace {}", alias_path.display()))?;
    }

    #[cfg(windows)]
    {
        let target = exe
            .file_name()
            .context("the running executable has no file name")?
            .to_string_lossy()
            .to_string();
        std::fs::write(alias_path, cmd_shim_body(&target))
            .with_context(|| format!("failed to write {}", alias_path.display()))?;
    }

    #[cfg(not(windows))]
    {
        std::os::unix::fs::symlink(exe, alias_path).with_context(|| {
            format!(
                "failed to link {} -> {}",
                alias_path.display(),
                exe.display()
            )
        })?;
    }

    println!("Created {} -> {}", alias_path.display(), exe.display());
    Ok(())
}

fn remove_alias(alias_path: &Path) -> Result<()> {
    // A dangling symlink does not satisfy `exists()`, which follows links, so
    // check the link itself. Otherwise `--remove` could not clean up an alias
    // left behind by an uninstalled binary, which is exactly when you need it.
    let present = alias_path.symlink_metadata().is_ok();
    if !present {
        bail!("{} does not exist", alias_path.display());
    }
    std::fs::remove_file(alias_path)
        .with_context(|| format!("failed to remove {}", alias_path.display()))?;
    println!("Removed {}", alias_path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alias_path_uses_the_platform_extension() {
        let path = alias_path(Path::new("/usr/local/bin"), "harbor");
        if cfg!(windows) {
            assert_eq!(path.file_name().unwrap(), "harbor.cmd");
        } else {
            assert_eq!(path.file_name().unwrap(), "harbor");
        }
    }

    #[test]
    fn cmd_shim_is_relative_to_itself_and_forwards_arguments() {
        let body = cmd_shim_body("harbour.exe");
        // Relative to the shim's own directory, so moving the install
        // directory does not break the alias.
        assert!(body.contains("%~dp0harbour.exe"));
        // All arguments forwarded, not just the first.
        assert!(body.contains("%*"));
        // Batch files want CRLF.
        assert!(body.ends_with("\r\n"));
    }

    #[test]
    fn creating_an_alias_produces_a_working_path() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = tmp.path().join(if cfg!(windows) {
            "harbour.exe"
        } else {
            "harbour"
        });
        std::fs::write(&exe, b"#!/bin/sh\ntrue\n").unwrap();

        let alias = alias_path(tmp.path(), "harbor");
        create_alias(&exe, &alias, false).unwrap();

        assert!(alias.symlink_metadata().is_ok(), "alias was not created");
        #[cfg(not(windows))]
        assert_eq!(std::fs::read_link(&alias).unwrap(), exe);
    }

    #[test]
    fn creating_refuses_to_clobber_without_force() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = tmp.path().join("harbour");
        std::fs::write(&exe, b"x").unwrap();
        let alias = alias_path(tmp.path(), "harbor");
        std::fs::write(&alias, b"something the user cares about").unwrap();

        let err = create_alias(&exe, &alias, false).unwrap_err();
        assert!(err.to_string().contains("--force"));
        // The pre-existing file must survive a refused create.
        assert_eq!(
            std::fs::read(&alias).unwrap(),
            b"something the user cares about"
        );
    }

    #[test]
    fn force_replaces_an_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = tmp.path().join("harbour");
        std::fs::write(&exe, b"x").unwrap();
        let alias = alias_path(tmp.path(), "harbor");
        std::fs::write(&alias, b"stale").unwrap();

        create_alias(&exe, &alias, true).unwrap();
        #[cfg(not(windows))]
        assert_eq!(std::fs::read_link(&alias).unwrap(), exe);
    }

    #[test]
    fn remove_reports_a_missing_alias_rather_than_succeeding() {
        let tmp = tempfile::tempdir().unwrap();
        let alias = alias_path(tmp.path(), "harbor");
        let err = remove_alias(&alias).unwrap_err();
        assert!(err.to_string().contains("does not exist"));
    }

    #[cfg(not(windows))]
    #[test]
    fn remove_cleans_up_a_dangling_symlink() {
        // The case that motivates checking symlink_metadata rather than
        // exists(): the target is gone, so exists() is false, but the link
        // is still sitting in the install directory.
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("harbour");
        let alias = alias_path(tmp.path(), "harbor");
        std::os::unix::fs::symlink(&missing, &alias).unwrap();

        assert!(!alias.exists(), "precondition: link should be dangling");
        remove_alias(&alias).unwrap();
        assert!(alias.symlink_metadata().is_err());
    }
}
