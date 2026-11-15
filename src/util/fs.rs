//! Filesystem utilities.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
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
    Ok(expand_sources(base, patterns, exclude)?.files)
}

/// How many files one source pattern matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternMatch {
    pub pattern: String,
    /// Files the pattern matched before `exclude` was applied.
    pub matched: usize,
    /// Of those, the files that survived `exclude`.
    pub kept: usize,
}

/// The result of [`expand_sources`]: the file set plus, per pattern, what each
/// pattern contributed. The per-pattern counts are what let a caller say *which*
/// pattern is empty, rather than only that the total is.
#[derive(Debug, Clone, Default)]
pub struct SourceExpansion {
    /// Sorted, de-duplicated files that match a pattern and no `exclude`.
    pub files: Vec<PathBuf>,
    /// One entry per input pattern, in the order given.
    pub patterns: Vec<PatternMatch>,
}

/// [`glob_files_excluding`], keeping the per-pattern match counts.
pub fn expand_sources(
    base: &Path,
    patterns: &[String],
    exclude: &[String],
) -> Result<SourceExpansion> {
    reject_brace_patterns("`sources`", patterns)?;
    reject_brace_patterns("`exclude`", exclude)?;

    let mut excluded = std::collections::HashSet::new();
    for pattern in exclude {
        let full_pattern = base.join(pattern);
        let pattern_str = full_pattern.to_string_lossy();
        for entry in
            glob(&pattern_str).with_context(|| format!("invalid exclude pattern: {}", pattern))?
        {
            match entry {
                Ok(path) => {
                    excluded.insert(path);
                }
                Err(e) => tracing::warn!("glob error in exclude pattern: {}", e),
            }
        }
    }

    let mut results = Vec::new();
    let mut per_pattern = Vec::with_capacity(patterns.len());

    for pattern in patterns {
        // Make pattern absolute by joining with base
        let full_pattern = base.join(pattern);
        let pattern_str = full_pattern.to_string_lossy();
        let mut matched = 0;
        let mut kept = 0;

        // Handle glob patterns
        for entry in
            glob(&pattern_str).with_context(|| format!("invalid glob pattern: {}", pattern))?
        {
            match entry {
                Ok(path) => {
                    if path.is_file() {
                        matched += 1;
                        if !excluded.contains(&path) {
                            kept += 1;
                            results.push(path);
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("glob error: {}", e);
                }
            }
        }
        per_pattern.push(PatternMatch {
            pattern: pattern.clone(),
            matched,
            kept,
        });
    }

    results.sort();
    results.dedup();
    Ok(SourceExpansion {
        files: results,
        patterns: per_pattern,
    })
}

/// Whether `exclude` pattern `pattern` covers the manifest-relative path
/// `entry`, under the same rules the expansion applies.
///
/// Expansion walks the tree one path component at a time, so a `*` there never
/// crosses a `/`; `Pattern::matches` with default options has no such notion
/// and lets `src/*.c` match `src/a/b.c`. `require_literal_separator` restores
/// the directory-aware meaning, so "is this entry excluded" and "was this file
/// excluded" cannot give different answers for the same pattern.
pub fn exclude_covers(pattern: &str, entry: &str) -> bool {
    let options = glob::MatchOptions {
        require_literal_separator: true,
        ..glob::MatchOptions::new()
    };
    glob::Pattern::new(pattern)
        .map(|p| p.matches_with(entry, options))
        .unwrap_or(false)
}

/// Refuse a `{` or `}` in a file pattern.
///
/// The `glob` crate has no brace expansion, so `{adler32,crc32}.c` is a
/// pattern for a file literally named that -- it matches nothing, and (since
/// an empty glob is legal) used to do so silently. Expanding braces here
/// instead would invite questions about nesting and escaping for a feature
/// nobody needs; listing the alternatives is always possible, so say that.
///
/// `what` names the manifest key for the message, e.g. "`sources`".
pub fn reject_brace_patterns(what: &str, patterns: &[String]) -> Result<()> {
    for pattern in patterns {
        if !pattern.contains(['{', '}']) {
            continue;
        }
        let example = match brace_alternatives(pattern) {
            Some(alts) => format!(
                ", e.g. {}",
                alts.iter()
                    .map(|a| format!("\"{a}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            None => String::new(),
        };
        bail!(
            "{what} entry \"{pattern}\" uses `{{` or `}}`, but Harbour does not expand \
             braces (it would only match a file literally named that)\n\
             hint: list the alternatives as separate entries{example}"
        );
    }
    Ok(())
}

/// Expand a pattern with exactly one brace group into its alternatives, for the
/// hint only. `None` for anything more involved: the hint then stays generic.
fn brace_alternatives(pattern: &str) -> Option<Vec<String>> {
    let open = pattern.find('{')?;
    let close = open + pattern[open..].find('}')?;
    let (head, body, tail) = (
        &pattern[..open],
        &pattern[open + 1..close],
        &pattern[close + 1..],
    );
    if body.contains(['{', '}']) || tail.contains(['{', '}']) || !body.contains(',') {
        return None;
    }
    Some(
        body.split(',')
            .map(|alt| format!("{head}{alt}{tail}"))
            .collect(),
    )
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

/// Strip Windows' `\\?\` verbatim prefix when the path is usable without it.
///
/// `fs::canonicalize` returns a verbatim path on Windows, and that prefix
/// leaks into places that cannot accept it: MSVC's `cl` rejects a source path
/// spelled `\\?\C:\...`, and it would also end up in generated files such as
/// `compile_commands.json`.
///
/// The prefix is genuinely *required* for UNC paths and for paths at or beyond
/// the legacy `MAX_PATH` limit, so it is only removed when neither applies.
/// On Unix the prefix never appears and this is a no-op, which is also why the
/// rule is expressed on strings and can be tested on any platform.
fn strip_verbatim_prefix(path: &Path) -> PathBuf {
    const VERBATIM: &str = r"\\?\";
    const LEGACY_MAX_PATH: usize = 260;

    let Some(s) = path.to_str() else {
        return path.to_path_buf();
    };
    let Some(rest) = s.strip_prefix(VERBATIM) else {
        return path.to_path_buf();
    };
    // A verbatim UNC path cannot drop the prefix without changing meaning.
    if rest.starts_with("UNC\\") || rest.len() >= LEGACY_MAX_PATH {
        return path.to_path_buf();
    }
    PathBuf::from(rest)
}

/// Canonicalize a path, but don't fail if it doesn't exist yet.
/// Returns the path as-is if canonicalization fails.
///
/// The result never carries Windows' verbatim prefix unless that prefix is
/// required -- see [`strip_verbatim_prefix`]. Callers feed these paths to
/// compilers, so a verbatim path here becomes a build failure there.
pub fn normalize_path(path: &Path) -> PathBuf {
    match path.canonicalize() {
        Ok(canonical) => strip_verbatim_prefix(&canonical),
        Err(_) => path.to_path_buf(),
    }
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
    fn test_strip_verbatim_prefix_removes_a_plain_drive_prefix() {
        // What broke the Windows build: cl cannot compile a source spelled
        // with the verbatim prefix.
        assert_eq!(
            super::strip_verbatim_prefix(Path::new(r"\\?\C:\Users\x\src\main.c")),
            PathBuf::from(r"C:\Users\x\src\main.c")
        );
    }

    #[test]
    fn test_strip_verbatim_prefix_keeps_unc_paths() {
        // A verbatim UNC path changes meaning if the prefix is dropped.
        let unc = Path::new(r"\\?\UNC\server\share\file");
        assert_eq!(super::strip_verbatim_prefix(unc), unc.to_path_buf());
    }

    #[test]
    fn test_strip_verbatim_prefix_keeps_long_paths() {
        // At or beyond legacy MAX_PATH the prefix is what makes the path work.
        let long = format!(r"\\?\C:\{}", "a".repeat(300));
        let p = Path::new(&long);
        assert_eq!(super::strip_verbatim_prefix(p), p.to_path_buf());
    }

    #[test]
    fn test_strip_verbatim_prefix_leaves_ordinary_paths_alone() {
        for p in ["/tmp/x", r"C:\Users\x", "relative/path"] {
            assert_eq!(super::strip_verbatim_prefix(Path::new(p)), PathBuf::from(p));
        }
    }

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
