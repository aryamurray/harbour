//! FFI runtime bundling operation.
//!
//! Creates a self-contained bundle of a shared library and its runtime dependencies
//! for FFI consumption by other languages.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::builder::shim::{DiscoveredSurface, LibraryKind};
use crate::core::target::TargetTriple;

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
    // TODO: this should be the actual build target, plumbed through from
    // `BuildContext` / the wider build path once cross-compilation wiring
    // lands there. Until then we assume the artifact was built for the host,
    // which is correct for every non-cross build.
    let target = TargetTriple::host();

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
                let dest = opts
                    .output_dir
                    .join(dep_path.file_name().context("dependency has no filename")?);

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
            format!(
                "failed to create bundle directory: {}",
                opts.output_dir.display()
            )
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
            if file.kind == BundledFileKind::PrimaryLib || file.kind == BundledFileKind::RuntimeDep
            {
                if let Err(e) = rewrite_rpath(&file.destination, &target) {
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

        let manifest =
            BundleManifest::new(primary_lib_name, runtime_dep_names, total_size, &target);

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
    /// Create a new bundle manifest for the given target.
    ///
    /// `target` is the triple the bundled artifacts were built for -- not
    /// necessarily the host running Harbour -- so the recorded platform is
    /// correct even when cross-compiling.
    pub fn new(
        primary_lib: String,
        runtime_deps: Vec<String>,
        total_size: u64,
        target: &TargetTriple,
    ) -> Self {
        BundleManifest {
            version: 1,
            primary_lib,
            runtime_deps,
            total_size,
            platform: get_platform_string(target),
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

/// Get platform string for the given target.
///
/// Derived entirely from the *target* triple -- never from
/// `std::env::consts` -- so a bundle cross-built for another platform is
/// labelled correctly.
fn get_platform_string(target: &TargetTriple) -> String {
    let os = if target.is_windows() {
        "windows"
    } else if target.is_apple() {
        "macos"
    } else {
        target.os().unwrap_or("unknown")
    };
    format!("{}-{}", os, target.arch())
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

/// The strategy for a target-platform-specific operation: which tool's
/// output format applies, and which tool rewrites/inspects binaries.
///
/// This is derived purely from the *target* triple. It is deliberately not
/// gated by `#[cfg(target_os = ...)]`: all variants are compiled and
/// selectable on every host, because the host running Harbour may differ
/// from the target being bundled (cross-compilation). Whether the
/// corresponding tool is actually *installed* on this machine is a
/// separate, runtime concern -- see the tool-availability checks in each
/// `*_linux`/`*_macos`/`*_windows` implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetToolStrategy {
    /// ELF binaries: `patchelf` / `ldd`.
    Linux,
    /// Mach-O binaries: `otool` / `install_name_tool`.
    Macos,
    /// PE binaries: `dumpbin`; RPATH itself doesn't apply.
    Windows,
    /// No known strategy for this target (e.g. bare metal, WASM, BSDs).
    Unsupported,
}

fn tool_strategy_for(target: &TargetTriple) -> TargetToolStrategy {
    if target.is_windows() {
        TargetToolStrategy::Windows
    } else if target.is_apple() {
        TargetToolStrategy::Macos
    } else if target.os() == Some("linux") {
        TargetToolStrategy::Linux
    } else {
        TargetToolStrategy::Unsupported
    }
}

/// Rewrite RPATH/runpath to be relative, using the strategy appropriate for
/// `target` (the platform the artifact was built for, not necessarily the
/// host running Harbour).
///
/// On Linux targets, sets RPATH to $ORIGIN via `patchelf`.
/// On macOS targets, updates install_name and @rpath references via `otool`
/// / `install_name_tool`.
/// On Windows targets, this is a no-op: DLLs are found via PATH or same
/// directory rather than RPATH.
fn rewrite_rpath(lib_path: &Path, target: &TargetTriple) -> Result<()> {
    match tool_strategy_for(target) {
        TargetToolStrategy::Linux => rewrite_rpath_linux(lib_path),
        TargetToolStrategy::Macos => rewrite_rpath_macos(lib_path),
        TargetToolStrategy::Windows => {
            let _ = lib_path;
            Ok(())
        }
        TargetToolStrategy::Unsupported => {
            tracing::warn!(
                "RPATH rewriting not supported for target '{}'",
                target.as_str()
            );
            Ok(())
        }
    }
}

fn rewrite_rpath_linux(lib_path: &Path) -> Result<()> {
    use std::process::Command;

    // Check if patchelf is available. Missing here does not mean "nothing
    // to do" -- it means the rpath is left wrong and the artifact would
    // ship broken, so this is a hard error rather than a silent skip.
    let patchelf_check = Command::new("patchelf").arg("--version").output();

    if patchelf_check.is_err() {
        bail!(
            "cross-building for linux requires patchelf on this machine, but it was not found in PATH (needed to rewrite RPATH for {})",
            lib_path.display()
        );
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

fn rewrite_rpath_macos(lib_path: &Path) -> Result<()> {
    use std::process::Command;

    // Get current install name
    let output = Command::new("otool")
        .args(["-D", lib_path.to_str().unwrap()])
        .output()
        .map_err(|e| {
            anyhow::anyhow!(
                "cross-building for macos requires otool on this machine, but it was not found in PATH ({e})"
            )
        })?;

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
            .args(["-id", &new_install_name, lib_path.to_str().unwrap()])
            .status()
            .map_err(|e| {
                anyhow::anyhow!(
                    "cross-building for macos requires install_name_tool on this machine, but it was not found in PATH ({e})"
                )
            })?;

        if !status.success() {
            bail!("install_name_tool failed");
        }

        tracing::debug!(
            "Changed install name: {} -> {}",
            old_install_name,
            new_install_name
        );
    }

    // Add @loader_path to rpath. Unlike the tool-missing cases above, a
    // non-zero exit here (e.g. the rpath entry already exists) is benign
    // and not worth failing the whole bundle over -- only a missing tool is.
    let status = Command::new("install_name_tool")
        .args(["-add_rpath", "@loader_path", lib_path.to_str().unwrap()])
        .status()
        .map_err(|e| {
            anyhow::anyhow!(
                "cross-building for macos requires install_name_tool on this machine, but it was not found in PATH ({e})"
            )
        })?;

    if !status.success() {
        tracing::debug!(
            "install_name_tool -add_rpath returned non-zero for {} (likely already present)",
            lib_path.display()
        );
    }

    Ok(())
}

/// Collect runtime dependencies for a shared library built for `target`.
///
/// This uses target-specific tools (ldd, otool, dumpbin) to find the shared
/// libraries that will be needed at runtime. The tool selected follows
/// `target`, not the host running Harbour: inspecting a Linux artifact
/// always means parsing `ldd`-shaped output, even when cross-building from
/// macOS or Windows.
pub fn collect_runtime_deps(lib_path: &Path, target: &TargetTriple) -> Result<Vec<PathBuf>> {
    match tool_strategy_for(target) {
        TargetToolStrategy::Linux => collect_runtime_deps_linux(lib_path),
        TargetToolStrategy::Macos => collect_runtime_deps_macos(lib_path),
        TargetToolStrategy::Windows => collect_runtime_deps_windows(lib_path),
        TargetToolStrategy::Unsupported => {
            tracing::warn!(
                "runtime dependency collection not supported for target '{}'",
                target.as_str()
            );
            Ok(Vec::new())
        }
    }
}

fn collect_runtime_deps_linux(lib_path: &Path) -> Result<Vec<PathBuf>> {
    use std::process::Command;

    let output = Command::new("ldd").arg(lib_path).output().map_err(|e| {
        anyhow::anyhow!(
            "cross-building for linux requires ldd on this machine, but it was not found in PATH ({e})"
        )
    })?;

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

fn collect_runtime_deps_macos(lib_path: &Path) -> Result<Vec<PathBuf>> {
    use std::process::Command;

    let output = Command::new("otool")
        .args(["-L", lib_path.to_str().unwrap()])
        .output()
        .map_err(|e| {
            anyhow::anyhow!(
                "cross-building for macos requires otool on this machine, but it was not found in PATH ({e})"
            )
        })?;

    if !output.status.success() {
        bail!("otool failed");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut deps = Vec::new();

    for line in stdout.lines().skip(1) {
        // Skip first line (library itself)
        let line = line.trim();
        if let Some(paren_pos) = line.find(" (") {
            let path = line[..paren_pos].trim();
            if !path.starts_with('@')
                && !path.starts_with("/usr/lib/")
                && !path.starts_with("/System/")
            {
                // Skip system libraries and @rpath references
                deps.push(PathBuf::from(path));
            }
        }
    }

    Ok(deps)
}

fn collect_runtime_deps_windows(lib_path: &Path) -> Result<Vec<PathBuf>> {
    use std::process::Command;

    // dumpbin (from Visual Studio) is the only tool used here. Its absence
    // is a hard error rather than a silent empty dependency list: shipping
    // a bundle that is missing DLL dependencies is worse than failing loud.
    let output = Command::new("dumpbin")
        .args(["/DEPENDENTS", lib_path.to_str().unwrap()])
        .output()
        .map_err(|e| {
            anyhow::anyhow!(
                "cross-building for windows requires dumpbin on this machine, but it was not found in PATH ({e})"
            )
        })?;

    if !output.status.success() {
        bail!("dumpbin failed with exit code: {:?}", output.status.code());
    }

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

    Ok(deps)
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

    // --- Target-driven dispatch ---
    //
    // These must hold no matter which OS runs the test suite: the strategy
    // follows the *target* triple passed in, never `cfg!(target_os = ...)`.
    // Under the old `#[cfg(target_os = ...)]`-gated code this distinction
    // didn't exist at all -- only the host's own variant was even compiled
    // -- so a Linux target on a macOS host (for example) had no way to
    // select the Linux strategy.

    #[test]
    fn linux_target_selects_linux_strategy_on_any_host() {
        let target = TargetTriple::parse("x86_64-unknown-linux-gnu");
        assert_eq!(tool_strategy_for(&target), TargetToolStrategy::Linux);

        let target = TargetTriple::parse("aarch64-unknown-linux-musl");
        assert_eq!(tool_strategy_for(&target), TargetToolStrategy::Linux);
    }

    #[test]
    fn macos_target_selects_macos_strategy_on_any_host() {
        let target = TargetTriple::parse("x86_64-apple-darwin");
        assert_eq!(tool_strategy_for(&target), TargetToolStrategy::Macos);

        let target = TargetTriple::parse("aarch64-apple-darwin");
        assert_eq!(tool_strategy_for(&target), TargetToolStrategy::Macos);
    }

    #[test]
    fn windows_target_selects_windows_strategy_on_any_host() {
        let target = TargetTriple::parse("x86_64-pc-windows-msvc");
        assert_eq!(tool_strategy_for(&target), TargetToolStrategy::Windows);

        let target = TargetTriple::parse("x86_64-pc-windows-gnu");
        assert_eq!(tool_strategy_for(&target), TargetToolStrategy::Windows);
    }

    #[test]
    fn unsupported_target_falls_back_without_panicking() {
        let target = TargetTriple::parse("thumbv7em-none-eabi");
        assert_eq!(tool_strategy_for(&target), TargetToolStrategy::Unsupported);
    }

    #[test]
    fn rewrite_rpath_is_a_noop_for_windows_targets_regardless_of_host() {
        // Windows targets never touch the filesystem or spawn a process for
        // RPATH rewriting, so this must succeed even for a path that does
        // not exist.
        let target = TargetTriple::parse("x86_64-pc-windows-msvc");
        let bogus = Path::new("/does/not/exist.dll");
        assert!(rewrite_rpath(bogus, &target).is_ok());
    }

    #[test]
    fn unsupported_target_rpath_rewrite_warns_but_does_not_error() {
        let target = TargetTriple::parse("thumbv7em-none-eabi");
        let bogus = Path::new("/does/not/exist.bin");
        assert!(rewrite_rpath(bogus, &target).is_ok());
    }

    #[test]
    fn unsupported_target_collect_runtime_deps_returns_empty() {
        let target = TargetTriple::parse("thumbv7em-none-eabi");
        let bogus = Path::new("/does/not/exist.bin");
        let deps = collect_runtime_deps(bogus, &target).expect("must not error");
        assert!(deps.is_empty());
    }

    // --- Platform string derivation ---

    #[test]
    fn platform_string_follows_target_not_host() {
        assert_eq!(
            get_platform_string(&TargetTriple::parse("x86_64-unknown-linux-gnu")),
            "linux-x86_64"
        );
        assert_eq!(
            get_platform_string(&TargetTriple::parse("aarch64-apple-darwin")),
            "macos-aarch64"
        );
        assert_eq!(
            get_platform_string(&TargetTriple::parse("x86_64-pc-windows-msvc")),
            "windows-x86_64"
        );
    }
}
