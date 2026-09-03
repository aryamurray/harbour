//! Tier-1 package index: one file per package, one line per version.
//!
//! # Why NDJSON
//!
//! Two tiers, two encodings, on purpose:
//!
//! - Tier 2 (the shim, `shim.rs`) is hand-authored and reviewed as a pull
//!   request, so TOML - a format optimized for a human writing and reading a
//!   single record - suits it.
//! - Tier 1 is machine-generated and append-only: publishing a version adds
//!   exactly one record to a file that may already hold hundreds of them.
//!   A line-oriented format (one JSON object per line, a la crates.io's own
//!   index and countless log formats) gives that for free:
//!     - A diff of "one version was published" is a one-line diff, not a
//!       reflow of a shared TOML table.
//!     - Reading doesn't require parsing the whole file up front - a client
//!       can stream line by line, which matters once index files serve
//!       hundreds of versions over HTTP.
//!     - Appending is `O(1)`: open for append, write one line, done. No
//!       existing content has to be re-parsed or re-rendered.
//!
//! A single TOML (or JSON) document per package would need the whole file
//! parsed and re-serialized for every publish, and produces a diff that can
//! touch unrelated lines depending on how the serializer orders keys.
//!
//! # Format version
//!
//! Every record carries `format_version`. It is intentionally per-record
//! (rather than a single header line) so a reader never has to special-case
//! line 1: any line can be validated independently, which matters for a
//! streaming HTTP reader that may fetch a byte range rather than the whole
//! file.

use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::shim::validate_package_name;

/// The only tier-1 index format understood by this version of Harbour.
///
/// Bump this whenever a breaking change is made to [`IndexRecord`]'s shape,
/// and teach [`parse_record`] to reject (or migrate) older/newer values
/// explicitly rather than silently misreading them.
pub const CURRENT_FORMAT_VERSION: u32 = 1;

/// A single version record from a package's tier-1 index file.
///
/// Carries exactly what dependency resolution needs and nothing more - see
/// the module docs for why this is split from the tier-2 shim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexRecord {
    /// Format version this record was written under.
    pub format_version: u32,

    /// Package name.
    pub name: String,

    /// Exact version this record describes.
    pub version: String,

    /// Whether this version has been yanked.
    ///
    /// Yanked versions are excluded from new resolutions (see
    /// [`IndexRecord::is_available`]) but remain fetchable - the record and
    /// its tier-2 shim are never deleted, only flagged, so a lockfile that
    /// already pins a yanked version keeps working.
    #[serde(default)]
    pub yanked: bool,

    /// Dependencies needed to resolve this version.
    #[serde(default)]
    pub deps: Vec<IndexDependency>,

    /// sha256 checksum of the artifact (tarball bytes, or the git tree for
    /// sources that record one). `None` for git sources that omit it -
    /// see `shim.rs`'s module docs on why that is optional in v1.
    #[serde(default)]
    pub checksum: Option<String>,

    /// Path to the tier-2 shim, relative to the registry's `index/` root
    /// (e.g. `"z/zlib/1.3.1.toml"`). Fetched only once this version is
    /// actually selected.
    pub shim: String,
}

impl IndexRecord {
    /// Whether this version should be offered to a *new* resolution.
    ///
    /// Yanked versions are hidden here but remain loadable by exact
    /// version - see the `yanked` field's docs.
    pub fn is_available(&self) -> bool {
        !self.yanked
    }
}

/// A dependency requirement carried in the tier-1 index.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexDependency {
    /// Name of the depended-upon package.
    pub name: String,

    /// Version requirement, in the same syntax accepted in `Harbour.toml`.
    pub version_req: String,

    /// Whether the dependency is optional.
    #[serde(default)]
    pub optional: bool,

    /// Whether the dependency's default features are enabled.
    #[serde(default = "default_true")]
    pub default_features: bool,

    /// Dependency kind (normal/dev/build). Harbour only has "normal"
    /// dependencies today; the field exists so the format doesn't need a
    /// breaking change the day that changes.
    #[serde(default)]
    pub kind: IndexDependencyKind,

    /// Registry URL this dependency resolves against, if different from
    /// the registry this index belongs to. `None` means "same registry".
    #[serde(default)]
    pub registry: Option<String>,
}

fn default_true() -> bool {
    true
}

/// Dependency kind, mirroring Cargo's normal/dev/build split.
///
/// Harbour currently only has normal dependencies; `Dev` and `Build` are
/// reserved so adding them later doesn't require an index format bump.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IndexDependencyKind {
    #[default]
    Normal,
    Dev,
    Build,
}

/// Compute the tier-1 index path for a package, relative to the registry's
/// `index/` root: `<first-letter>/<name>.idx`.
///
/// This deliberately mirrors the tier-2 shim's sharding
/// (`<first-letter>/<name>/<version>.toml`, see `shim_path`) so both tiers
/// shard the same way, but a plain sibling file rather than a directory -
/// `z/zlib.idx` sits next to the `z/zlib/` directory that holds shims,
/// with no name collision between the two.
pub fn index_path(name: &str) -> Result<String> {
    validate_package_name(name)?;
    let first_char = name.chars().next().unwrap();
    Ok(format!("{first_char}/{name}.idx"))
}

/// Parse a tier-1 index file's raw bytes into its records.
///
/// `source` is used only to make error messages point at the file/URL the
/// bytes came from. Blank lines are ignored (so a trailing newline, or one
/// accidentally left in by an editor, is not a parse error). Every
/// non-blank line must be a single valid [`IndexRecord`] at
/// [`CURRENT_FORMAT_VERSION`]; the first invalid line aborts with its line
/// number.
pub fn parse_index(bytes: &[u8], source: &str) -> Result<Vec<IndexRecord>> {
    let text = std::str::from_utf8(bytes)
        .with_context(|| format!("index file is not valid UTF-8: {source}"))?;

    let mut records = Vec::new();
    for (line_no, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let record: IndexRecord = serde_json::from_str(line).with_context(|| {
            format!(
                "malformed tier-1 index record at {source}:{} - not a valid index record",
                line_no + 1
            )
        })?;

        if record.format_version != CURRENT_FORMAT_VERSION {
            bail!(
                "{source}:{}: unsupported tier-1 index format version {} \
                 (this build of harbour understands version {})",
                line_no + 1,
                record.format_version,
                CURRENT_FORMAT_VERSION
            );
        }

        records.push(record);
    }

    Ok(records)
}

/// Serialize a single record to its on-disk line form (no trailing
/// newline).
pub fn serialize_record(record: &IndexRecord) -> Result<String> {
    serde_json::to_string(record).context("failed to serialize tier-1 index record")
}

/// Append a single record to a package's tier-1 index file on disk,
/// creating the file (and its parent directories) if this is the first
/// version published for the package.
///
/// This is the operation a real publish step performs: one new line, no
/// rewriting of existing content. It is exposed here mainly so tests (and
/// any future publish tooling) don't have to hand-roll NDJSON.
pub fn append_record(index_file: &Path, record: &IndexRecord) -> Result<()> {
    use std::io::Write;

    if let Some(parent) = index_file.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create index directory {}", parent.display()))?;
    }

    let line = serialize_record(record)?;

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(index_file)
        .with_context(|| format!("failed to open index file {}", index_file.display()))?;

    writeln!(file, "{line}").with_context(|| {
        format!(
            "failed to append record to index file {}",
            index_file.display()
        )
    })?;

    Ok(())
}

/// Write a package's entire tier-1 index file from scratch, overwriting
/// whatever was there before.
///
/// Used by [`super::generate::generate_index`] to (re)build the whole tree
/// deterministically, e.g. for CI to diff against what is committed. Real
/// publishing appends a single record instead - see [`append_record`].
pub fn write_index_file(index_file: &Path, records: &[IndexRecord]) -> Result<()> {
    if let Some(parent) = index_file.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create index directory {}", parent.display()))?;
    }

    let mut contents = String::new();
    for record in records {
        contents.push_str(&serialize_record(record)?);
        contents.push('\n');
    }

    std::fs::write(index_file, contents)
        .with_context(|| format!("failed to write index file {}", index_file.display()))
}

/// Read and parse a package's tier-1 index file directly from disk.
///
/// Returns `Ok(None)` if the file does not exist (the package has no
/// published versions in this registry) rather than treating that as an
/// error - a missing tier-1 file is the ordinary "package not found" case,
/// not a corruption.
pub fn read_index_file(index_file: &Path) -> Result<Option<Vec<IndexRecord>>> {
    match std::fs::read(index_file) {
        Ok(bytes) => Ok(Some(parse_index(
            &bytes,
            &index_file.display().to_string(),
        )?)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => {
            Err(err).with_context(|| format!("failed to read index file {}", index_file.display()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_record() -> IndexRecord {
        IndexRecord {
            format_version: CURRENT_FORMAT_VERSION,
            name: "zlib".to_string(),
            version: "1.3.1".to_string(),
            yanked: false,
            deps: vec![IndexDependency {
                name: "liba".to_string(),
                version_req: "^0.1".to_string(),
                optional: false,
                default_features: true,
                kind: IndexDependencyKind::Normal,
                registry: None,
            }],
            checksum: Some("a".repeat(64)),
            shim: "z/zlib/1.3.1.toml".to_string(),
        }
    }

    #[test]
    fn index_path_shards_by_first_letter() {
        assert_eq!(index_path("zlib").unwrap(), "z/zlib.idx");
        assert_eq!(index_path("sqlite").unwrap(), "s/sqlite.idx");
    }

    #[test]
    fn round_trips_a_record() {
        let record = sample_record();
        let line = serialize_record(&record).unwrap();
        assert!(!line.contains('\n'), "a record must serialize to one line");

        let parsed = parse_index(line.as_bytes(), "test").unwrap();
        assert_eq!(parsed, vec![record]);
    }

    #[test]
    fn parses_multiple_lines_and_skips_blanks() {
        let a = sample_record();
        let mut b = sample_record();
        b.version = "1.3.2".to_string();

        let text = format!(
            "\n{}\n\n{}\n",
            serialize_record(&a).unwrap(),
            serialize_record(&b).unwrap()
        );

        let parsed = parse_index(text.as_bytes(), "test").unwrap();
        assert_eq!(parsed, vec![a, b]);
    }

    #[test]
    fn rejects_malformed_json_with_line_number() {
        let text = format!(
            "{}\nnot json at all\n",
            serialize_record(&sample_record()).unwrap()
        );
        let err = parse_index(text.as_bytes(), "z/zlib.idx").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("z/zlib.idx:2"), "unexpected message: {msg}");
    }

    #[test]
    fn rejects_unknown_format_version() {
        let mut record = sample_record();
        record.format_version = CURRENT_FORMAT_VERSION + 1;
        let line = serialize_record(&record).unwrap();

        let err = parse_index(line.as_bytes(), "test").unwrap_err();
        assert!(err
            .to_string()
            .contains("unsupported tier-1 index format version"));
    }

    #[test]
    fn yanked_record_is_not_available() {
        let mut record = sample_record();
        assert!(record.is_available());
        record.yanked = true;
        assert!(!record.is_available());
    }

    #[test]
    fn missing_index_file_reads_as_none_not_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("z").join("zlib.idx");
        assert!(read_index_file(&path).unwrap().is_none());
    }

    #[test]
    fn append_then_read_round_trips() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("z").join("zlib.idx");

        let a = sample_record();
        let mut b = sample_record();
        b.version = "1.3.2".to_string();

        append_record(&path, &a).unwrap();
        append_record(&path, &b).unwrap();

        let records = read_index_file(&path).unwrap().unwrap();
        assert_eq!(records, vec![a, b]);
    }
}
