//! Harbour.toml manifest parsing and schema.
//!
//! The manifest is the central configuration file for a Harbour package.
//! Supports both `Harbour.toml` (canonical) and `Harbor.toml` (alias).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use semver::Version;
use serde::{Deserialize, Serialize};

use crate::core::dependency::DependencySpec;
use crate::core::surface::{
    AbiToggles, CompileRequirements, CompileSurface, ConditionalSurface, LinkRequirements,
    LinkSurface, Surface,
};
use crate::core::target::{BuildRecipe, CppStandard, Language, Target, TargetDepSpec, TargetKind};
use crate::util::InternedString;

/// C++ runtime library selection (non-MSVC platforms).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CppRuntime {
    /// GNU libstdc++ (default on Linux with GCC)
    #[serde(alias = "libstdc++")]
    Libstdcxx,
    /// LLVM libc++ (default on macOS)
    #[serde(alias = "libc++")]
    Libcxx,
}

impl CppRuntime {
    /// Get the compiler flag for this runtime.
    pub fn as_flag(&self) -> &'static str {
        match self {
            CppRuntime::Libstdcxx => "-stdlib=libstdc++",
            CppRuntime::Libcxx => "-stdlib=libc++",
        }
    }
}

/// MSVC runtime library selection (Windows only).
///
/// This controls the /MD vs /MT flag for MSVC builds.
/// Must be consistent across the entire build graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MsvcRuntime {
    /// Dynamic CRT (/MD, /MDd) - default
    #[default]
    Dynamic,
    /// Static CRT (/MT, /MTd)
    Static,
}

impl MsvcRuntime {
    /// Get the compiler flag for this runtime (release mode).
    pub fn as_flag(&self) -> &'static str {
        match self {
            MsvcRuntime::Dynamic => "/MD",
            MsvcRuntime::Static => "/MT",
        }
    }

    /// Get the compiler flag for this runtime (debug mode).
    pub fn as_debug_flag(&self) -> &'static str {
        match self {
            MsvcRuntime::Dynamic => "/MDd",
            MsvcRuntime::Static => "/MTd",
        }
    }
}

/// Workspace configuration from [workspace] section.
///
/// This defines workspace membership, exclusions, and shared dependencies.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WorkspaceConfig {
    /// Glob patterns for workspace member directories.
    #[serde(default)]
    pub members: Vec<String>,

    /// Glob patterns for directories to exclude from workspace.
    #[serde(default)]
    pub exclude: Vec<String>,

    /// Default members to build when no package is specified.
    /// If absent, all members are built.
    #[serde(default, rename = "default-members")]
    pub default_members: Option<Vec<String>>,

    /// Shared dependencies that members can inherit with `workspace = true`.
    /// Parsed from HashMap<String, DependencySpec> during validation.
    #[serde(default)]
    pub dependencies: HashMap<String, DependencySpec>,
}

/// Workspace-level build configuration.
///
/// These settings apply to the entire build graph and are specified
/// in the `[build]` section of Harbour.toml.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BuildConfig {
    /// Default C++ standard for the workspace
    #[serde(default)]
    pub cpp_std: Option<CppStandard>,

    /// C++ runtime library (non-MSVC platforms)
    #[serde(default)]
    pub cpp_runtime: Option<CppRuntime>,

    /// MSVC runtime library (Windows only)
    #[serde(default)]
    pub msvc_runtime: Option<MsvcRuntime>,

    /// Enable C++ exceptions (default: true)
    #[serde(default = "default_true")]
    pub exceptions: bool,

    /// Enable C++ RTTI (default: true)
    #[serde(default = "default_true")]
    pub rtti: bool,
}

fn default_true() -> bool {
    true
}

/// The parsed Harbour.toml manifest.
#[derive(Debug, Clone)]
pub struct Manifest {
    /// Package metadata (None for virtual workspaces)
    pub package: Option<PackageMetadata>,

    /// Workspace configuration (None for non-workspace packages)
    pub workspace: Option<WorkspaceConfig>,

    /// Top-level dependencies
    pub dependencies: HashMap<String, DependencySpec>,

    /// Build targets
    pub targets: Vec<Target>,

    /// Build profiles
    pub profiles: HashMap<String, Profile>,

    /// Build configuration (C++ settings, etc.)
    pub build: BuildConfig,

    /// The directory containing this manifest
    pub manifest_dir: PathBuf,
}

/// Package metadata from [package] section.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageMetadata {
    /// Package name
    pub name: String,

    /// Package version (semver)
    pub version: String,

    /// Package description
    #[serde(default)]
    pub description: Option<String>,

    /// License identifier
    #[serde(default)]
    pub license: Option<String>,

    /// Authors
    #[serde(default)]
    pub authors: Vec<String>,

    /// Repository URL
    #[serde(default)]
    pub repository: Option<String>,

    /// Package homepage
    #[serde(default)]
    pub homepage: Option<String>,

    /// Documentation URL
    #[serde(default)]
    pub documentation: Option<String>,

    /// Keywords for discovery
    #[serde(default)]
    pub keywords: Vec<String>,

    /// Categories
    #[serde(default)]
    pub categories: Vec<String>,
}

impl PackageMetadata {
    /// Parse the version string as semver.
    pub fn version(&self) -> Result<Version> {
        self.version
            .parse()
            .with_context(|| format!("invalid version: {}", self.version))
    }
}

/// Build profile configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Profile {
    /// Optimization level (0, 1, 2, 3, s, z)
    #[serde(default)]
    pub opt_level: Option<String>,

    /// Debug information (0, 1, 2, full)
    #[serde(default)]
    pub debug: Option<String>,

    /// Link-time optimization
    #[serde(default)]
    pub lto: Option<bool>,

    /// Sanitizers to enable
    #[serde(default)]
    pub sanitizers: Vec<String>,

    /// Additional compiler flags
    #[serde(default)]
    pub cflags: Vec<String>,

    /// Additional linker flags
    #[serde(default)]
    pub ldflags: Vec<String>,
}

/// Raw manifest as deserialized from TOML.
#[derive(Debug, Deserialize)]
struct RawManifest {
    #[serde(default)]
    package: Option<PackageMetadata>,

    #[serde(default)]
    workspace: Option<WorkspaceConfig>,

    #[serde(default)]
    dependencies: HashMap<String, DependencySpec>,

    #[serde(default)]
    targets: HashMap<String, RawTarget>,

    #[serde(default)]
    profile: HashMap<String, Profile>,

    #[serde(default)]
    build: BuildConfig,
}

/// Raw target from TOML (before processing).
#[derive(Debug, Deserialize)]
struct RawTarget {
    kind: Option<TargetKind>,

    #[serde(default)]
    sources: Vec<String>,

    #[serde(default)]
    public_headers: Vec<String>,

    #[serde(default)]
    surface: Option<RawSurface>,

    #[serde(default)]
    lang: Language,

    #[serde(default)]
    cpp_std: Option<CppStandard>,

    #[serde(default)]
    deps: Option<HashMap<String, RawTargetDep>>,

    #[serde(default)]
    recipe: Option<BuildRecipe>,
}

/// Raw surface from TOML.
#[derive(Debug, Default, Deserialize)]
struct RawSurface {
    #[serde(default)]
    compile: Option<RawCompileSurface>,

    #[serde(default)]
    link: Option<RawLinkSurface>,

    #[serde(default)]
    abi: Option<AbiToggles>,

    #[serde(default, rename = "when")]
    conditionals: Vec<ConditionalSurface>,
}

#[derive(Debug, Default, Deserialize)]
struct RawCompileSurface {
    #[serde(default)]
    public: Option<CompileRequirements>,

    #[serde(default)]
    private: Option<CompileRequirements>,

    /// Minimum C++ standard required by this library's public API
    #[serde(default)]
    requires_cpp: Option<CppStandard>,
}

#[derive(Debug, Default, Deserialize)]
struct RawLinkSurface {
    #[serde(default)]
    public: Option<LinkRequirements>,

    #[serde(default)]
    private: Option<LinkRequirements>,
}

/// Raw target dependency.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawTargetDep {
    Simple(String),
    Detailed {
        target: Option<String>,
        compile: Option<String>,
        link: Option<String>,
    },
}

impl Manifest {
    /// Load a manifest from a file path.
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read manifest: {}", path.display()))?;

        Self::parse(&content, path)
    }

    /// Parse manifest content.
    pub fn parse(content: &str, path: &Path) -> Result<Self> {
        let raw: RawManifest =
            toml::from_str(content).with_context(|| "failed to parse Harbour.toml")?;

        let manifest_dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();

        // Validate: must have either [package] or [workspace] (or both)
        if raw.package.is_none() && raw.workspace.is_none() {
            anyhow::bail!(
                "manifest at {} must have either [package] or [workspace] section",
                path.display()
            );
        }

        // Convert raw targets to Target structs
        let mut targets = Vec::new();
        for (name, raw_target) in raw.targets {
            targets.push(Self::convert_target(name, raw_target)?);
        }

        // If no targets defined and we have a package, create a default one based on package name
        if targets.is_empty() {
            if let Some(ref pkg) = raw.package {
                targets.push(Target::new(pkg.name.clone(), TargetKind::StaticLib));
            }
            // Virtual workspaces (workspace without package) have no default targets
        }

        Ok(Manifest {
            package: raw.package,
            workspace: raw.workspace,
            dependencies: raw.dependencies,
            targets,
            profiles: raw.profile,
            build: raw.build,
            manifest_dir,
        })
    }

    /// Check if this is a virtual workspace (has [workspace] but no [package]).
    pub fn is_virtual_workspace(&self) -> bool {
        self.workspace.is_some() && self.package.is_none()
    }

    /// Check if this manifest has a workspace section.
    pub fn is_workspace(&self) -> bool {
        self.workspace.is_some()
    }

    fn convert_target(name: String, raw: RawTarget) -> Result<Target> {
        let kind = raw.kind.unwrap_or(TargetKind::StaticLib);

        let surface = if let Some(raw_surface) = raw.surface {
            // Warn about unimplemented features (no silent ignore policy)
            if let Some(ref link) = raw_surface.link {
                if let Some(ref public) = link.public {
                    if !public.groups.is_empty() {
                        tracing::warn!(
                            "target `{}`: LinkGroup is parsed but platform support varies - \
                             may cause link errors on some platforms",
                            name
                        );
                    }
                }
                if let Some(ref private) = link.private {
                    if !private.groups.is_empty() {
                        tracing::warn!(
                            "target `{}`: LinkGroup is parsed but platform support varies - \
                             may cause link errors on some platforms",
                            name
                        );
                    }
                }
            }

            // Warn about conditionals (partially implemented)
            if !raw_surface.conditionals.is_empty() {
                tracing::debug!(
                    "target `{}`: {} conditional surface entries will be applied",
                    name,
                    raw_surface.conditionals.len()
                );
            }

            Surface {
                compile: CompileSurface {
                    public: raw_surface
                        .compile
                        .as_ref()
                        .and_then(|c| c.public.clone())
                        .unwrap_or_default(),
                    private: raw_surface
                        .compile
                        .as_ref()
                        .and_then(|c| c.private.clone())
                        .unwrap_or_default(),
                    requires_cpp: raw_surface
                        .compile
                        .as_ref()
                        .and_then(|c| c.requires_cpp),
                },
                link: LinkSurface {
                    public: raw_surface
                        .link
                        .as_ref()
                        .and_then(|l| l.public.clone())
                        .unwrap_or_default(),
                    private: raw_surface
                        .link
                        .as_ref()
                        .and_then(|l| l.private.clone())
                        .unwrap_or_default(),
                },
                abi: raw_surface.abi.unwrap_or_default(),
                conditionals: raw_surface.conditionals,
            }
        } else {
            Surface::default()
        };

        let deps = if let Some(raw_deps) = raw.deps {
            raw_deps
                .into_iter()
                .map(|(pkg, dep)| {
                    let key = InternedString::new(&pkg);
                    let spec = Self::convert_target_dep(dep);
                    (key, spec)
                })
                .collect()
        } else {
            HashMap::new()
        };

        let target = Target {
            name: InternedString::new(name),
            kind,
            sources: raw.sources,
            public_headers: raw.public_headers,
            surface,
            deps,
            recipe: raw.recipe,
            lang: raw.lang,
            cpp_std: raw.cpp_std,
        };

        // Validate target configuration
        target.validate()?;

        Ok(target)
    }

    fn convert_target_dep(raw: RawTargetDep) -> TargetDepSpec {
        match raw {
            RawTargetDep::Simple(target) => TargetDepSpec {
                target: Some(target),
                compile: crate::core::target::Visibility::Public,
                link: crate::core::target::Visibility::Public,
            },
            RawTargetDep::Detailed {
                target,
                compile,
                link,
            } => TargetDepSpec {
                target,
                compile: compile
                    .map(|s| {
                        if s == "private" {
                            crate::core::target::Visibility::Private
                        } else {
                            crate::core::target::Visibility::Public
                        }
                    })
                    .unwrap_or(crate::core::target::Visibility::Public),
                link: link
                    .map(|s| {
                        if s == "private" {
                            crate::core::target::Visibility::Private
                        } else {
                            crate::core::target::Visibility::Public
                        }
                    })
                    .unwrap_or(crate::core::target::Visibility::Public),
            },
        }
    }

    /// Get the package name (panics if this is a virtual workspace).
    pub fn name(&self) -> &str {
        &self
            .package
            .as_ref()
            .expect("called name() on virtual workspace")
            .name
    }

    /// Get the package name if this manifest has a package section.
    pub fn package_name(&self) -> Option<&str> {
        self.package.as_ref().map(|p| p.name.as_str())
    }

    /// Get the package version (panics if this is a virtual workspace).
    pub fn version(&self) -> Result<Version> {
        self.package
            .as_ref()
            .expect("called version() on virtual workspace")
            .version()
    }

    /// Get the package version if this manifest has a package section.
    pub fn package_version(&self) -> Option<Result<Version>> {
        self.package.as_ref().map(|p| p.version())
    }

    /// Get a target by name.
    pub fn target(&self, name: &str) -> Option<&Target> {
        self.targets.iter().find(|t| t.name.as_str() == name)
    }

    /// Get the default target (first library, or first target).
    pub fn default_target(&self) -> Option<&Target> {
        self.targets
            .iter()
            .find(|t| t.kind.is_library())
            .or_else(|| self.targets.first())
    }

    /// Get a profile by name.
    pub fn profile(&self, name: &str) -> Option<&Profile> {
        self.profiles.get(name)
    }

    /// Get the debug profile (with defaults).
    pub fn debug_profile(&self) -> Profile {
        let mut profile = Profile {
            opt_level: Some("0".to_string()),
            debug: Some("2".to_string()),
            ..Default::default()
        };

        if let Some(custom) = self.profiles.get("debug") {
            merge_profile(&mut profile, custom);
        }

        profile
    }

    /// Get the release profile (with defaults).
    pub fn release_profile(&self) -> Profile {
        let mut profile = Profile {
            opt_level: Some("3".to_string()),
            debug: Some("0".to_string()),
            ..Default::default()
        };

        if let Some(custom) = self.profiles.get("release") {
            merge_profile(&mut profile, custom);
        }

        profile
    }
}

fn merge_profile(base: &mut Profile, custom: &Profile) {
    if custom.opt_level.is_some() {
        base.opt_level = custom.opt_level.clone();
    }
    if custom.debug.is_some() {
        base.debug = custom.debug.clone();
    }
    if custom.lto.is_some() {
        base.lto = custom.lto;
    }
    if !custom.sanitizers.is_empty() {
        base.sanitizers = custom.sanitizers.clone();
    }
    if !custom.cflags.is_empty() {
        base.cflags = custom.cflags.clone();
    }
    if !custom.ldflags.is_empty() {
        base.ldflags = custom.ldflags.clone();
    }
}

/// Generate a default Harbour.toml for a new package.
pub fn generate_default_manifest(name: &str, is_lib: bool) -> String {
    let kind = if is_lib { "staticlib" } else { "exe" };
    let sources = if is_lib {
        "src/**/*.c"
    } else {
        "src/main.c"
    };

    format!(
        r#"[package]
name = "{name}"
version = "0.1.0"
license = "MIT"

[targets.{name}]
kind = "{kind}"
sources = ["{sources}"]
"#
    )
}

/// Generate a default Harbour.toml for a library.
pub fn generate_lib_manifest(name: &str) -> String {
    format!(
        r#"[package]
name = "{name}"
version = "0.1.0"
license = "MIT"

[targets.{name}]
kind = "staticlib"
sources = ["src/**/*.c"]
public_headers = ["include/**/*.h"]

[targets.{name}.surface.compile.public]
include_dirs = ["include"]

[targets.{name}.surface.compile.private]
include_dirs = ["src"]
cflags = ["-Wall", "-Wextra"]
"#
    )
}

/// Generate a default Harbour.toml for an executable.
pub fn generate_exe_manifest(name: &str) -> String {
    format!(
        r#"[package]
name = "{name}"
version = "0.1.0"
license = "MIT"

[targets.{name}]
kind = "exe"
sources = ["src/**/*.c"]

[targets.{name}.surface.compile.private]
cflags = ["-Wall", "-Wextra"]
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_parse_basic_manifest() {
        let content = r#"
[package]
name = "mylib"
version = "1.0.0"

[targets.mylib]
kind = "staticlib"
sources = ["src/**/*.c"]
"#;
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("Harbour.toml");

        let manifest = Manifest::parse(content, &path).unwrap();
        assert_eq!(manifest.name(), "mylib");
        assert_eq!(manifest.version().unwrap(), Version::new(1, 0, 0));
        assert_eq!(manifest.targets.len(), 1);
        assert_eq!(manifest.targets[0].kind, TargetKind::StaticLib);
    }

    #[test]
    fn test_parse_manifest_with_surface() {
        let content = r#"
[package]
name = "mylib"
version = "1.0.0"

[targets.mylib]
kind = "staticlib"
sources = ["src/**/*.c"]
public_headers = ["include/**/*.h"]

[targets.mylib.surface.compile.public]
include_dirs = ["include"]
defines = ["MYLIB_API=1"]

[targets.mylib.surface.compile.private]
include_dirs = ["src"]
cflags = ["-Wall"]

[targets.mylib.surface.link.public]
libs = [
  { kind = "system", name = "m" }
]
"#;
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("Harbour.toml");

        let manifest = Manifest::parse(content, &path).unwrap();
        let target = &manifest.targets[0];

        assert_eq!(target.surface.compile.public.include_dirs.len(), 1);
        assert_eq!(target.surface.compile.private.cflags.len(), 1);
        assert_eq!(target.surface.link.public.libs.len(), 1);
    }

    #[test]
    fn test_parse_manifest_with_deps() {
        let content = r#"
[package]
name = "myapp"
version = "1.0.0"

[dependencies]
mylib = { path = "../mylib" }
zlib = { git = "https://github.com/madler/zlib", tag = "v1.3.1" }

[targets.myapp]
kind = "exe"
sources = ["src/**/*.c"]
"#;
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("Harbour.toml");

        let manifest = Manifest::parse(content, &path).unwrap();
        assert_eq!(manifest.dependencies.len(), 2);
    }

    #[test]
    fn test_generate_lib_manifest() {
        let manifest = generate_lib_manifest("mylib");
        assert!(manifest.contains("name = \"mylib\""));
        assert!(manifest.contains("kind = \"staticlib\""));
        assert!(manifest.contains("public_headers"));
    }

    #[test]
    fn test_parse_virtual_workspace() {
        let content = r#"
[workspace]
members = ["packages/*"]
exclude = ["packages/experimental"]

[workspace.dependencies]
zlib = { git = "https://github.com/madler/zlib", tag = "v1.3.1" }
"#;
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("Harbour.toml");

        let manifest = Manifest::parse(content, &path).unwrap();
        assert!(manifest.is_virtual_workspace());
        assert!(manifest.is_workspace());
        assert!(manifest.package.is_none());

        let ws = manifest.workspace.as_ref().unwrap();
        assert_eq!(ws.members.len(), 1);
        assert_eq!(ws.members[0], "packages/*");
        assert_eq!(ws.exclude.len(), 1);
        assert_eq!(ws.dependencies.len(), 1);
    }

    #[test]
    fn test_parse_workspace_with_package() {
        let content = r#"
[package]
name = "myworkspace"
version = "1.0.0"

[workspace]
members = ["crates/*"]
default-members = ["crates/core"]
"#;
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("Harbour.toml");

        let manifest = Manifest::parse(content, &path).unwrap();
        assert!(!manifest.is_virtual_workspace());
        assert!(manifest.is_workspace());
        assert!(manifest.package.is_some());

        let ws = manifest.workspace.as_ref().unwrap();
        assert_eq!(ws.members.len(), 1);
        assert_eq!(ws.default_members, Some(vec!["crates/core".to_string()]));
    }

    #[test]
    fn test_manifest_requires_package_or_workspace() {
        let content = r#"
[dependencies]
foo = "1.0"
"#;
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("Harbour.toml");

        let result = Manifest::parse(content, &path);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("must have either [package] or [workspace]"));
    }
}
