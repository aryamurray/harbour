//! Filesystem utilities.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use glob::glob;
use walkdir::WalkDir;

/// Recursively copy a directory.
pub fn copy_dir_all(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)
        .with_context(|| format!("failed to create directory: {}", dst.display()))?;

    for entry in fs::read_dir(src)
        .with_context(|| format!("failed to read directory: {}", src.display()))?
    {
        let entry = entry?;
        let ty = entry.file_type()?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if ty.is_dir() {
            copy_dir_all(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path).with_context(|| {
                format!(
                    "failed to copy {} to {}",
                    src_path.display(),
                    dst_path.display()
                )
            })?;
        }
    }
    Ok(())
}

/// Remove a directory and all its contents, if it exists.
pub fn remove_dir_all_if_exists(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_dir_all(path)
            .with_context(|| format!("failed to remove directory: {}", path.display()))?;
    }
    Ok(())
}

/// Ensure a directory exists, creating it if necessary.
pub fn ensure_dir(path: &Path) -> Result<()> {
    if !path.exists() {
        fs::create_dir_all(path)
            .with_context(|| format!("failed to create directory: {}", path.display()))?;
    }
    Ok(())
}

/// Read a file to string, with nice error messages.
pub fn read_to_string(path: &Path) -> Result<String> {
    fs::read_to_string(path)
        .with_context(|| format!("failed to read file: {}", path.display()))
}

/// Write a string to a file, creating parent directories if needed.
pub fn write_string(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }
    fs::write(path, contents)
        .with_context(|| format!("failed to write file: {}", path.display()))
}

/// Find files matching glob patterns relative to a base directory.
pub fn glob_files(base: &Path, patterns: &[String]) -> Result<Vec<PathBuf>> {
    let mut results = Vec::new();

    for pattern in patterns {
        // Make pattern absolute by joining with base
        let full_pattern = base.join(pattern);
        let pattern_str = full_pattern.to_string_lossy();

        // Handle glob patterns
        for entry in glob(&pattern_str)
            .with_context(|| format!("invalid glob pattern: {}", pattern))?
        {
            match entry {
                Ok(path) => {
                    if path.is_file() {
                        results.push(path);
                    }
                }
                Err(e) => {
                    tracing::warn!("glob error: {}", e);
                }
            }
        }
    }

    results.sort();
    results.dedup();
    Ok(results)
}

/// Canonicalize a path, but don't fail if it doesn't exist yet.
/// Returns the path as-is if canonicalization fails.
pub fn normalize_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Get the relative path from `base` to `path`.
pub fn relative_path(base: &Path, path: &Path) -> PathBuf {
    pathdiff::diff_paths(path, base).unwrap_or_else(|| path.to_path_buf())
}

/// Check if a path is inside another path.
pub fn is_inside(path: &Path, parent: &Path) -> bool {
    path.starts_with(parent)
}

/// Create a symlink (platform-aware).
#[cfg(unix)]
pub fn symlink(src: &Path, dst: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(src, dst)
}

#[cfg(windows)]
pub fn symlink(src: &Path, dst: &Path) -> io::Result<()> {
    if src.is_dir() {
        std::os::windows::fs::symlink_dir(src, dst)
    } else {
        std::os::windows::fs::symlink_file(src, dst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_glob_files() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("main.c"), "int main() {}").unwrap();
        fs::write(src.join("util.c"), "void util() {}").unwrap();
        fs::write(src.join("readme.txt"), "readme").unwrap();

        let files = glob_files(tmp.path(), &["src/**/*.c".to_string()]).unwrap();
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn test_copy_dir_all() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");

        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("file.txt"), "content").unwrap();

        copy_dir_all(&src, &dst).unwrap();

        assert!(dst.join("file.txt").exists());
        assert_eq!(fs::read_to_string(dst.join("file.txt")).unwrap(), "content");
    }
}
