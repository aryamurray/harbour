//! Filesystem utilities.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use glob::glob;
use url::Url;

/// Recursively copy a directory.
pub fn copy_dir_all(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)
        .with_context(|| format!("failed to create directory: {}", dst.display()))?;

    for entry in
        fs::read_dir(src).with_context(|| format!("failed to read directory: {}", src.display()))?
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
    fs::read_to_string(path).with_context(|| format!("failed to read file: {}", path.display()))
}

/// Write a string to a file, creating parent directories if needed.
pub fn write_string(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }
    fs::write(path, contents).with_context(|| format!("failed to write file: {}", path.display()))
}

/// Find files matching glob patterns relative to a base directory.
pub fn glob_files(base: &Path, patterns: &[String]) -> Result<Vec<PathBuf>> {
    glob_files_excluding(base, patterns, &[])
}

/// Expand `patterns` relative to `base`, then drop anything matching `exclude`.
///
/// Exclusion runs through the same glob machinery as inclusion, so `**` and
/// character classes behave identically in both -- an exclude pattern that
/// looks like an include pattern matches the same files.
///
/// This exists because real C libraries routinely ship programs beside their
/// library sources: libpng has `example.c` and `pngtest.c` at its root, each
/// defining `main()`, so `sources = ["*.c"]` would put two entry points into a
/// static library. The alternative is enumerating every source by hand.
pub fn glob_files_excluding(
    base: &Path,
    patterns: &[String],
    exclude: &[String],
) -> Result<Vec<PathBuf>> {
    let mut results = Vec::new();

    for pattern in patterns {
        // Make pattern absolute by joining with base
        let full_pattern = base.join(pattern);
        let pattern_str = full_pattern.to_string_lossy();

        // Handle glob patterns
        for entry in
            glob(&pattern_str).with_context(|| format!("invalid glob pattern: {}", pattern))?
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

    if !exclude.is_empty() {
        let mut excluded = std::collections::HashSet::new();
        for pattern in exclude {
            let full_pattern = base.join(pattern);
            let pattern_str = full_pattern.to_string_lossy();
            for entry in glob(&pattern_str)
                .with_context(|| format!("invalid exclude pattern: {}", pattern))?
            {
                match entry {
                    Ok(path) => {
                        excluded.insert(path);
                    }
                    Err(e) => tracing::warn!("glob error in exclude pattern: {}", e),
                }
            }
        }
        results.retain(|path| !excluded.contains(path));
    }

    results.sort();
    results.dedup();
    Ok(results)
}

/// Turn a URL into a directory name usable as a cache key.
///
/// Shared by the git and registry sources, which previously each carried a
/// byte-identical private copy -- and therefore each had to be fixed
/// separately for the Windows bug below.
///
/// The result is reduced to characters legal in a path on every platform. A
/// `file://` URL carries the drive letter in its path on Windows, so the naive
/// result is `-C:-Users-...`, and a colon cannot appear in a Windows directory
/// name -- creating the cache directory fails with "The directory name is
/// invalid". `https://` URLs are unaffected, since neither a host nor a URL
/// path contains a colon, which is why only local registries and local git
/// remotes hit it.
///
/// Filtering to an allowlist rather than blocklisting Windows-illegal
/// characters keeps the mapping identical on every platform, so a given URL
/// always names the same cache directory.
pub fn sanitize_url_for_path(url: &Url) -> String {
    let mut name = String::new();

    if let Some(host) = url.host_str() {
        name.push_str(host);
    }

    let path = url.path().trim_matches('/');
    if !path.is_empty() {
        name.push('-');
        name.push_str(&path.replace('/', "-"));
    }

    if name.ends_with(".git") {
        name.truncate(name.len() - 4);
    }

    let sanitized: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();

    sanitized.trim_matches('-').to_string()
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
    fn test_glob_files_excluding_drops_matching_files() {
        let tmp = TempDir::new().unwrap();
        for name in ["png.c", "pngread.c", "example.c", "pngtest.c"] {
            std::fs::write(tmp.path().join(name), "int x;").unwrap();
        }

        // The real libpng shape: library sources and standalone programs share
        // a directory, so a single glob would pull in two main() definitions.
        let files = glob_files_excluding(
            tmp.path(),
            &["*.c".to_string()],
            &["example.c".to_string(), "pngtest.c".to_string()],
        )
        .unwrap();

        let names: Vec<String> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["png.c", "pngread.c"]);
    }

    #[test]
    fn test_glob_files_excluding_accepts_glob_patterns() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("test")).unwrap();
        std::fs::write(tmp.path().join("lib.c"), "int x;").unwrap();
        std::fs::write(tmp.path().join("test/a_test.c"), "int x;").unwrap();
        std::fs::write(tmp.path().join("test/b_test.c"), "int x;").unwrap();

        // Exclusion goes through the same matcher as inclusion, so `**` works
        // in both -- an exclude that looks like an include behaves like one.
        let files = glob_files_excluding(
            tmp.path(),
            &["**/*.c".to_string()],
            &["test/**/*.c".to_string()],
        )
        .unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].file_name().unwrap(), "lib.c");
    }

    #[test]
    fn test_glob_files_without_exclusions_is_unchanged() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("a.c"), "int x;").unwrap();
        assert_eq!(
            glob_files(tmp.path(), &["*.c".to_string()]).unwrap(),
            glob_files_excluding(tmp.path(), &["*.c".to_string()], &[]).unwrap()
        );
    }

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
