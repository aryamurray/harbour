//! Harbor.toml manifest parsing and schema.
//!
//! The manifest is the central configuration file for a Harbour package.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use semver::Version;
use serde::{Deserialize, Serialize};

use crate::core::dependency::{DependencySpec, DetailedDependencySpec};
use crate::core::surface::{
    AbiToggles, CompileRequirements, CompileSurface, ConditionalSurface, LinkRequirements,
    LinkSurface, Surface,
};
use crate::core::target::{BuildRecipe, Target, TargetDep, TargetKind};
use crate::util::InternedString;

/// The parsed Harbor.toml manifest.
#[derive(Debug, Clone)]
pub struct Manifest {
    /// Package metadata
    pub package: PackageMetadata,

    /// Top-level dependencies
    pub dependencies: HashMap<String, DependencySpec>,

    /// Build targets
    pub targets: Vec<Target>,

    /// Build profiles
    pub profiles: HashMap<String, Profile>,

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
    package: PackageMetadata,

    #[serde(default)]
    dependencies: HashMap<String, DependencySpec>,

    #[serde(default)]
    targets: HashMap<String, RawTarget>,

    #[serde(default)]
    profile: HashMap<String, Profile>,
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
            toml::from_str(content).with_context(|| "failed to parse Harbor.toml")?;

        let manifest_dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();

        // Convert raw targets to Target structs
        let mut targets = Vec::new();
        for (name, raw_target) in raw.targets {
            targets.push(Self::convert_target(name, raw_target)?);
        }

        // If no targets defined, create a default one based on package name
        if targets.is_empty() {
            targets.push(Target::new(
                raw.package.name.clone(),
                TargetKind::StaticLib,
            ));
        }

        Ok(Manifest {
            package: raw.package,
            dependencies: raw.dependencies,
            targets,
            profiles: raw.profile,
            manifest_dir,
        })
    }

    fn convert_target(name: String, raw: RawTarget) -> Result<Target> {
        let kind = raw.kind.unwrap_or(TargetKind::StaticLib);

        let surface = if let Some(raw_surface) = raw.surface {
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
                .map(|(pkg, dep)| Self::convert_target_dep(pkg, dep))
                .collect()
        } else {
            Vec::new()
        };

        Ok(Target {
            name: InternedString::new(name),
            kind,
            sources: raw.sources,
            public_headers: raw.public_headers,
            surface,
            deps,
            recipe: raw.recipe,
        })
    }

    fn convert_target_dep(package: String, raw: RawTargetDep) -> TargetDep {
        match raw {
            RawTargetDep::Simple(target) => TargetDep {
                package,
                target: Some(target),
                compile: crate::core::target::Visibility::Public,
                link: crate::core::target::Visibility::Public,
            },
            RawTargetDep::Detailed {
                target,
                compile,
                link,
            } => TargetDep {
                package,
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

    /// Get the package name.
    pub fn name(&self) -> &str {
        &self.package.name
    }

    /// Get the package version.
    pub fn version(&self) -> Result<Version> {
        self.package.version()
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

/// Generate a default Harbor.toml for a new package.
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

/// Generate a default Harbor.toml for a library.
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

/// Generate a default Harbor.toml for an executable.
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
        let path = tmp.path().join("Harbor.toml");

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
        let path = tmp.path().join("Harbor.toml");

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
        let path = tmp.path().join("Harbor.toml");

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
}
