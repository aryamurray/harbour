//! FFI runtime bundling operation.
//!
//! Creates a self-contained bundle of a shared library and its runtime dependencies
//! for FFI consumption by other languages.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::builder::shim::{DiscoveredSurface, LibraryKind};

/// Options for creating an FFI bundle.
#[derive(Debug, Clone)]
pub struct BundleOptions {
    /// Output directory for the bundle
    pub output_dir: PathBuf,

    /// Include transitive runtime dependencies
    pub include_transitive: bool,

    /// Rewrite RPATH to $ORIGIN (Linux) or @executable_path (macOS)
    pub rpath_rewrite: bool,

    /// Copy debug symbols if available
    pub include_debug: bool,

    /// Create a manifest file listing all bundled files
    pub create_manifest: bool,

    /// Dry run - don't actually copy files
    pub dry_run: bool,
}

impl Default for BundleOptions {
    fn default() -> Self {
        BundleOptions {
            output_dir: PathBuf::from("ffi_bundle"),
            include_transitive: true,
            rpath_rewrite: true,
            include_debug: false,
            create_manifest: true,
            dry_run: false,
        }
    }
}

impl BundleOptions {
    /// Create new bundle options with the given output directory.
    pub fn new(output_dir: impl Into<PathBuf>) -> Self {
        BundleOptions {
            output_dir: output_dir.into(),
            ..Default::default()
        }
    }

    /// Set whether to include transitive dependencies.
    pub fn with_transitive(mut self, include: bool) -> Self {
        self.include_transitive = include;
        self
    }

    /// Set whether to rewrite RPATH.
    pub fn with_rpath_rewrite(mut self, rewrite: bool) -> Self {
        self.rpath_rewrite = rewrite;
        self
    }

    /// Set dry run mode.
    pub fn with_dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }
}

/// Result of creating an FFI bundle.
#[derive(Debug, Clone)]
pub struct BundleResult {
    /// Path to the primary shared library
    pub primary_lib: PathBuf,

    /// Paths to all bundled runtime dependencies
    pub runtime_deps: Vec<PathBuf>,

    /// Install name mappings (macOS: old name -> new name)
    pub install_names: HashMap<PathBuf, String>,

    /// Total size of the bundle in bytes
    pub total_size: u64,
}

/// A bundled file with metadata.
#[derive(Debug, Clone)]
pub struct BundledFile {
    /// Source path
    pub source: PathBuf,

    /// Destination path (in bundle)
    pub destination: PathBuf,

    /// File kind
    pub kind: BundledFileKind,

    /// File size in bytes
    pub size: u64,
}

/// Kind of bundled file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BundledFileKind {
    /// Primary shared library
    PrimaryLib,
    /// Runtime dependency
    RuntimeDep,
    /// Debug symbols
    DebugSymbols,
    /// Header file
    Header,
}

/// Create an FFI bundle from discovered surface and build artifacts.
///
/// This copies the primary shared library and all its runtime dependencies
/// to a single directory, optionally rewriting RPATH for portability.
pub fn create_ffi_bundle(
    surface: &DiscoveredSurface,
    opts: &BundleOptions,
) -> Result<BundleResult> {
    // Find the primary shared library
    let primary_lib = surface
        .libraries
        .iter()
        .find(|lib| lib.kind == LibraryKind::Shared)
        .context("no shared library found in surface - FFI bundle requires a shared library")?;

    // Collect all files to bundle
    let mut files_to_bundle: Vec<BundledFile> = Vec::new();

    // Add primary library
    let primary_dest = opts.output_dir.join(
        primary_lib
            .path
            .file_name()
            .context("primary library has no filename")?,
    );

    files_to_bundle.push(BundledFile {
        source: primary_lib.path.clone(),
        destination: primary_dest.clone(),
        kind: BundledFileKind::PrimaryLib,
        size: std::fs::metadata(&primary_lib.path)
            .map(|m| m.len())
            .unwrap_or(0),
    });

    // Add runtime dependencies
    if opts.include_transitive {
        for dep_path in &surface.runtime_deps {
            if dep_path.exists() {
                let dest = opts.output_dir.join(
                    dep_path
                        .file_name()
                        .context("dependency has no filename")?,
                );

                files_to_bundle.push(BundledFile {
                    source: dep_path.clone(),
                    destination: dest,
                    kind: BundledFileKind::RuntimeDep,
                    size: std::fs::metadata(dep_path).map(|m| m.len()).unwrap_or(0),
                });
            } else {
                tracing::warn!("Runtime dependency not found: {}", dep_path.display());
            }
        }
    }

    // Create output directory
    if !opts.dry_run {
        std::fs::create_dir_all(&opts.output_dir).with_context(|| {
            format!("failed to create bundle directory: {}", opts.output_dir.display())
        })?;
    }

    // Copy files
    let mut total_size = 0u64;
    let mut runtime_deps = Vec::new();
    let install_names = HashMap::new();

    for file in &files_to_bundle {
        total_size += file.size;

        if opts.dry_run {
            tracing::info!(
                "[dry-run] Would copy {} -> {}",
                file.source.display(),
                file.destination.display()
            );
        } else {
            std::fs::copy(&file.source, &file.destination).with_context(|| {
                format!(
                    "failed to copy {} -> {}",
                    file.source.display(),
                    file.destination.display()
                )
            })?;

            tracing::debug!(
                "Copied {} -> {}",
                file.source.display(),
                file.destination.display()
            );
        }

        if file.kind == BundledFileKind::RuntimeDep {
            runtime_deps.push(file.destination.clone());
        }
    }

    // Rewrite RPATH if requested
    if opts.rpath_rewrite && !opts.dry_run {
        for file in &files_to_bundle {
            if file.kind == BundledFileKind::PrimaryLib
                || file.kind == BundledFileKind::RuntimeDep
            {
                if let Err(e) = rewrite_rpath(&file.destination) {
                    tracing::warn!(
                        "Failed to rewrite RPATH for {}: {}",
                        file.destination.display(),
                        e
                    );
                }
            }
        }
    }

    // Create manifest
    if opts.create_manifest && !opts.dry_run {
        let manifest_path = opts.output_dir.join("bundle_manifest.json");
        let primary_lib_name = primary_dest
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let runtime_dep_names: Vec<String> = runtime_deps
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .collect();

        let manifest = BundleManifest::new(primary_lib_name, runtime_dep_names, total_size);

        let manifest_json = serde_json::to_string_pretty(&manifest)
            .context("failed to serialize bundle manifest")?;

        std::fs::write(&manifest_path, manifest_json)
            .with_context(|| format!("failed to write manifest: {}", manifest_path.display()))?;

        tracing::info!("Created bundle manifest: {}", manifest_path.display());
    }

    Ok(BundleResult {
        primary_lib: primary_dest,
        runtime_deps,
        install_names,
        total_size,
    })
}

/// Bundle manifest for JSON output.
///
/// This manifest contains all the information needed for FFI consumers
/// to load and use the bundled library.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BundleManifest {
    /// Bundle format version
    pub version: u32,

    /// Primary shared library filename
    pub primary_lib: String,

    /// Runtime dependency filenames
    pub runtime_deps: Vec<String>,

    /// Total bundle size in bytes
    pub total_size: u64,

    /// Platform this bundle was created for
    pub platform: String,

    /// Exported function signatures
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exports: Vec<ExportedFunction>,

    /// Type definitions (structs, enums)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub types: Vec<TypeDefinition>,

    /// Constant values
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constants: Vec<ExportedConstant>,
}

impl BundleManifest {
    /// Create a new bundle manifest.
    pub fn new(primary_lib: String, runtime_deps: Vec<String>, total_size: u64) -> Self {
        BundleManifest {
            version: 1,
            primary_lib,
            runtime_deps,
            total_size,
            platform: get_platform_string(),
            exports: Vec::new(),
            types: Vec::new(),
            constants: Vec::new(),
        }
    }

    /// Add exports from parsed headers.
    pub fn with_exports(mut self, exports: Vec<ExportedFunction>) -> Self {
        self.exports = exports;
        self
    }

    /// Add type definitions from parsed headers.
    pub fn with_types(mut self, types: Vec<TypeDefinition>) -> Self {
        self.types = types;
        self
    }

    /// Add constants from parsed headers.
    pub fn with_constants(mut self, constants: Vec<ExportedConstant>) -> Self {
        self.constants = constants;
        self
    }
}

/// Get platform string for the current target.
fn get_platform_string() -> String {
    format!(
        "{}-{}",
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

/// An exported function signature.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExportedFunction {
    /// Function name
    pub name: String,

    /// Return type as a C type string
    pub return_type: String,

    /// Parameter types
    pub params: Vec<FunctionParam>,

    /// Calling convention (cdecl, stdcall, fastcall)
    #[serde(default = "default_calling_convention")]
    pub calling_convention: String,

    /// Whether this is a variadic function
    #[serde(default)]
    pub variadic: bool,

    /// Documentation comment
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
}

fn default_calling_convention() -> String {
    "cdecl".to_string()
}

/// A function parameter.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FunctionParam {
    /// Parameter name (may be empty)
    pub name: String,

    /// Parameter type as a C type string
    pub param_type: String,
}

/// A type definition (struct, enum, or typedef).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind")]
pub enum TypeDefinition {
    /// Struct definition
    #[serde(rename = "struct")]
    Struct {
        /// Struct name
        name: String,
        /// Struct fields
        fields: Vec<StructField>,
        /// Whether this is a packed struct
        #[serde(default)]
        packed: bool,
    },

    /// Enum definition
    #[serde(rename = "enum")]
    Enum {
        /// Enum name
        name: String,
        /// Enum variants
        variants: Vec<EnumVariant>,
    },

    /// Typedef alias
    #[serde(rename = "typedef")]
    Typedef {
        /// New type name
        name: String,
        /// Underlying type
        underlying_type: String,
    },
}

/// A struct field.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StructField {
    /// Field name
    pub name: String,

    /// Field type as a C type string
    pub field_type: String,

    /// Bit width for bitfields
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bit_width: Option<u32>,
}

/// An enum variant.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EnumVariant {
    /// Variant name
    pub name: String,

    /// Explicit value (if any)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<i64>,
}

/// An exported constant value.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExportedConstant {
    /// Constant name
    pub name: String,

    /// Constant value as a string
    pub value: String,

    /// Inferred type
    #[serde(skip_serializing_if = "Option::is_none")]
    pub const_type: Option<String>,
}

/// Rewrite RPATH/runpath to be relative.
///
/// On Linux, sets RPATH to $ORIGIN.
/// On macOS, updates install_name and @rpath references.
fn rewrite_rpath(lib_path: &Path) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        rewrite_rpath_linux(lib_path)
    }

    #[cfg(target_os = "macos")]
    {
        rewrite_rpath_macos(lib_path)
    }

    #[cfg(target_os = "windows")]
    {
        // Windows doesn't use RPATH - DLLs are found via PATH or same directory
        let _ = lib_path;
        Ok(())
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = lib_path;
        tracing::warn!("RPATH rewriting not supported on this platform");
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn rewrite_rpath_linux(lib_path: &Path) -> Result<()> {
    use std::process::Command;

    // Check if patchelf is available
    let patchelf_check = Command::new("patchelf").arg("--version").output();

    if patchelf_check.is_err() {
        tracing::debug!("patchelf not found, skipping RPATH rewrite");
        return Ok(());
    }

    // Set RPATH to $ORIGIN
    let status = Command::new("patchelf")
        .args(["--set-rpath", "$ORIGIN", lib_path.to_str().unwrap()])
        .status()
        .context("failed to run patchelf")?;

    if !status.success() {
        bail!("patchelf failed with exit code: {:?}", status.code());
    }

    tracing::debug!("Set RPATH to $ORIGIN for {}", lib_path.display());
    Ok(())
}

#[cfg(target_os = "macos")]
fn rewrite_rpath_macos(lib_path: &Path) -> Result<()> {
    use std::process::Command;

    // Get current install name
    let output = Command::new("otool")
        .args(["-D", lib_path.to_str().unwrap()])
        .output()
        .context("failed to run otool")?;

    if !output.status.success() {
        bail!("otool failed");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();

    if lines.len() < 2 {
        return Ok(()); // No install name to change
    }

    let old_install_name = lines[1].trim();
    let filename = lib_path
        .file_name()
        .context("no filename")?
        .to_string_lossy();
    let new_install_name = format!("@rpath/{}", filename);

    if old_install_name != new_install_name {
        // Change install name
        let status = Command::new("install_name_tool")
            .args([
                "-id",
                &new_install_name,
                lib_path.to_str().unwrap(),
            ])
            .status()
            .context("failed to run install_name_tool")?;

        if !status.success() {
            bail!("install_name_tool failed");
        }

        tracing::debug!(
            "Changed install name: {} -> {}",
            old_install_name,
            new_install_name
        );
    }

    // Add @loader_path to rpath
    let _ = Command::new("install_name_tool")
        .args(["-add_rpath", "@loader_path", lib_path.to_str().unwrap()])
        .status();

    Ok(())
}

/// Collect runtime dependencies for a shared library.
///
/// This uses platform-specific tools (ldd, otool, dumpbin) to find
/// the shared libraries that will be needed at runtime.
pub fn collect_runtime_deps(lib_path: &Path) -> Result<Vec<PathBuf>> {
    #[cfg(target_os = "linux")]
    {
        collect_runtime_deps_linux(lib_path)
    }

    #[cfg(target_os = "macos")]
    {
        collect_runtime_deps_macos(lib_path)
    }

    #[cfg(target_os = "windows")]
    {
        collect_runtime_deps_windows(lib_path)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = lib_path;
        Ok(Vec::new())
    }
}

#[cfg(target_os = "linux")]
fn collect_runtime_deps_linux(lib_path: &Path) -> Result<Vec<PathBuf>> {
    use std::process::Command;

    let output = Command::new("ldd")
        .arg(lib_path)
        .output()
        .context("failed to run ldd")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut deps = Vec::new();

    for line in stdout.lines() {
        // Parse ldd output: "libfoo.so.1 => /usr/lib/libfoo.so.1 (0x...)"
        if let Some(arrow_pos) = line.find("=>") {
            let after_arrow = &line[arrow_pos + 2..];
            if let Some(path_end) = after_arrow.find(" (") {
                let path = after_arrow[..path_end].trim();
                if !path.is_empty() && !path.starts_with("linux-") {
                    // Skip virtual DSOs like linux-vdso.so
                    deps.push(PathBuf::from(path));
                }
            }
        }
    }

    Ok(deps)
}

#[cfg(target_os = "macos")]
fn collect_runtime_deps_macos(lib_path: &Path) -> Result<Vec<PathBuf>> {
    use std::process::Command;

    let output = Command::new("otool")
        .args(["-L", lib_path.to_str().unwrap()])
        .output()
        .context("failed to run otool")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut deps = Vec::new();

    for line in stdout.lines().skip(1) {
        // Skip first line (library itself)
        let line = line.trim();
        if let Some(paren_pos) = line.find(" (") {
            let path = &line[..paren_pos].trim();
            if !path.starts_with("@") && !path.starts_with("/usr/lib/") && !path.starts_with("/System/") {
                // Skip system libraries and @rpath references
                deps.push(PathBuf::from(path));
            }
        }
    }

    Ok(deps)
}

#[cfg(target_os = "windows")]
fn collect_runtime_deps_windows(lib_path: &Path) -> Result<Vec<PathBuf>> {
    use std::process::Command;

    // Try dumpbin if available (from Visual Studio)
    let output = Command::new("dumpbin")
        .args(["/DEPENDENTS", lib_path.to_str().unwrap()])
        .output();

    if let Ok(output) = output {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let mut deps = Vec::new();

            let mut in_deps_section = false;
            for line in stdout.lines() {
                if line.contains("Image has the following dependencies:") {
                    in_deps_section = true;
                    continue;
                }
                if in_deps_section {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        break;
                    }
                    if trimmed.ends_with(".dll") || trimmed.ends_with(".DLL") {
                        // Note: dumpbin only gives names, not paths
                        // Would need to search PATH to find actual locations
                        deps.push(PathBuf::from(trimmed));
                    }
                }
            }

            return Ok(deps);
        }
    }

    // Fallback: no dependency analysis
    tracing::debug!("dumpbin not available, cannot analyze DLL dependencies");
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bundle_options_default() {
        let opts = BundleOptions::default();
        assert!(opts.include_transitive);
        assert!(opts.rpath_rewrite);
        assert!(!opts.dry_run);
    }

    #[test]
    fn test_bundle_options_builder() {
        let opts = BundleOptions::new("/tmp/bundle")
            .with_transitive(false)
            .with_dry_run(true);

        assert_eq!(opts.output_dir, PathBuf::from("/tmp/bundle"));
        assert!(!opts.include_transitive);
        assert!(opts.dry_run);
    }
}
