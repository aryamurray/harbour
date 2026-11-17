//! Downloading and unpacking the archives a registry shim points at.
//!
//! Two kinds of shim arrive here: a `[source.tarball]` (one source tree for
//! every platform) and a `[[source.archive]]` (one prebuilt binary archive per
//! platform, which for libraries like libtorch is hundreds of megabytes to a
//! couple of gigabytes). Both are fetched the same way:
//!
//! - **Streamed, never held in memory.** A remote archive is written to a file
//!   beside its destination while its SHA-256 is computed from the same bytes,
//!   so it is hashed once, on the way in. A `file://` archive is hashed by
//!   reading it once and is then unpacked from where it is.
//! - **Bounded.** A download stops at [`MAX_ARCHIVE_BYTES`], and unpacking at
//!   [`MAX_UNPACKED_BYTES`], so a mistaken URL or a hostile archive runs out of
//!   allowance before the disk does. Both are far above any real package.
//! - **Contained.** Every entry of every format goes through the same check
//!   as a tarball's (`ensure_contained`): no `..`, no absolute path, no
//!   symlink pointing outside the destination.
//!
//! Formats: `.tar.gz`/`.tgz`, `.tar.xz`/`.txz`, `.tar.bz2`/`.tbz2`, plain
//! `.tar`, and `.zip`. The decoders are all Rust (`flate2`'s default backend,
//! `lzma-rust2`, `bzip2`'s `libbz2-rs-sys` backend, `zip` with deflate only),
//! so none of them makes a C compiler or `cmake` a requirement for building
//! Harbour itself.

use std::fs::File;
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use url::Url;

use super::ensure_contained;

/// The largest archive Harbour will download: 16 GiB.
///
/// A sanity limit, not a quota. The largest archives this exists for
/// (libtorch with CUDA) are around 2.5 GB.
pub const MAX_ARCHIVE_BYTES: u64 = 16 * 1024 * 1024 * 1024;

/// The most an archive may unpack to: 64 GiB.
pub const MAX_UNPACKED_BYTES: u64 = 64 * 1024 * 1024 * 1024;

/// An archive format Harbour can unpack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveFormat {
    TarGz,
    TarXz,
    TarBz2,
    Tar,
    Zip,
}

impl ArchiveFormat {
    /// The names `format = "..."` accepts in a shim.
    pub const NAMES: [&'static str; 5] = ["zip", "tar.gz", "tar.xz", "tar.bz2", "tar"];

    /// Parse a declared `format`.
    pub fn parse(name: &str) -> Option<Self> {
        match name
            .trim()
            .trim_start_matches('.')
            .to_ascii_lowercase()
            .as_str()
        {
            "zip" => Some(Self::Zip),
            "tar.gz" | "tgz" => Some(Self::TarGz),
            "tar.xz" | "txz" => Some(Self::TarXz),
            "tar.bz2" | "tbz2" | "tbz" => Some(Self::TarBz2),
            "tar" => Some(Self::Tar),
            _ => None,
        }
    }

    /// The format a file name or URL ends in, ignoring any `?query` or
    /// `#fragment`. `None` when the name does not say.
    pub fn from_file_name(name: &str) -> Option<Self> {
        let name = name.split(['?', '#']).next().unwrap_or(name);
        let lower = name.to_ascii_lowercase();
        const SUFFIXES: [(&str, ArchiveFormat); 9] = [
            (".tar.gz", ArchiveFormat::TarGz),
            (".tgz", ArchiveFormat::TarGz),
            (".tar.xz", ArchiveFormat::TarXz),
            (".txz", ArchiveFormat::TarXz),
            (".tar.bz2", ArchiveFormat::TarBz2),
            (".tbz2", ArchiveFormat::TarBz2),
            (".tbz", ArchiveFormat::TarBz2),
            (".tar", ArchiveFormat::Tar),
            (".zip", ArchiveFormat::Zip),
        ];
        SUFFIXES
            .iter()
            .find(|(suffix, _)| lower.ends_with(suffix))
            .map(|(_, format)| *format)
    }
}

/// An archive on local disk, with the hash of its bytes.
pub struct FetchedArchive {
    path: PathBuf,
    /// Lowercase hex SHA-256 of the file.
    pub sha256: String,
    /// Its size in bytes.
    pub size: u64,
    /// Set for a download: the file is ours and is removed on drop.
    temporary: bool,
}

impl FetchedArchive {
    /// Where the archive is.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for FetchedArchive {
    fn drop(&mut self) {
        if self.temporary {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// A human-readable size: `812 KB`, `196.4 MB`, `2.31 GB`.
pub fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b < KB {
        format!("{bytes} B")
    } else if b < KB * KB {
        format!("{:.0} KB", b / KB)
    } else if b < KB * KB * KB {
        format!("{:.1} MB", b / (KB * KB))
    } else {
        format!("{:.2} GB", b / (KB * KB * KB))
    }
}

/// Copy `reader` to `writer` (if any), hashing what passes and refusing more
/// than `limit` bytes. `progress` is told the running total.
fn copy_hashing(
    mut reader: impl Read,
    mut writer: Option<&mut dyn Write>,
    limit: u64,
    what: &str,
    mut progress: impl FnMut(u64),
) -> Result<(String, u64)> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 256 * 1024];
    let mut total: u64 = 0;
    loop {
        let n = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e).with_context(|| format!("failed while reading {what}")),
        };
        total += n as u64;
        if total > limit {
            bail!(
                "{what} is larger than {}, the most Harbour will fetch as one archive",
                human_size(limit)
            );
        }
        hasher.update(&buffer[..n]);
        if let Some(writer) = writer.as_mut() {
            writer
                .write_all(&buffer[..n])
                .with_context(|| format!("failed to store {what}"))?;
        }
        progress(total);
    }
    Ok((hex::encode(hasher.finalize()), total))
}

/// Get the archive at `url` onto local disk and hash it, once.
///
/// `label` names what is being fetched (`libtorch 2.5.1`), for the line that
/// says so: a download of this size can take minutes, and a build that sits
/// silent for minutes looks hung. `staging_dir` is where a download is kept
/// until it is unpacked; it should be on the volume the archive is unpacked
/// to.
pub fn fetch_archive(url: &str, label: &str, staging_dir: &Path) -> Result<FetchedArchive> {
    if let Ok(parsed) = Url::parse(url) {
        if parsed.scheme() == "file" {
            let path = parsed
                .to_file_path()
                .map_err(|_| anyhow::anyhow!("invalid file URL for an archive: {url}"))?;
            let file = File::open(&path)
                .with_context(|| format!("failed to read archive {}", path.display()))?;
            let (sha256, size) = copy_hashing(
                file,
                None,
                MAX_ARCHIVE_BYTES,
                &path.display().to_string(),
                |_| {},
            )?;
            return Ok(FetchedArchive {
                path,
                sha256,
                size,
                temporary: false,
            });
        }
    }

    // No overall timeout: the default for a blocking request is 30 seconds
    // for the *whole* transfer, which no large archive survives. A peer that
    // has gone away is still noticed, by TCP keep-alive.
    let client = reqwest::blocking::Client::builder()
        .timeout(None)
        .connect_timeout(std::time::Duration::from_secs(30))
        .tcp_keepalive(std::time::Duration::from_secs(30))
        .build()
        .context("failed to set up the HTTP client")?;
    let response = client
        .get(url)
        .send()
        .with_context(|| format!("failed to download {url}"))?;
    if !response.status().is_success() {
        bail!("failed to download {url}: HTTP {}", response.status());
    }

    let expected = response.content_length();
    if let Some(length) = expected {
        if length > MAX_ARCHIVE_BYTES {
            bail!(
                "{url} is {}, larger than {}, the most Harbour will fetch as one archive",
                human_size(length),
                human_size(MAX_ARCHIVE_BYTES)
            );
        }
    }
    match expected {
        Some(length) => tracing::info!("Downloading {label} ({}) from {url}", human_size(length)),
        None => tracing::info!("Downloading {label} from {url}"),
    }

    std::fs::create_dir_all(staging_dir)
        .with_context(|| format!("failed to create {}", staging_dir.display()))?;
    let temp = tempfile::Builder::new()
        .prefix(".download-")
        .suffix(".partial")
        .tempfile_in(staging_dir)
        .with_context(|| format!("failed to create a file in {}", staging_dir.display()))?;
    let (mut file, path) = temp
        .keep()
        .context("failed to keep the downloaded archive")?;
    // From here the file is removed by `FetchedArchive`'s drop, including on
    // the error paths below.
    let mut fetched = FetchedArchive {
        path,
        sha256: String::new(),
        size: 0,
        temporary: true,
    };

    // Drawn only on a terminal; a log or CI run gets the line above instead.
    let bar = match expected {
        Some(length) => indicatif::ProgressBar::new(length),
        None => indicatif::ProgressBar::new_spinner(),
    };
    if let Ok(style) = indicatif::ProgressStyle::with_template(
        "  {bar:30} {bytes}/{total_bytes} ({bytes_per_sec}, {eta} left)",
    ) {
        if expected.is_some() {
            bar.set_style(style);
        }
    }

    let copied = copy_hashing(response, Some(&mut file), MAX_ARCHIVE_BYTES, url, |total| {
        bar.set_position(total)
    });
    bar.finish_and_clear();
    let (sha256, size) = copied?;
    file.flush()
        .context("failed to store the downloaded archive")?;
    drop(file);

    if let Some(length) = expected {
        if length != size {
            bail!(
                "download of {url} was cut short: got {} of {}",
                human_size(size),
                human_size(length)
            );
        }
    }
    fetched.sha256 = sha256;
    fetched.size = size;
    Ok(fetched)
}

/// Unpack the archive file at `path` into `dest`.
///
/// `strip_prefix` removes a leading directory from every entry, as upstream
/// release archives usually have one (`libtorch/`, `zlib-1.3.1/`).
pub fn extract_archive(
    path: &Path,
    format: ArchiveFormat,
    dest: &Path,
    strip_prefix: Option<&str>,
) -> Result<()> {
    let file =
        File::open(path).with_context(|| format!("failed to open archive {}", path.display()))?;
    match format {
        ArchiveFormat::Zip => extract_zip(file, dest, strip_prefix),
        ArchiveFormat::Tar => super::extract_tar(BufReader::new(file), dest, strip_prefix),
        ArchiveFormat::TarGz => super::extract_tar(
            flate2::read::GzDecoder::new(BufReader::new(file)),
            dest,
            strip_prefix,
        ),
        ArchiveFormat::TarXz => super::extract_tar(
            lzma_rust2::XzReader::new(BufReader::new(file), true),
            dest,
            strip_prefix,
        ),
        ArchiveFormat::TarBz2 => super::extract_tar(
            bzip2::read::MultiBzDecoder::new(BufReader::new(file)),
            dest,
            strip_prefix,
        ),
    }
}

/// Where the archive entry `name` goes under `dest`, or `None` when
/// `strip_prefix` leaves nothing of it (the prefix directory itself).
///
/// The same rules as a tarball entry: the name is checked before and after
/// the prefix is removed, because a rewrite is another chance to escape.
fn zip_entry_destination(
    name: &str,
    dest: &Path,
    strip_prefix: Option<&str>,
) -> Result<Option<PathBuf>> {
    // Zip names are `/`-separated by specification; archives written by
    // careless Windows tools use `\`.
    let normalized = name.replace('\\', "/");
    ensure_contained(Path::new(&normalized), "zip entry")?;

    let relative: &str = match strip_prefix {
        Some(prefix) => {
            let prefix = prefix.trim_end_matches('/');
            if normalized.trim_end_matches('/') == prefix {
                return Ok(None);
            }
            match normalized.strip_prefix(&format!("{prefix}/")) {
                Some(rest) => rest,
                // Outside the prefix: kept where it is, as for a tarball.
                None => normalized.as_str(),
            }
        }
        None => normalized.as_str(),
    };
    let relative = relative.trim_matches('/');
    if relative.is_empty() {
        return Ok(None);
    }
    let relative: PathBuf = relative.split('/').filter(|c| !c.is_empty()).collect();
    ensure_contained(&relative, "zip entry")?;
    Ok(Some(dest.join(relative)))
}

/// Refuse a path that an already-extracted symlink leads out of `dest`.
///
/// A lexical check cannot see this one: the entry's own name is innocent and
/// a directory on the way to it is a link. Symlink targets are themselves
/// checked when they are created, so this is defence in depth.
fn ensure_resolves_inside(path: &Path, dest: &Path, name: &str) -> Result<()> {
    let canonical_dest = dest.canonicalize().unwrap_or_else(|_| dest.to_path_buf());
    if let Ok(canonical) = path.canonicalize() {
        if !canonical.starts_with(&canonical_dest) {
            bail!("zip entry escapes destination directory: {name}");
        }
    }
    Ok(())
}

/// Unpack a zip archive into `dest`.
pub fn extract_zip<R: Read + std::io::Seek>(
    reader: R,
    dest: &Path,
    strip_prefix: Option<&str>,
) -> Result<()> {
    let mut archive = zip::ZipArchive::new(reader).context("failed to read zip archive")?;

    std::fs::create_dir_all(dest)
        .with_context(|| format!("failed to create destination directory: {}", dest.display()))?;

    let mut unpacked: u64 = 0;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .context("failed to read zip entry")?;
        let name = entry
            .name()
            .map(|n| n.into_owned())
            .unwrap_or_else(|_| String::from_utf8_lossy(entry.name_raw()).into_owned());

        let Some(output_path) = zip_entry_destination(&name, dest, strip_prefix)? else {
            continue;
        };

        if entry.is_dir() {
            std::fs::create_dir_all(&output_path).with_context(|| {
                format!("failed to create directory: {}", output_path.display())
            })?;
            ensure_resolves_inside(&output_path, dest, &name)?;
            continue;
        }

        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create directory: {}", parent.display()))?;
            ensure_resolves_inside(parent, dest, &name)?;
        }

        if entry.is_symlink() {
            // The link's target is the entry's content. It gets the check an
            // entry name gets, and for a sharper reason: the link itself is
            // created inside the destination, so only the target can escape,
            // and anything later written through the link lands wherever it
            // points.
            let mut target = String::new();
            entry
                .by_ref()
                .take(4096)
                .read_to_string(&mut target)
                .with_context(|| format!("failed to read symlink target of {name}"))?;
            ensure_contained(Path::new(&target), "zip symlink target")?;

            #[cfg(unix)]
            {
                let _ = std::fs::remove_file(&output_path);
                std::os::unix::fs::symlink(&target, &output_path).with_context(|| {
                    format!("failed to create symlink: {}", output_path.display())
                })?;
            }
            #[cfg(not(unix))]
            {
                tracing::debug!("Skipping symlink on this platform: {name}");
            }
            continue;
        }

        // A regular file. If something is already there -- a symlink from an
        // earlier entry, most pointedly -- it is replaced, never written
        // through.
        if output_path.symlink_metadata().is_ok() {
            ensure_resolves_inside(&output_path, dest, &name)?;
            let _ = std::fs::remove_file(&output_path);
        }
        let mut out = File::create(&output_path)
            .with_context(|| format!("failed to extract file: {}", output_path.display()))?;
        let allowance = MAX_UNPACKED_BYTES - unpacked;
        let written = std::io::copy(&mut entry.by_ref().take(allowance + 1), &mut out)
            .with_context(|| format!("failed to extract file: {}", output_path.display()))?;
        unpacked += written;
        if unpacked > MAX_UNPACKED_BYTES {
            bail!(
                "zip archive unpacks to more than {}; refusing to continue",
                human_size(MAX_UNPACKED_BYTES)
            );
        }
        drop(out);

        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            // Permission bits only: no setuid, setgid or sticky from an archive.
            let _ = std::fs::set_permissions(
                &output_path,
                std::fs::Permissions::from_mode(mode & 0o777),
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use zip::write::SimpleFileOptions;

    fn zip_of(build: impl FnOnce(&mut zip::ZipWriter<Cursor<Vec<u8>>>)) -> Cursor<Vec<u8>> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        build(&mut writer);
        let mut cursor = writer.finish().unwrap();
        cursor.set_position(0);
        cursor
    }

    fn stored() -> SimpleFileOptions {
        SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored)
    }

    #[test]
    fn formats_are_read_from_names_and_urls() {
        for (name, format) in [
            ("libtorch-shared-2.5.1%2Bcpu.zip", ArchiveFormat::Zip),
            ("https://x.test/a/zlib-1.3.1.tar.gz", ArchiveFormat::TarGz),
            ("mbedtls-4.0.0.tar.bz2", ArchiveFormat::TarBz2),
            ("harfbuzz-11.0.0.tar.xz", ArchiveFormat::TarXz),
            ("x.TGZ", ArchiveFormat::TarGz),
            (
                "https://x.test/dl/pkg.zip?token=abc#frag",
                ArchiveFormat::Zip,
            ),
            ("plain.tar", ArchiveFormat::Tar),
        ] {
            assert_eq!(ArchiveFormat::from_file_name(name), Some(format), "{name}");
        }
        assert_eq!(
            ArchiveFormat::from_file_name("https://x.test/download"),
            None
        );
        assert_eq!(ArchiveFormat::parse("tar.xz"), Some(ArchiveFormat::TarXz));
        assert_eq!(ArchiveFormat::parse(".zip"), Some(ArchiveFormat::Zip));
        assert_eq!(ArchiveFormat::parse("rar"), None);
        for name in ArchiveFormat::NAMES {
            assert!(ArchiveFormat::parse(name).is_some(), "{name}");
        }
    }

    #[test]
    fn a_zip_is_unpacked_with_its_prefix_removed() {
        let zip = zip_of(|w| {
            w.add_directory("pkg-1.0/", stored()).unwrap();
            w.start_file("pkg-1.0/include/pkg.h", stored()).unwrap();
            w.write_all(b"int pkg(void);\n").unwrap();
            w.start_file(
                "pkg-1.0/lib/libpkg.so",
                SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated)
                    .unix_permissions(0o755),
            )
            .unwrap();
            w.write_all(&[7u8; 4096]).unwrap();
        });
        let tmp = tempfile::TempDir::new().unwrap();
        let dest = tmp.path().join("out");
        extract_zip(zip, &dest, Some("pkg-1.0")).unwrap();

        assert_eq!(
            std::fs::read_to_string(dest.join("include/pkg.h")).unwrap(),
            "int pkg(void);\n"
        );
        assert_eq!(
            std::fs::read(dest.join("lib/libpkg.so")).unwrap().len(),
            4096
        );
        assert!(!dest.join("pkg-1.0").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dest.join("lib/libpkg.so"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(
                mode & 0o777,
                0o755,
                "the archive's permission bits are kept"
            );
        }
    }

    /// The zip twin of `symlink_escape_is_rejected`/`path_traversal_...` for
    /// tarballs: an entry may not name a path outside the destination.
    #[test]
    fn a_zip_entry_may_not_leave_the_destination() {
        for evil in ["../escaped.txt", "a/../../escaped.txt", "/abs/escaped.txt"] {
            let zip = zip_of(|w| {
                w.start_file(evil, stored()).unwrap();
                w.write_all(b"pwned").unwrap();
            });
            let tmp = tempfile::TempDir::new().unwrap();
            let dest = tmp.path().join("inner").join("dest");
            let err = extract_zip(zip, &dest, None).unwrap_err();
            let text = format!("{err:#}");
            assert!(
                text.contains("..") || text.contains("absolute"),
                "{evil}: {text}"
            );
            assert!(!tmp.path().join("escaped.txt").exists(), "{evil}");
            assert!(!tmp.path().join("inner/escaped.txt").exists(), "{evil}");
        }
    }

    /// A backslash is a path separator to the tools that write such names, so
    /// it is one here: `..\x` is `../x`.
    #[test]
    fn a_backslash_traversal_is_refused_too() {
        let zip = zip_of(|w| {
            w.start_file("..\\escaped.txt", stored()).unwrap();
            w.write_all(b"pwned").unwrap();
        });
        let tmp = tempfile::TempDir::new().unwrap();
        let dest = tmp.path().join("dest");
        assert!(extract_zip(zip, &dest, None).is_err());
        assert!(!tmp.path().join("escaped.txt").exists());
    }

    /// Stripping a prefix rewrites the name, and the rewritten name is checked
    /// again.
    #[test]
    fn a_prefix_does_not_launder_a_traversal() {
        let zip = zip_of(|w| {
            w.start_file("top/../../escaped.txt", stored()).unwrap();
            w.write_all(b"pwned").unwrap();
        });
        let tmp = tempfile::TempDir::new().unwrap();
        let dest = tmp.path().join("a").join("dest");
        assert!(extract_zip(zip, &dest, Some("top")).is_err());
        assert!(!tmp.path().join("escaped.txt").exists());
    }

    /// A symlink whose target is outside the destination is refused, and so
    /// nothing can be written through it.
    #[test]
    fn a_zip_symlink_may_not_point_outside() {
        for target in ["../../outside", "/etc"] {
            let zip = zip_of(|w| {
                w.add_symlink("link", target, stored()).unwrap();
                w.start_file("link/escaped.txt", stored()).unwrap();
                w.write_all(b"pwned").unwrap();
            });
            let tmp = tempfile::TempDir::new().unwrap();
            std::fs::create_dir_all(tmp.path().join("outside")).unwrap();
            let dest = tmp.path().join("a").join("dest");
            let err = extract_zip(zip, &dest, None).unwrap_err();
            assert!(
                format!("{err:#}").contains("symlink target"),
                "{target}: {err:#}"
            );
            assert!(!tmp.path().join("outside/escaped.txt").exists());
        }
    }

    /// A contained symlink is what a versioned shared library is
    /// (`libx.so -> libx.so.1`), and is kept.
    #[cfg(unix)]
    #[test]
    fn a_contained_zip_symlink_is_created() {
        let zip = zip_of(|w| {
            w.start_file("lib/libx.so.1", stored()).unwrap();
            w.write_all(b"elf").unwrap();
            w.add_symlink("lib/libx.so", "libx.so.1", stored()).unwrap();
        });
        let tmp = tempfile::TempDir::new().unwrap();
        let dest = tmp.path().join("dest");
        extract_zip(zip, &dest, None).unwrap();
        assert_eq!(
            std::fs::read_link(dest.join("lib/libx.so")).unwrap(),
            PathBuf::from("libx.so.1")
        );
        assert_eq!(std::fs::read(dest.join("lib/libx.so")).unwrap(), b"elf");
    }

    #[test]
    fn tar_xz_and_tar_bz2_are_unpacked() {
        fn tar_bytes() -> Vec<u8> {
            let mut builder = tar::Builder::new(Vec::new());
            let content = b"int answer(void);\n";
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, "pkg-1.0/include/answer.h", &content[..])
                .unwrap();
            builder.into_inner().unwrap()
        }
        let tmp = tempfile::TempDir::new().unwrap();

        let xz_path = tmp.path().join("pkg.tar.xz");
        {
            let mut writer = lzma_rust2::XzWriter::new(
                File::create(&xz_path).unwrap(),
                lzma_rust2::XzOptions::default(),
            )
            .unwrap();
            writer.write_all(&tar_bytes()).unwrap();
            writer.finish().unwrap();
        }
        let bz_path = tmp.path().join("pkg.tar.bz2");
        {
            let mut writer = bzip2::write::BzEncoder::new(
                File::create(&bz_path).unwrap(),
                bzip2::Compression::default(),
            );
            writer.write_all(&tar_bytes()).unwrap();
            writer.finish().unwrap();
        }

        for (path, format) in [
            (&xz_path, ArchiveFormat::TarXz),
            (&bz_path, ArchiveFormat::TarBz2),
        ] {
            assert_eq!(
                ArchiveFormat::from_file_name(&path.display().to_string()),
                Some(format)
            );
            let dest = tmp.path().join(format!("out-{format:?}"));
            extract_archive(path, format, &dest, Some("pkg-1.0")).unwrap();
            assert_eq!(
                std::fs::read_to_string(dest.join("include/answer.h")).unwrap(),
                "int answer(void);\n",
                "{format:?}"
            );
        }
    }

    #[test]
    fn a_local_archive_is_hashed_where_it_is() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("a.zip");
        std::fs::write(&path, b"not really a zip").unwrap();
        let url = Url::from_file_path(&path).unwrap().to_string();

        let fetched = fetch_archive(&url, "a 1.0.0", &tmp.path().join("staging")).unwrap();
        assert_eq!(
            fetched.sha256,
            crate::util::hash::sha256_bytes(b"not really a zip")
        );
        assert_eq!(fetched.size, 16);
        assert_eq!(fetched.path(), path);
        drop(fetched);
        assert!(path.exists(), "a local archive is not ours to delete");
        assert!(!tmp.path().join("staging").exists(), "nothing is copied");
    }

    #[test]
    fn the_copy_stops_at_its_limit() {
        let data = vec![0u8; 1024];
        let err = copy_hashing(&data[..], None, 512, "the archive", |_| {}).unwrap_err();
        assert!(err.to_string().contains("larger than"), "{err}");
        let (hash, n) = copy_hashing(&data[..], None, 1024, "the archive", |_| {}).unwrap();
        assert_eq!(n, 1024);
        assert_eq!(hash, crate::util::hash::sha256_bytes(&data));
    }

    #[test]
    fn sizes_read_naturally() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(200 * 1024 * 1024), "200.0 MB");
        assert_eq!(human_size(5 * 1024 * 1024 * 1024 / 2), "2.50 GB");
    }
}
