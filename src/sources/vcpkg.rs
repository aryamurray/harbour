//! Vcpkg source integration.
//!
//! This module provides integration with vcpkg, a C/C++ package manager.
//! Instead of relying on fragile CLI output parsing, we read metadata
//! directly from vcpkg's installed file structure:
//!
//! ```text
//! <vcpkg-root>/
//! ├── installed/<triplet>/
//! │   ├── include/              # Headers
//! │   ├── lib/                  # Libraries
//! │   └── share/<port>/
//! │       ├── vcpkg.json        # Port metadata (version info)
//! │       ├── usage             # Library usage hints
//! │       └── copyright
//! └── versions/
//!     └── baseline.json         # Version baseline
//! ```

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use semver::Version;
use serde::Deserialize;

use crate::core::{Dependency, Manifest, Package, PackageId, SourceId, Summary};
use crate::util::process::ProcessBuilder;
use crate::util::VcpkgIntegration;

/// Metadata from a vcpkg port's installed vcpkg.json file.
#[derive(Debug, Deserialize)]
struct VcpkgPortInfo {
    #[serde(default)]
    version: Option<String>,
    #[serde(rename = "version-string", default)]
    version_string: Option<String>,
    #[serde(rename = "version-semver", default)]
    version_semver: Option<String>,
    #[serde(rename = "version-date", default)]
    version_date: Option<String>,
    #[serde(rename = "port-version", default)]
    port_version: Option<u32>,
}

impl VcpkgPortInfo {
    /// Get the version string from any of the version fields.
    fn get_version(&self) -> Option<&str> {
        self.version
            .as_deref()
            .or(self.version_semver.as_deref())
            .or(self.version_string.as_deref())
            .or(self.version_date.as_deref())
    }
}

/// Provenance information for a vcpkg package, used for lockfile pinning.
#[derive(Debug, Clone)]
pub struct VcpkgProvenance {
    /// Port name
    pub port: String,
    /// Package version
    pub version: String,
    /// Port version (patch level within the same version)
    pub port_version: u32,
    /// Target triplet
    pub triplet: String,
    /// Enabled features
    pub features: Vec<String>,
    /// Vcpkg baseline commit (for reproducibility)
    pub baseline: Option<String>,
}

/// Vcpkg package source.
pub struct VcpkgSource {
    port: String,
    triplet: String,
    libs: Vec<String>,
    features: Vec<String>,
    baseline: Option<String>,
    root: PathBuf,
    manifest_dir: PathBuf,
    source_id: SourceId,
}

impl VcpkgSource {
    pub fn new(
        source_id: SourceId,
        integration: &VcpkgIntegration,
        cache_dir: &Path,
    ) -> Result<Self> {
        let port = source_id
            .vcpkg_port()
            .ok_or_else(|| anyhow::anyhow!("vcpkg source missing port"))?
            .to_string();

        let triplet = source_id
            .vcpkg_triplet()
            .unwrap_or_else(|| integration.triplet.clone());

        let libs = source_id.vcpkg_libs().unwrap_or_else(|| vec![port.clone()]);
        let features = source_id.vcpkg_features().unwrap_or_default();
        let baseline = source_id.vcpkg_baseline();

        let manifest_dir = cache_dir.join("vcpkg").join(&triplet).join(&port);

        Ok(VcpkgSource {
            port,
            triplet,
            libs,
            features,
            baseline,
            root: integration.root.clone(),
            manifest_dir,
            source_id,
        })
    }

    fn installed_share_dir(&self) -> PathBuf {
        self.root
            .join("installed")
            .join(&self.triplet)
            .join("share")
            .join(&self.port)
    }

    fn manifest_path(&self) -> PathBuf {
        self.manifest_dir.join("Harbour.toml")
    }

    fn vcpkg_binary(&self) -> PathBuf {
        let exe = if cfg!(windows) { "vcpkg.exe" } else { "vcpkg" };
        self.root.join(exe)
    }

    /// Path to the installed lib directory for this triplet.
    fn installed_lib_dir(&self) -> PathBuf {
        self.root
            .join("installed")
            .join(&self.triplet)
            .join("lib")
    }

    /// Path to the port's vcpkg.json metadata file.
    fn installed_metadata_path(&self) -> PathBuf {
        self.installed_share_dir().join("vcpkg.json")
    }

    /// Path to the port's usage file (contains library hints).
    fn installed_usage_path(&self) -> PathBuf {
        self.installed_share_dir().join("usage")
    }

    fn ensure_installed(&self) -> Result<()> {
        if self.installed_share_dir().exists() {
            return Ok(());
        }

        let vcpkg = self.vcpkg_binary();
        if !vcpkg.exists() {
            bail!(
                "vcpkg binary not found at {}. {}",
                vcpkg.display(),
                diagnose_vcpkg_setup_error()
            );
        }

        // Build the install spec with features if specified
        let spec = self.build_install_spec();
        tracing::info!("Installing vcpkg package: {}", spec);

        let output = ProcessBuilder::new(&vcpkg)
            .arg("install")
            .arg(&spec)
            .exec()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!(
                "vcpkg install failed for {}\n{}\n{}",
                spec,
                stderr,
                diagnose_vcpkg_install_error(&stderr)
            );
        }

        Ok(())
    }

    /// Build the vcpkg install spec string (e.g., "glfw3[wayland,x11]:x64-linux").
    fn build_install_spec(&self) -> String {
        if self.features.is_empty() {
            format!("{}:{}", self.port, self.triplet)
        } else {
            format!(
                "{}[{}]:{}",
                self.port,
                self.features.join(","),
                self.triplet
            )
        }
    }

    /// Read version from installed metadata file (preferred method).
    ///
    /// This reads from `{vcpkg_root}/installed/{triplet}/share/{port}/vcpkg.json`
    /// which is more reliable than parsing `vcpkg list` output.
    fn read_installed_version(&self) -> Result<Option<VcpkgProvenance>> {
        let info_file = self.installed_metadata_path();

        if !info_file.exists() {
            return Ok(None);
        }

        let content = fs::read_to_string(&info_file)
            .with_context(|| format!("failed to read vcpkg metadata: {}", info_file.display()))?;

        let info: VcpkgPortInfo = serde_json::from_str(&content)
            .with_context(|| format!("failed to parse vcpkg metadata: {}", info_file.display()))?;

        let version = info.get_version().unwrap_or("0.0.0").to_string();
        let port_version = info.port_version.unwrap_or(0);

        Ok(Some(VcpkgProvenance {
            port: self.port.clone(),
            version,
            port_version,
            triplet: self.triplet.clone(),
            features: self.features.clone(),
            baseline: self.baseline.clone(),
        }))
    }

    /// Discover libraries provided by this port.
    ///
    /// Tries multiple strategies:
    /// 1. Parse the usage file for library hints
    /// 2. Scan the lib/ directory for matching library files
    /// 3. Fall back to the port name as the library name
    fn discover_libraries(&self) -> Vec<String> {
        // Try to read usage file for hints
        if let Some(libs) = self.parse_usage_file() {
            if !libs.is_empty() {
                return libs;
            }
        }

        // Scan lib directory for libraries matching the port name
        if let Some(libs) = self.scan_lib_directory() {
            if !libs.is_empty() {
                return libs;
            }
        }

        // Fall back to port name
        vec![self.port.clone()]
    }

    /// Parse the usage file for library names.
    fn parse_usage_file(&self) -> Option<Vec<String>> {
        let usage_path = self.installed_usage_path();
        let content = fs::read_to_string(&usage_path).ok()?;

        let mut libs = Vec::new();

        // Look for patterns like:
        // - find_package(GLFW3 CONFIG REQUIRED)
        // - target_link_libraries(main PRIVATE GLFW::glfw)
        // - -l<libname>
        for line in content.lines() {
            let line = line.trim();

            // Extract from target_link_libraries patterns
            if line.contains("target_link_libraries") {
                if let Some(lib) = extract_cmake_target_lib(line) {
                    libs.push(lib);
                }
            }

            // Extract -l flags
            for part in line.split_whitespace() {
                if let Some(lib) = part.strip_prefix("-l") {
                    libs.push(lib.to_string());
                }
            }
        }

        if libs.is_empty() {
            None
        } else {
            Some(libs)
        }
    }

    /// Scan the lib directory for library files.
    fn scan_lib_directory(&self) -> Option<Vec<String>> {
        let lib_dir = self.installed_lib_dir();
        if !lib_dir.exists() {
            return None;
        }

        let mut libs = Vec::new();
        let port_lower = self.port.to_lowercase();

        if let Ok(entries) = fs::read_dir(&lib_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();

                // Extract library name from various formats
                if let Some(lib) = extract_lib_name(&name_str, &port_lower) {
                    if !libs.contains(&lib) {
                        libs.push(lib);
                    }
                }
            }
        }

        if libs.is_empty() {
            None
        } else {
            Some(libs)
        }
    }

    /// Query version using filesystem first, CLI as fallback.
    fn query_version(&self) -> Result<Version> {
        // Try reading from installed metadata (preferred)
        if let Ok(Some(provenance)) = self.read_installed_version() {
            return Ok(normalize_version(&provenance.version));
        }

        // Fall back to CLI parsing for older vcpkg installations
        self.query_version_via_cli()
    }

    /// Query version by parsing vcpkg list output (fallback method).
    fn query_version_via_cli(&self) -> Result<Version> {
        let vcpkg = self.vcpkg_binary();
        if !vcpkg.exists() {
            return Ok(Version::new(0, 0, 0));
        }

        let output = ProcessBuilder::new(vcpkg)
            .arg("list")
            .arg(&self.port)
            .exec()?;

        if !output.status.success() {
            return Ok(Version::new(0, 0, 0));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            let mut parts = line.split_whitespace();
            let name_triplet = match parts.next() {
                Some(value) => value,
                None => continue,
            };
            if !name_triplet.starts_with(&format!("{}:", self.port)) {
                continue;
            }

            let mut name_parts = name_triplet.split(':');
            let _name = name_parts.next();
            let list_triplet = name_parts.next();

            if list_triplet != Some(self.triplet.as_str()) {
                continue;
            }

            if let Some(version_raw) = parts.next() {
                return Ok(normalize_version(version_raw));
            }
        }

        Ok(Version::new(0, 0, 0))
    }

    /// Get provenance information for lockfile pinning.
    pub fn get_provenance(&self) -> Result<Option<VcpkgProvenance>> {
        self.read_installed_version()
    }

    fn write_manifest(&self, version: &Version) -> Result<()> {
        std::fs::create_dir_all(&self.manifest_dir)
            .with_context(|| format!("failed to create {}", self.manifest_dir.display()))?;

        let include_dir = self
            .root
            .join("installed")
            .join(&self.triplet)
            .join("include");
        let include_dir_display = include_dir.display().to_string().replace('\\', "/");

        let libs = self
            .libs
            .iter()
            .map(|lib| format!("\"{}\"", lib))
            .collect::<Vec<_>>()
            .join(", ");

        let manifest = format!(
            r#"[package]
name = "{name}"
version = "{version}"

[targets.{name}]
kind = "header-only"

[targets.{name}.surface.compile.public]
include_dirs = ["{include_dir}"]

[targets.{name}.surface.link.public]
libs = [{libs}]
"#,
            name = self.port,
            version = version,
            include_dir = include_dir_display,
            libs = libs
        );

        std::fs::write(self.manifest_path(), manifest)
            .with_context(|| "failed to write vcpkg manifest")?;

        Ok(())
    }

    fn ensure_manifest(&self) -> Result<Manifest> {
        let version = self.query_version()?;
        self.write_manifest(&version)?;
        Manifest::load(&self.manifest_path())
    }
}

impl crate::sources::Source for VcpkgSource {
    fn name(&self) -> &str {
        "vcpkg"
    }

    fn supports(&self, dep: &Dependency) -> bool {
        dep.source_id().is_vcpkg()
    }

    fn query(&mut self, _dep: &Dependency) -> Result<Vec<Summary>> {
        self.ensure_ready()?;
        let version = self.query_version()?;
        let pkg_id = PackageId::new(self.port.clone(), version, self.source_id);
        Ok(vec![Summary::new(pkg_id, Vec::new(), None)])
    }

    fn ensure_ready(&mut self) -> Result<()> {
        self.ensure_installed()
    }

    fn get_package_path(&self, _pkg_id: PackageId) -> Result<&Path> {
        Ok(self.manifest_dir.as_path())
    }

    fn load_package(&mut self, _pkg_id: PackageId) -> Result<Package> {
        let manifest = self.ensure_manifest()?;
        Package::with_source_id(manifest, self.manifest_dir.clone(), self.source_id)
    }

    fn is_cached(&self, _pkg_id: PackageId) -> bool {
        self.manifest_path().exists()
    }
}

fn normalize_version(raw: &str) -> Version {
    let raw = raw.split('#').next().unwrap_or(raw);
    let raw = raw.split('+').next().unwrap_or(raw);
    if let Ok(version) = Version::parse(raw) {
        return version;
    }

    let mut digits = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_digit() || ch == '.' {
                ch
            } else {
                '.'
            }
        })
        .collect::<String>();
    digits = digits.trim_matches('.').to_string();

    if let Ok(version) = Version::parse(&digits) {
        return version;
    }

    Version::new(0, 0, 0)
}

/// Extract library name from a CMake target_link_libraries line.
fn extract_cmake_target_lib(line: &str) -> Option<String> {
    // Look for patterns like "GLFW::glfw" or just library names after PRIVATE/PUBLIC
    let parts: Vec<&str> = line.split_whitespace().collect();

    for part in parts {
        // Skip CMake keywords
        if matches!(
            part.to_uppercase().as_str(),
            "TARGET_LINK_LIBRARIES"
                | "PRIVATE"
                | "PUBLIC"
                | "INTERFACE"
                | "("
                | ")"
                | "MAIN"
        ) {
            continue;
        }

        // Handle namespace targets like "GLFW::glfw"
        if let Some((_namespace, lib)) = part.split_once("::") {
            return Some(lib.trim_matches(')').to_string());
        }

        // Handle plain library names (skip things that look like targets)
        if !part.contains("(") && !part.contains(")") && !part.starts_with('$') {
            let lib = part.trim_matches(|c| c == ')' || c == '(');
            if !lib.is_empty() {
                return Some(lib.to_string());
            }
        }
    }

    None
}

/// Extract library name from a filename.
fn extract_lib_name(filename: &str, port_hint: &str) -> Option<String> {
    // Handle various library naming conventions:
    // - libfoo.a / libfoo.so / libfoo.dylib (Unix)
    // - foo.lib (Windows)
    // - libfoo.dll.a (MinGW import lib)

    let name = if let Some(name) = filename.strip_prefix("lib") {
        name
    } else {
        filename
    };

    // Remove extensions
    let name = name
        .strip_suffix(".a")
        .or_else(|| name.strip_suffix(".lib"))
        .or_else(|| name.strip_suffix(".so"))
        .or_else(|| name.strip_suffix(".dylib"))
        .or_else(|| name.strip_suffix(".dll.a"))
        .unwrap_or(name);

    // Only return if it contains the port name (to avoid unrelated libs)
    if name.to_lowercase().contains(port_hint) {
        Some(name.to_string())
    } else {
        None
    }
}

/// Provide diagnostic hints for vcpkg setup errors.
fn diagnose_vcpkg_setup_error() -> &'static str {
    "Set VCPKG_ROOT environment variable to your vcpkg installation, \
     or configure [vcpkg.root] in .harbour/config.toml"
}

/// Analyze vcpkg install stderr and provide helpful diagnostics.
fn diagnose_vcpkg_install_error(stderr: &str) -> String {
    if stderr.contains("could not find a port named") {
        return "Port not found. Check the port name or run 'vcpkg search' to find available packages.".to_string();
    }

    if stderr.contains("triplet") && stderr.contains("not found") {
        return "Triplet not found. Check available triplets with 'vcpkg help triplet'.".to_string();
    }

    if stderr.contains("dependency") && stderr.contains("failed") {
        return "A dependency failed to build. Check the build logs above for details.".to_string();
    }

    if stderr.contains("error: building") || stderr.contains("CMake Error") {
        return "Build failed. This may be a vcpkg port issue or missing system dependencies.".to_string();
    }

    if stderr.contains("VCPKG_ROOT") {
        return diagnose_vcpkg_setup_error().to_string();
    }

    String::new()
}
