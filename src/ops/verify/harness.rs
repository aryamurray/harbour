//! The harness: what verification links against the package, and runs.
//!
//! The harness is the consumer project's one executable, built by the same
//! `harbour build` as everything else. That is the point: the include paths,
//! defines, link order and system libraries it sees are whatever a real
//! consumer of the package would see, because the surface resolver supplies
//! them -- there is no second place that works them out.
//!
//! Two levels:
//!
//! - **declared**: the shim's `[metadata.harness]` (`header`, `test_call`,
//!   `lang`) is compiled, linked and run.
//! - **default**: when the shim declares none. Every public header that
//!   compiles on its own is included (one translation unit each), and the
//!   program references a symbol the library exports, so the link has to pull
//!   the archive in. This catches public headers a consumer could not compile
//!   and a library that exports nothing; it does not exercise any function.
//!
//! The default is deliberately conservative about *which* headers it holds the
//! package to. Public headers are not always includable alone (order
//! dependencies, platform-specific headers, headers that need an optional
//! dependency), and failing a working package over one of those would make the
//! gate untrustworthy in the other direction. A header that does not compile
//! alone is a warning carrying the compiler's first error; only "none of them
//! do" fails.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use super::build::HarnessSource;
use super::types::RejectedHeader;
use crate::core::target::Target;
use crate::sources::registry::shim::HarnessConfig;

/// Most public headers the default harness will try. A package with more has
/// its first ones checked and says so.
const MAX_HEADERS: usize = 256;

/// Prefix of the file name each header's translation unit gets. The number is
/// the header's index in the *original* candidate list, so removing a header
/// that does not compile leaves every other file name (and so its cached
/// object) alone.
const HEADER_UNIT_PREFIX: &str = "verify_header_";

/// Directory names that are never public headers.
const NON_PUBLIC_DIRS: &[&str] = &[
    "internal",
    "private",
    "detail",
    "impl",
    "test",
    "tests",
    "example",
    "examples",
    "contrib",
    "third_party",
    "thirdparty",
    "deps",
    "vendor",
    "doc",
    "docs",
    "bench",
    "benchmark",
    "fuzz",
    "tools",
];

/// A public header, as a consumer would include it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Header {
    /// The path after `#include <`, `/`-separated.
    pub include: String,
}

/// Verify that artifacts exist and are not empty.
pub(crate) fn verify_artifacts(artifacts: &[PathBuf]) -> Result<()> {
    for artifact in artifacts {
        if !artifact.exists() {
            bail!("artifact does not exist: {}", artifact.display());
        }

        let metadata = std::fs::metadata(artifact)?;
        if metadata.len() == 0 {
            bail!("artifact is empty: {}", artifact.display());
        }
    }

    Ok(())
}

/// The sources of the declared harness. The language is the shim's `lang`.
pub(crate) fn declared_sources(config: &HarnessConfig) -> Result<(bool, Vec<HarnessSource>)> {
    let cxx = config.lang == "cxx" || config.lang == "c++";
    let dir = tempfile::tempdir().context("failed to create a scratch directory")?;
    let path = generate_harness(config, dir.path())?;
    let content = std::fs::read_to_string(&path)?;
    let file = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "harness_test.c".to_string());
    Ok((cxx, vec![HarnessSource { file, content }]))
}

/// The placeholder program the build stage links: it proves the package builds
/// and a consumer links, before any harness is held against it.
pub(crate) fn placeholder_source(cxx: bool) -> HarnessSource {
    HarnessSource {
        file: if cxx {
            "verify_main.cpp"
        } else {
            "verify_main.c"
        }
        .to_string(),
        content: "int main(void) { return 0; }\n".to_string(),
    }
}

/// Whether a target is written in C++: its declared language, or C++ source
/// patterns on a target that left the language at its default.
pub(crate) fn is_cxx(target: &Target) -> bool {
    target.lang == crate::core::target::Language::Cxx
        || target.sources.iter().any(|s| {
            let s = s.to_ascii_lowercase();
            [".cc", ".cpp", ".cxx", ".c++"]
                .iter()
                .any(|e| s.ends_with(e))
        })
}

/// The public headers to hold the package to.
///
/// `public_headers` where the library declares them; otherwise every header in
/// the public include directories, minus the directories that are never public
/// (see [`NON_PUBLIC_DIRS`]). When the include directory is the package root --
/// zlib's layout -- only its top level is read, since the tree below it is
/// tests and contributions, not an interface.
///
/// `include_dirs` are the directories the *consumer* compiles with, so a header
/// is named the way a consumer would write it.
pub(crate) fn discover_headers(
    package_name: &str,
    package_root: &Path,
    closure: &[&Target],
    include_dirs: &[PathBuf],
    cxx: bool,
) -> Result<(Vec<Header>, Vec<RejectedHeader>)> {
    let root = crate::util::fs::normalize_path(package_root);
    let dirs: Vec<PathBuf> = include_dirs
        .iter()
        .map(|d| crate::util::fs::normalize_path(d))
        .filter(|d| d.starts_with(&root))
        .collect();

    let mut files: Vec<PathBuf> = Vec::new();
    let declared: Vec<String> = closure
        .iter()
        .flat_map(|t| t.public_headers.iter().cloned())
        .collect();
    if !declared.is_empty() {
        files = crate::util::fs::glob_files(&root, &declared)
            .context("failed to expand `public_headers`")?;
    } else {
        for dir in &dirs {
            let depth = if *dir == root { 0 } else { 3 };
            collect_headers(dir, depth, cxx, &mut files);
        }

        // With the package root itself on the include path (zlib, libpng) the
        // public headers sit among the private ones and nothing says which is
        // which. The header named for the package is the entry point by
        // convention (`zlib.h`, `png.h` for `libpng`); the rest are tried only
        // if there is no such header, rather than reported as failures of a
        // package that never offered them.
        if dirs.contains(&root) {
            let stem = package_name.strip_prefix("lib").unwrap_or(package_name);
            let named: Vec<PathBuf> = files
                .iter()
                .filter(|f| {
                    f.parent() == Some(root.as_path())
                        && f.file_stem().is_some_and(|s| {
                            s.eq_ignore_ascii_case(package_name) || s.eq_ignore_ascii_case(stem)
                        })
                })
                .cloned()
                .collect();
            if !named.is_empty() {
                files.retain(|f| f.parent() != Some(root.as_path()) || named.contains(f));
            }
        }
    }

    let mut headers: Vec<Header> = Vec::new();
    let mut rejected: Vec<RejectedHeader> = Vec::new();
    for file in files {
        let file = crate::util::fs::normalize_path(&file);
        // The deepest include directory that holds it: `generated/table.h` is
        // `<table.h>` to a consumer that has `generated` on its path, whatever
        // else is on it.
        let Some(relative) = dirs
            .iter()
            .filter_map(|d| file.strip_prefix(d).ok())
            .min_by_key(|r| r.components().count())
        else {
            rejected.push(RejectedHeader {
                header: file
                    .strip_prefix(&root)
                    .unwrap_or(&file)
                    .to_string_lossy()
                    .replace('\\', "/"),
                kind: "compile",
                error: "declared public, but it is under none of the package's public include \
                        directories, so a consumer cannot include it"
                    .to_string(),
            });
            continue;
        };
        let include = relative
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        let header = Header { include };
        if !headers.contains(&header) {
            headers.push(header);
        }
    }
    headers.sort_by(|a, b| a.include.cmp(&b.include));

    // Headers that refuse to be included alone, on purpose. GLib, GTK and
    // HarfBuzz all guard their sub-headers with `#error "Include <x.h>
    // instead."`; trying them would only record the guard firing.
    let mut entry_points = Vec::new();
    for header in headers {
        let umbrella = dirs
            .iter()
            .map(|d| d.join(&header.include))
            .find(|p| p.is_file())
            .and_then(|p| umbrella_guard(&p));
        match umbrella {
            Some(message) => rejected.push(RejectedHeader {
                header: header.include,
                kind: "umbrella",
                error: message,
            }),
            None => entry_points.push(header),
        }
    }
    let mut headers = entry_points;

    if headers.len() > MAX_HEADERS {
        rejected.push(RejectedHeader {
            header: format!("{} more headers", headers.len() - MAX_HEADERS),
            kind: "compile",
            error: format!("not checked: the default harness tries at most {MAX_HEADERS}"),
        });
        headers.truncate(MAX_HEADERS);
    }
    Ok((headers, rejected))
}

fn collect_headers(dir: &Path, depth: usize, cxx: bool, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        if path.is_dir() {
            if depth > 0 && !NON_PUBLIC_DIRS.contains(&name.as_str()) && !name.starts_with('.') {
                collect_headers(&path, depth - 1, cxx, out);
            }
        } else if name.ends_with(".h")
            || (cxx && (name.ends_with(".hpp") || name.ends_with(".hxx")))
        {
            out.push(path);
        }
    }
}

/// The `#error` that says this header is not an entry point, if it has one.
fn umbrella_guard(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    text.lines()
        .map(str::trim)
        .filter(|l| l.starts_with('#'))
        .map(|l| l.trim_start_matches('#').trim_start())
        .filter_map(|l| l.strip_prefix("error"))
        .map(str::trim)
        .find(|l| {
            let l = l.to_ascii_lowercase();
            l.contains("instead") || l.contains("included directly") || l.contains("only <")
        })
        .map(|l| format!("#error {l}"))
}

/// The source file for the translation unit that includes `header`, number `n`.
pub(crate) fn header_unit(n: usize, header: &Header, cxx: bool) -> HarnessSource {
    HarnessSource {
        file: format!(
            "{HEADER_UNIT_PREFIX}{n:04}.{}",
            if cxx { "cpp" } else { "c" }
        ),
        // A typedef, so the unit is not empty (an empty translation unit is a
        // pedantic warning) and defines no symbol for two units to collide on.
        content: format!(
            "#include <{}>\ntypedef int harbour_verify_unit_{n};\n",
            header.include
        ),
    }
}

/// Which header a compile failure belongs to, if it is a header unit's.
///
/// The build reports a failed compile as `compilation failed for <path>`
/// followed by the compiler's own output; the unit's index is in the file name.
pub(crate) fn failed_header_unit(error: &str) -> Option<usize> {
    let line = error.lines().find_map(|l| {
        l.split_once("compilation failed for ")
            .map(|(_, p)| p.trim())
    })?;
    let name = Path::new(line).file_name()?.to_string_lossy().into_owned();
    let rest = name.strip_prefix(HEADER_UNIT_PREFIX)?;
    rest.split('.').next()?.parse().ok()
}

/// The first thing the compiler said was wrong, from a failed compile's text.
pub(crate) fn first_compiler_error(error: &str, roots: &[&Path]) -> String {
    let body = error
        .split_once("compilation failed for ")
        .and_then(|(_, rest)| rest.split_once('\n'))
        .map(|(_, body)| body)
        .unwrap_or(error);
    let line = body
        .lines()
        .find(|l| {
            let l = l.to_ascii_lowercase();
            l.contains("error") && !l.trim_start().starts_with("in file included")
        })
        .or_else(|| body.lines().find(|l| !l.trim().is_empty()))
        .unwrap_or("(the compiler printed nothing)");
    let mut line = line.trim().to_string();
    // Paths into the cache say nothing a reader needs and push the message
    // off the screen; what the compiler objected to is what follows them.
    for root in roots {
        let root = root.display().to_string();
        line = line
            .replace(&format!("{root}/"), "")
            .replace(&format!("{root}\\"), "");
    }
    let line = line.as_str();
    if line.chars().count() > 240 {
        let cut: String = line.chars().take(240).collect();
        format!("{cut}...")
    } else {
        line.to_string()
    }
}

/// The program that makes the link pull the library in: it stores the address
/// of one symbol the archive exports.
///
/// Without this a consumer that references nothing links against an empty
/// archive just as happily as against a full one.
pub(crate) fn main_source(symbol: Option<&str>, cxx: bool) -> HarnessSource {
    let file = if cxx {
        "verify_main.cpp"
    } else {
        "verify_main.c"
    };
    let content = match symbol {
        // Volatile and read back, so no optimizer can prove the reference away.
        Some(symbol) => format!(
            "#ifdef __cplusplus\nextern \"C\" {{\n#endif\nextern char {symbol};\n#ifdef __cplusplus\n}}\n#endif\n\
             static const void *volatile harbour_verify_ref = &{symbol};\n\
             int main(void) {{ return harbour_verify_ref == 0 ? 1 : 0; }}\n"
        ),
        None => "int main(void) { return 0; }\n".to_string(),
    };
    HarnessSource {
        file: file.to_string(),
        content,
    }
}

/// A symbol the archive at `path` exports that a harness can name, or `None`
/// if its symbol table cannot be read or holds nothing nameable.
///
/// `Err` is for an archive whose table is readable and *empty*: a library that
/// exports nothing.
pub(crate) fn linkable_symbol(path: &Path, apple: bool) -> Result<Option<String>> {
    let Some(symbols) = archive_symbols(path)? else {
        return Ok(None);
    };
    if symbols.is_empty() {
        bail!(
            "{} exports no symbols: the archive exists but a consumer has nothing to link\n\
             hint: check the target's `sources` match files in the fetched tree, and that \
             they define something",
            path.display()
        );
    }

    let nameable = |s: &str| -> bool {
        let mut chars = s.chars();
        matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
            && !s.starts_with("_Z")
    };
    // Mach-O prefixes every C symbol with an underscore; the source does not.
    let source_name = |s: &str| -> String {
        if apple {
            s.strip_prefix('_').unwrap_or(s).to_string()
        } else {
            s.to_string()
        }
    };
    let candidates: Vec<String> = symbols
        .iter()
        .map(|s| source_name(s))
        .filter(|s| nameable(s))
        .collect();
    // Prefer an ordinary name over a reserved-looking one (`__x`, `_x`).
    Ok(candidates
        .iter()
        .find(|s| !s.starts_with('_'))
        .or_else(|| candidates.first())
        .cloned())
}

/// Read the symbol table of a static archive: `Ok(None)` if the file is not an
/// archive or has no table to read (that is not a finding), `Ok(Some(names))`
/// otherwise.
///
/// GNU `ar` and MSVC's `lib` write the same first member (`/`: a big-endian
/// count, the member offsets, then the NUL-separated names); Apple's writes a
/// `__.SYMDEF` member of little-endian `ranlib` entries. Reading them here
/// keeps verification independent of whether `nm` or `dumpbin` is installed.
pub(crate) fn archive_symbols(path: &Path) -> Result<Option<Vec<String>>> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("failed to read archive {}", path.display()))?;
    Ok(parse_archive_symbols(&bytes))
}

fn parse_archive_symbols(bytes: &[u8]) -> Option<Vec<String>> {
    const MAGIC: &[u8] = b"!<arch>\n";
    const HEADER: usize = 60;
    if !bytes.starts_with(MAGIC) || bytes.len() < MAGIC.len() + HEADER {
        return None;
    }
    let header = &bytes[MAGIC.len()..MAGIC.len() + HEADER];
    let name = std::str::from_utf8(&header[..16]).ok()?.trim_end();
    let size: usize = std::str::from_utf8(&header[48..58])
        .ok()?
        .trim()
        .parse()
        .ok()?;
    let body_start = MAGIC.len() + HEADER;

    let be32 = |b: &[u8]| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize;
    let le32 = |b: &[u8]| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize;
    let cstr_list = |b: &[u8], count: usize| -> Vec<String> {
        b.split(|&c| c == 0)
            .take(count)
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect()
    };

    if name == "/" {
        let body = bytes.get(body_start..body_start.checked_add(size)?)?;
        let count = be32(body.get(..4)?);
        let names = body.get(4usize.checked_add(count.checked_mul(4)?)?..)?;
        return Some(cstr_list(names, count));
    }
    if name == "/SYM64/" {
        let body = bytes.get(body_start..body_start.checked_add(size)?)?;
        let count = u64::from_be_bytes(body.get(..8)?.try_into().ok()?) as usize;
        let names = body.get(8usize.checked_add(count.checked_mul(8)?)?..)?;
        return Some(cstr_list(names, count));
    }
    // BSD: `#1/<n>` puts an n-byte name (`__.SYMDEF...`) at the start of the body.
    if let Some(len) = name
        .strip_prefix("#1/")
        .and_then(|n| n.trim().parse::<usize>().ok())
    {
        let body = bytes.get(body_start..body_start.checked_add(size)?)?;
        let member_name = String::from_utf8_lossy(body.get(..len)?);
        if !member_name.starts_with("__.SYMDEF") {
            return None;
        }
        let table = body.get(len..)?;
        let ranlib_bytes = le32(table.get(..4)?);
        let entries = table.get(4..4usize.checked_add(ranlib_bytes)?)?;
        let strings_at = 4 + ranlib_bytes;
        let strings_len = le32(table.get(strings_at..strings_at + 4)?);
        let strings = table.get(strings_at + 4..strings_at + 4 + strings_len)?;
        let mut out = Vec::new();
        for entry in entries.as_chunks::<8>().0 {
            let at = le32(&entry[..4]);
            let rest = strings.get(at..)?;
            let end = rest.iter().position(|&c| c == 0).unwrap_or(rest.len());
            out.push(String::from_utf8_lossy(&rest[..end]).into_owned());
        }
        return Some(out);
    }
    None
}

/// How long the harness may run before it is stopped. A harness is a few
/// function calls; one that has not finished in this long is hung, and a hung
/// check must fail rather than hold up the merge gate.
const RUN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Run the harness executable in `cwd`, failing on a non-zero exit or a hang.
pub(crate) fn run_harness(exe: &Path, cwd: &Path) -> Result<()> {
    // Output goes to files rather than pipes, so a harness that prints more
    // than a pipe holds cannot block on a reader that is busy polling.
    let out_path = cwd.join("harness.stdout");
    let err_path = cwd.join("harness.stderr");
    let mut child = std::process::Command::new(exe)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::fs::File::create(&out_path)?)
        .stderr(std::fs::File::create(&err_path)?)
        .spawn()
        .with_context(|| format!("failed to execute the harness {}", exe.display()))?;

    let started = std::time::Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() > RUN_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            bail!(
                "the harness did not finish within {}s and was killed",
                RUN_TIMEOUT.as_secs()
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };

    if !status.success() {
        let stdout = std::fs::read_to_string(&out_path).unwrap_or_default();
        let stderr = std::fs::read_to_string(&err_path).unwrap_or_default();
        bail!(
            "harness execution failed ({status}):\n{}\n{}",
            stdout.trim_end(),
            stderr.trim_end()
        );
    }
    Ok(())
}

/// Generate a C test harness file.
pub fn generate_harness_c(config: &HarnessConfig, output_path: &Path) -> Result<()> {
    let content = format!(
        r#"/* Auto-generated harness test for Harbour */
#include <{header}>

int main(void) {{
    {test_call};
    return 0;
}}
"#,
        header = config.header,
        test_call = config.test_call
    );

    std::fs::write(output_path, content)
        .with_context(|| format!("failed to write harness file: {}", output_path.display()))?;

    Ok(())
}

/// Generate a C++ test harness file.
pub fn generate_harness_cxx(config: &HarnessConfig, output_path: &Path) -> Result<()> {
    let content = format!(
        r#"// Auto-generated harness test for Harbour
#include <{header}>

int main() {{
    {test_call};
    return 0;
}}
"#,
        header = config.header,
        test_call = config.test_call
    );

    std::fs::write(output_path, content)
        .with_context(|| format!("failed to write harness file: {}", output_path.display()))?;

    Ok(())
}

/// Generate the appropriate harness based on language.
pub fn generate_harness(config: &HarnessConfig, output_dir: &Path) -> Result<PathBuf> {
    // The fn-pointer type is inherently a bit noisy to clippy; introducing a `type` alias
    // here would only be used at this single call site, so it's not worth the indirection.
    #[allow(clippy::type_complexity)]
    let (filename, generator): (&str, fn(&HarnessConfig, &Path) -> Result<()>) =
        if config.lang == "cxx" || config.lang == "c++" {
            ("harness_test.cpp", generate_harness_cxx)
        } else {
            ("harness_test.c", generate_harness_c)
        };

    let output_path = output_dir.join(filename);
    generator(config, &output_path)?;
    Ok(output_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One archive member: a 60-byte header and the body, padded to even.
    fn member(name: &str, body: &[u8]) -> Vec<u8> {
        let mut out = format!(
            "{name:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
            0,
            0,
            0,
            0,
            body.len()
        )
        .into_bytes();
        out.extend_from_slice(body);
        if body.len() % 2 == 1 {
            out.push(b'\n');
        }
        out
    }

    #[test]
    fn reads_a_gnu_or_msvc_symbol_table() {
        let mut body = 2u32.to_be_bytes().to_vec();
        body.extend_from_slice(&0u32.to_be_bytes());
        body.extend_from_slice(&0u32.to_be_bytes());
        body.extend_from_slice(b"alpha\0beta\0");
        let mut archive = b"!<arch>\n".to_vec();
        archive.extend(member("/", &body));
        assert_eq!(
            parse_archive_symbols(&archive),
            Some(vec!["alpha".to_string(), "beta".to_string()])
        );
    }

    #[test]
    fn reads_an_apple_symbol_table() {
        // One ranlib entry: name offset 0, member offset 0.
        let mut table = 8u32.to_le_bytes().to_vec();
        table.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
        table.extend_from_slice(&6u32.to_le_bytes());
        table.extend_from_slice(b"_main\0");
        let name = format!("{:\0<20}", "__.SYMDEF");
        let mut body = name.into_bytes();
        body.extend(table);
        let mut archive = b"!<arch>\n".to_vec();
        archive.extend(member("#1/20", &body));
        assert_eq!(
            parse_archive_symbols(&archive),
            Some(vec!["_main".to_string()])
        );
    }

    #[test]
    fn an_empty_table_is_a_library_that_exports_nothing() {
        let mut archive = b"!<arch>\n".to_vec();
        archive.extend(member("/", &0u32.to_be_bytes()));
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("libempty.a");
        std::fs::write(&path, archive).unwrap();
        let err = linkable_symbol(&path, false).unwrap_err().to_string();
        assert!(err.contains("exports no symbols"), "{err}");
    }

    #[test]
    fn something_that_is_not_an_archive_is_not_judged() {
        assert_eq!(
            parse_archive_symbols(b"not an archive at all, nothing here"),
            None
        );
    }

    #[test]
    fn a_failed_compile_is_attributed_to_its_header_unit() {
        let text = "Build failed: compilation failed for /tmp/x/harness/verify_header_0012.c\n\
                    In file included from a.c:1:\n\
                    /tmp/x/pkg/lib.h:3:10: fatal error: nope.h: No such file or directory\n";
        assert_eq!(failed_header_unit(text), Some(12));
        assert_eq!(
            first_compiler_error(text, &[Path::new("/tmp/x/pkg")]),
            "lib.h:3:10: fatal error: nope.h: No such file or directory"
        );
        // A failure in the library, or a link failure, is nobody's header.
        assert_eq!(
            failed_header_unit("compilation failed for /cache/zlib/inflate.c\n"),
            None
        );
        assert_eq!(
            failed_header_unit("linking failed for /tmp/x/harness\n"),
            None
        );
    }

    #[test]
    fn a_header_that_says_to_include_another_is_not_an_entry_point() {
        let tmp = tempfile::tempdir().unwrap();
        let guarded = tmp.path().join("sub.h");
        std::fs::write(
            &guarded,
            "#ifndef HB_H_IN\n#error \"Include <hb.h> instead.\"\n#endif\n",
        )
        .unwrap();
        let plain = tmp.path().join("plain.h");
        std::fs::write(
            &plain,
            "#ifdef NOPE\n#error \"unsupported compiler\"\n#endif\n",
        )
        .unwrap();
        assert!(umbrella_guard(&guarded).unwrap().contains("hb.h"));
        assert!(umbrella_guard(&plain).is_none());
    }

    #[test]
    fn test_generate_harness_c() {
        let config = HarnessConfig {
            header: "zlib.h".to_string(),
            test_call: "zlibVersion()".to_string(),
            lang: "c".to_string(),
        };

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.c");
        generate_harness_c(&config, &path).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("#include <zlib.h>"));
        assert!(content.contains("zlibVersion()"));
    }

    #[test]
    fn test_generate_harness_cxx() {
        let config = HarnessConfig {
            header: "fmt/core.h".to_string(),
            test_call: "fmt::format(\"test\")".to_string(),
            lang: "cxx".to_string(),
        };

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.cpp");
        generate_harness_cxx(&config, &path).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("#include <fmt/core.h>"));
        assert!(content.contains("fmt::format"));
    }
}
