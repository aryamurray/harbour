//! Vcpkg integration helpers.
//!
//! This module provides utilities for integrating with vcpkg, including:
//! - Resolving vcpkg root and triplet from configuration and environment
//! - Path helpers for vcpkg directory structure
//! - Baseline detection for version pinning
//! - Custom registry configuration

use std::fs;
use std::path::PathBuf;

use crate::core::abi::TargetTriple;
use crate::util::config::VcpkgConfig;

/// Resolved vcpkg integration settings.
#[derive(Debug, Clone)]
pub struct VcpkgIntegration {
    /// Path to the vcpkg root directory
    pub root: PathBuf,
    /// Target triplet (e.g., x64-windows, x64-linux)
    pub triplet: String,
    /// Include directories for compilation
    pub include_dirs: Vec<PathBuf>,
    /// Library directories for linking
    pub lib_dirs: Vec<PathBuf>,
    /// Baseline commit for version pinning (if available)
    pub baseline: Option<String>,
    /// Whether custom registries are configured
    pub has_custom_registries: bool,
}

impl VcpkgIntegration {
    /// Resolve vcpkg integration from config, environment, and target triple.
    pub fn from_config(
        config: &VcpkgConfig,
        target: &TargetTriple,
        is_release: bool,
    ) -> Option<Self> {
        if !is_enabled(config) {
            return None;
        }

        let root = resolve_root(config)?;
        if !root.exists() {
            return None;
        }

        let triplet = resolve_triplet(config, target)?;
        let installed = root.join("installed").join(&triplet);

        let include_dir = installed.join("include");
        let mut include_dirs = Vec::new();
        include_dirs.push(include_dir);

        let mut lib_dirs = Vec::new();
        if !is_release {
            lib_dirs.push(installed.join("debug").join("lib"));
            lib_dirs.push(installed.join("debug").join("lib64"));
        }
        lib_dirs.push(installed.join("lib"));
        lib_dirs.push(installed.join("lib64"));

        // Get baseline from config or detect from vcpkg installation
        let baseline = config
            .baseline
            .clone()
            .or_else(|| detect_baseline(&root));

        let has_custom_registries = config.has_custom_registries();

        Some(VcpkgIntegration {
            root,
            triplet,
            include_dirs,
            lib_dirs,
            baseline,
            has_custom_registries,
        })
    }

    /// Get the path to the vcpkg binary.
    pub fn vcpkg_binary(&self) -> PathBuf {
        let exe = if cfg!(windows) { "vcpkg.exe" } else { "vcpkg" };
        self.root.join(exe)
    }

    /// Get the installed directory for the current triplet.
    pub fn installed_dir(&self) -> PathBuf {
        self.root.join("installed").join(&self.triplet)
    }

    /// Get the share directory for a specific port.
    pub fn port_share_dir(&self, port: &str) -> PathBuf {
        self.installed_dir().join("share").join(port)
    }

    /// Get the path to the versions/baseline.json file.
    pub fn baseline_path(&self) -> PathBuf {
        self.root.join("versions").join("baseline.json")
    }

    /// Check if a port is installed.
    pub fn is_port_installed(&self, port: &str) -> bool {
        self.port_share_dir(port).exists()
    }

    /// List installed ports for the current triplet.
    pub fn list_installed_ports(&self) -> Vec<String> {
        let share_dir = self.installed_dir().join("share");
        if !share_dir.exists() {
            return Vec::new();
        }

        fs::read_dir(&share_dir)
            .ok()
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .filter(|e| e.path().is_dir())
                    .filter_map(|e| e.file_name().into_string().ok())
                    .filter(|name| !name.starts_with('.'))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get version info for an installed port.
    pub fn get_port_version(&self, port: &str) -> Option<String> {
        let vcpkg_json = self.port_share_dir(port).join("vcpkg.json");
        if !vcpkg_json.exists() {
            return None;
        }

        let content = fs::read_to_string(&vcpkg_json).ok()?;
        let json: serde_json::Value = serde_json::from_str(&content).ok()?;

        json.get("version")
            .or_else(|| json.get("version-string"))
            .or_else(|| json.get("version-semver"))
            .or_else(|| json.get("version-date"))
            .and_then(|v| v.as_str())
            .map(String::from)
    }
}

/// Detect baseline from vcpkg's .git directory or builtin-baseline.
fn detect_baseline(root: &PathBuf) -> Option<String> {
    // Try to read from .git/HEAD or refs
    let git_dir = root.join(".git");
    if git_dir.exists() {
        // Try to get current commit
        if let Ok(head) = fs::read_to_string(git_dir.join("HEAD")) {
            let head = head.trim();
            if head.starts_with("ref: ") {
                // It's a reference, read the actual commit
                let ref_path = head.strip_prefix("ref: ")?;
                if let Ok(commit) = fs::read_to_string(git_dir.join(ref_path)) {
                    return Some(commit.trim().to_string());
                }
            } else if head.len() == 40 && head.chars().all(|c| c.is_ascii_hexdigit()) {
                // It's a direct commit hash
                return Some(head.to_string());
            }
        }
    }

    None
}

fn is_enabled(config: &VcpkgConfig) -> bool {
    match config.enabled {
        Some(value) => value,
        // Auto-enable if vcpkg can be detected
        None => {
            std::env::var_os("VCPKG_ROOT").is_some()
                || config.root.is_some()
                || detect_vcpkg_root().is_some()
        }
    }
}

fn resolve_root(config: &VcpkgConfig) -> Option<PathBuf> {
    // Priority: config file > environment variable > auto-detection
    config
        .root
        .clone()
        .or_else(|| std::env::var_os("VCPKG_ROOT").map(PathBuf::from))
        .or_else(detect_vcpkg_root)
}

/// Auto-detect vcpkg root using multiple strategies:
/// 1. Windows: Check LOCALAPPDATA for vcpkg integrate install
/// 2. Find vcpkg in PATH and derive root from binary location
/// 3. Check for .vcpkg-root marker file in parent directories
fn detect_vcpkg_root() -> Option<PathBuf> {
    // Strategy 1: Windows - check for vcpkg integrate install
    #[cfg(windows)]
    if let Some(root) = detect_from_windows_integration() {
        return Some(root);
    }

    // Strategy 2: Find vcpkg binary in PATH
    if let Some(root) = detect_from_path() {
        return Some(root);
    }

    None
}

/// On Windows, vcpkg integrate install creates files in LOCALAPPDATA.
#[cfg(windows)]
fn detect_from_windows_integration() -> Option<PathBuf> {
    let local_app_data = std::env::var_os("LOCALAPPDATA")?;
    let targets_file = PathBuf::from(&local_app_data)
        .join("vcpkg")
        .join("vcpkg.user.targets");

    if !targets_file.exists() {
        return None;
    }

    // Parse the targets file to find vcpkg root
    // The file contains: <Import Project="C:\path\to\vcpkg\scripts\buildsystems\msbuild\vcpkg.targets" ... />
    let content = fs::read_to_string(&targets_file).ok()?;

    // Find the path in the Import Project attribute
    for line in content.lines() {
        if let Some(start) = line.find("Project=\"") {
            let rest = &line[start + 9..];
            if let Some(end) = rest.find('"') {
                let target_path = PathBuf::from(&rest[..end]);
                // Navigate up from scripts/buildsystems/msbuild/vcpkg.targets to vcpkg root
                if let Some(root) = target_path
                    .parent() // msbuild
                    .and_then(|p| p.parent()) // buildsystems
                    .and_then(|p| p.parent()) // scripts
                    .and_then(|p| p.parent()) // vcpkg root
                {
                    if is_valid_vcpkg_root(root) {
                        tracing::debug!("Found vcpkg via Windows integration: {}", root.display());
                        return Some(root.to_path_buf());
                    }
                }
            }
        }
    }

    None
}

/// Stub for non-Windows.
#[cfg(not(windows))]
fn detect_from_windows_integration() -> Option<PathBuf> {
    None
}

/// Find vcpkg in PATH and derive root from binary location.
fn detect_from_path() -> Option<PathBuf> {
    let vcpkg_exe = if cfg!(windows) { "vcpkg.exe" } else { "vcpkg" };

    let vcpkg_path = which::which(vcpkg_exe).ok()?;

    // vcpkg binary is at <root>/vcpkg[.exe]
    let root = vcpkg_path.parent()?;

    if is_valid_vcpkg_root(root) {
        tracing::debug!("Found vcpkg in PATH: {}", root.display());
        return Some(root.to_path_buf());
    }

    None
}

/// Validate that a directory is a valid vcpkg root.
fn is_valid_vcpkg_root(path: &std::path::Path) -> bool {
    // Check for .vcpkg-root marker file (created by vcpkg bootstrap)
    if path.join(".vcpkg-root").exists() {
        return true;
    }

    // Fallback: check for vcpkg binary and scripts directory
    let vcpkg_exe = if cfg!(windows) { "vcpkg.exe" } else { "vcpkg" };
    path.join(vcpkg_exe).exists() && path.join("scripts").is_dir()
}

fn resolve_triplet(config: &VcpkgConfig, target: &TargetTriple) -> Option<String> {
    config
        .triplet
        .clone()
        .or_else(|| std::env::var("VCPKG_TARGET_TRIPLET").ok())
        .or_else(|| std::env::var("VCPKG_DEFAULT_TRIPLET").ok())
        .or_else(|| infer_triplet(target))
}

fn infer_triplet(target: &TargetTriple) -> Option<String> {
    let arch = match target.arch.as_str() {
        "x86_64" => "x64",
        "x86" | "i686" | "i386" => "x86",
        "aarch64" => "arm64",
        "arm" => "arm",
        _ => return None,
    };

    let os = match target.os.as_str() {
        "windows" => "windows",
        "linux" => "linux",
        "macos" | "darwin" => "osx",
        _ => return None,
    };

    Some(format!("{}-{}", arch, os))
}
