//! Surface contract - what a target exports/requires.
//!
//! The Surface is the core abstraction for C dependency management.
//! It defines what compile-time and link-time requirements a package
//! exports (public) vs uses internally (private).
//!
//! Key principle: public surfaces propagate to dependents, private don't.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Complete surface contract for a target.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Surface {
    /// Compile-time requirements (includes, defines, flags)
    #[serde(default)]
    pub compile: CompileSurface,

    /// Link-time requirements (libraries, flags)
    #[serde(default)]
    pub link: LinkSurface,

    /// ABI-affecting toggles
    #[serde(default)]
    pub abi: AbiToggles,

    /// Platform-conditional patches
    #[serde(default, rename = "when")]
    pub conditionals: Vec<ConditionalSurface>,
}

/// Compile-time surface (public vs private).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompileSurface {
    /// Requirements that propagate to dependents
    #[serde(default)]
    pub public: CompileRequirements,

    /// Internal-only requirements
    #[serde(default)]
    pub private: CompileRequirements,
}

/// Compile-time requirements.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompileRequirements {
    /// Include directories (-I)
    #[serde(default)]
    pub include_dirs: Vec<PathBuf>,

    /// Preprocessor defines (-D)
    /// Each entry is (name, optional_value)
    #[serde(default)]
    pub defines: Vec<Define>,

    /// Additional compiler flags
    #[serde(default)]
    pub cflags: Vec<String>,
}

/// A preprocessor define.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Define {
    /// Simple flag: -DFOO
    Flag(String),
    /// Key-value: -DFOO=bar
    KeyValue { name: String, value: String },
}

impl Define {
    /// Create a simple flag define.
    pub fn flag(name: impl Into<String>) -> Self {
        Define::Flag(name.into())
    }

    /// Create a key-value define.
    pub fn key_value(name: impl Into<String>, value: impl Into<String>) -> Self {
        Define::KeyValue {
            name: name.into(),
            value: value.into(),
        }
    }

    /// Get the define name.
    pub fn name(&self) -> &str {
        match self {
            Define::Flag(n) => n,
            Define::KeyValue { name, .. } => name,
        }
    }

    /// Get the define value, if any.
    pub fn value(&self) -> Option<&str> {
        match self {
            Define::Flag(_) => None,
            Define::KeyValue { value, .. } => Some(value),
        }
    }

    /// Convert to compiler flag format.
    pub fn to_flag(&self) -> String {
        match self {
            Define::Flag(name) => format!("-D{}", name),
            Define::KeyValue { name, value } => format!("-D{}={}", name, value),
        }
    }
}

/// Link-time surface (public vs private).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LinkSurface {
    /// Requirements that propagate to dependents
    #[serde(default)]
    pub public: LinkRequirements,

    /// Internal-only requirements
    #[serde(default)]
    pub private: LinkRequirements,
}

/// Link-time requirements.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LinkRequirements {
    /// Libraries to link against
    #[serde(default)]
    pub libs: Vec<LibRef>,

    /// Additional linker flags
    #[serde(default)]
    pub ldflags: Vec<String>,

    /// Link groups for ordering control
    #[serde(default)]
    pub groups: Vec<LinkGroup>,

    /// macOS frameworks
    #[serde(default)]
    pub frameworks: Vec<String>,
}

/// A library reference.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum LibRef {
    /// System library (e.g., -lm, -lpthread)
    System { name: String },

    /// macOS framework
    Framework { name: String },

    /// Vendored library at a specific path
    Path { path: PathBuf },

    /// Library from another Harbour package
    Package { name: String, target: String },
}

impl LibRef {
    /// Create a system library reference.
    pub fn system(name: impl Into<String>) -> Self {
        LibRef::System { name: name.into() }
    }

    /// Create a framework reference.
    pub fn framework(name: impl Into<String>) -> Self {
        LibRef::Framework { name: name.into() }
    }

    /// Create a path library reference.
    pub fn path(path: impl Into<PathBuf>) -> Self {
        LibRef::Path { path: path.into() }
    }

    /// Create a package library reference.
    pub fn package(name: impl Into<String>, target: impl Into<String>) -> Self {
        LibRef::Package {
            name: name.into(),
            target: target.into(),
        }
    }

    /// Convert to linker flag(s).
    pub fn to_flags(&self) -> Vec<String> {
        match self {
            LibRef::System { name } => vec![format!("-l{}", name)],
            LibRef::Framework { name } => vec!["-framework".to_string(), name.clone()],
            LibRef::Path { path } => vec![path.display().to_string()],
            LibRef::Package { .. } => {
                // Resolved during build planning
                vec![]
            }
        }
    }
}

/// Link group for controlling link order.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LinkGroup {
    /// Wrap in --whole-archive / --no-whole-archive
    WholeArchive { libs: Vec<String> },

    /// Wrap in --start-group / --end-group (for circular deps)
    StartEndGroup { libs: Vec<String> },
}

/// ABI-affecting toggles that influence the cache key.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AbiToggles {
    /// List of ABI-relevant settings
    #[serde(default)]
    pub toggles: Vec<String>,
}

impl AbiToggles {
    /// Common ABI toggles
    pub const PIC: &'static str = "pic";
    pub const VISIBILITY: &'static str = "visibility";
    pub const CRT: &'static str = "crt";
    pub const STDLIB: &'static str = "stdlib";

    /// Check if a toggle is enabled.
    pub fn has(&self, toggle: &str) -> bool {
        self.toggles.iter().any(|t| t == toggle)
    }
}

/// Platform-conditional surface patches.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConditionalSurface {
    /// Platform condition
    #[serde(flatten)]
    pub condition: PlatformCondition,

    /// Additional compile requirements (public)
    #[serde(default, rename = "compile.public")]
    pub compile_public: Option<CompileRequirements>,

    /// Additional link requirements (public)
    #[serde(default, rename = "link.public")]
    pub link_public: Option<LinkRequirements>,
}

/// Platform condition for conditional surfaces.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlatformCondition {
    /// Operating system: "linux", "macos", "windows"
    #[serde(default)]
    pub os: Option<String>,

    /// CPU architecture: "x86_64", "aarch64"
    #[serde(default)]
    pub arch: Option<String>,

    /// Environment: "gnu", "musl", "msvc"
    #[serde(default)]
    pub env: Option<String>,

    /// Compiler family: "gcc", "clang", "msvc"
    #[serde(default)]
    pub compiler: Option<String>,
}

impl PlatformCondition {
    /// Check if this condition matches the current platform.
    pub fn matches(&self, target: &TargetPlatform) -> bool {
        if let Some(ref os) = self.os {
            if os != &target.os {
                return false;
            }
        }
        if let Some(ref arch) = self.arch {
            if arch != &target.arch {
                return false;
            }
        }
        if let Some(ref env) = self.env {
            if Some(env.as_str()) != target.env.as_deref() {
                return false;
            }
        }
        if let Some(ref compiler) = self.compiler {
            if Some(compiler.as_str()) != target.compiler.as_deref() {
                return false;
            }
        }
        true
    }
}

/// Target platform information for evaluating conditions.
#[derive(Debug, Clone)]
pub struct TargetPlatform {
    pub os: String,
    pub arch: String,
    pub env: Option<String>,
    pub compiler: Option<String>,
}

impl TargetPlatform {
    /// Detect the current host platform.
    pub fn host() -> Self {
        TargetPlatform {
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            env: None, // Could be detected from compiler
            compiler: None,
        }
    }

    /// Set the compiler family.
    pub fn with_compiler(mut self, compiler: impl Into<String>) -> Self {
        self.compiler = Some(compiler.into());
        self
    }
}

impl Surface {
    /// Create an empty surface.
    pub fn empty() -> Self {
        Surface::default()
    }

    /// Apply platform conditions and return the effective surface.
    pub fn resolve(&self, platform: &TargetPlatform) -> ResolvedSurface {
        let mut compile_public = self.compile.public.clone();
        let mut link_public = self.link.public.clone();

        // Apply matching conditionals
        for cond in &self.conditionals {
            if cond.condition.matches(platform) {
                if let Some(ref cp) = cond.compile_public {
                    compile_public.merge(cp);
                }
                if let Some(ref lp) = cond.link_public {
                    link_public.merge(lp);
                }
            }
        }

        ResolvedSurface {
            compile_public,
            compile_private: self.compile.private.clone(),
            link_public,
            link_private: self.link.private.clone(),
            abi: self.abi.clone(),
        }
    }
}

impl CompileRequirements {
    /// Merge another set of requirements into this one.
    pub fn merge(&mut self, other: &CompileRequirements) {
        self.include_dirs.extend(other.include_dirs.iter().cloned());
        self.defines.extend(other.defines.iter().cloned());
        self.cflags.extend(other.cflags.iter().cloned());
    }

    /// Check if empty.
    pub fn is_empty(&self) -> bool {
        self.include_dirs.is_empty() && self.defines.is_empty() && self.cflags.is_empty()
    }
}

impl LinkRequirements {
    /// Merge another set of requirements into this one.
    pub fn merge(&mut self, other: &LinkRequirements) {
        self.libs.extend(other.libs.iter().cloned());
        self.ldflags.extend(other.ldflags.iter().cloned());
        self.groups.extend(other.groups.iter().cloned());
        self.frameworks.extend(other.frameworks.iter().cloned());
    }

    /// Check if empty.
    pub fn is_empty(&self) -> bool {
        self.libs.is_empty()
            && self.ldflags.is_empty()
            && self.groups.is_empty()
            && self.frameworks.is_empty()
    }
}

/// A resolved surface with platform conditions applied.
#[derive(Debug, Clone)]
pub struct ResolvedSurface {
    pub compile_public: CompileRequirements,
    pub compile_private: CompileRequirements,
    pub link_public: LinkRequirements,
    pub link_private: LinkRequirements,
    pub abi: AbiToggles,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_define_to_flag() {
        let d1 = Define::flag("DEBUG");
        assert_eq!(d1.to_flag(), "-DDEBUG");

        let d2 = Define::key_value("VERSION", "1");
        assert_eq!(d2.to_flag(), "-DVERSION=1");
    }

    #[test]
    fn test_lib_ref_to_flags() {
        let lib = LibRef::system("pthread");
        assert_eq!(lib.to_flags(), vec!["-lpthread"]);

        let framework = LibRef::framework("Security");
        assert_eq!(framework.to_flags(), vec!["-framework", "Security"]);
    }

    #[test]
    fn test_platform_condition_matching() {
        let platform = TargetPlatform {
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            env: Some("gnu".to_string()),
            compiler: Some("gcc".to_string()),
        };

        let cond1 = PlatformCondition {
            os: Some("linux".to_string()),
            ..Default::default()
        };
        assert!(cond1.matches(&platform));

        let cond2 = PlatformCondition {
            os: Some("windows".to_string()),
            ..Default::default()
        };
        assert!(!cond2.matches(&platform));

        let cond3 = PlatformCondition {
            os: Some("linux".to_string()),
            arch: Some("x86_64".to_string()),
            ..Default::default()
        };
        assert!(cond3.matches(&platform));
    }

    #[test]
    fn test_surface_resolve() {
        let mut surface = Surface::empty();
        surface.compile.public.defines.push(Define::flag("COMMON"));

        surface.conditionals.push(ConditionalSurface {
            condition: PlatformCondition {
                os: Some("windows".to_string()),
                ..Default::default()
            },
            compile_public: Some(CompileRequirements {
                defines: vec![Define::flag("WIN32")],
                ..Default::default()
            }),
            link_public: None,
        });

        // Linux platform should not have WIN32
        let linux = TargetPlatform {
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            env: None,
            compiler: None,
        };
        let resolved = surface.resolve(&linux);
        assert_eq!(resolved.compile_public.defines.len(), 1);

        // Windows platform should have WIN32
        let windows = TargetPlatform {
            os: "windows".to_string(),
            arch: "x86_64".to_string(),
            env: None,
            compiler: None,
        };
        let resolved = surface.resolve(&windows);
        assert_eq!(resolved.compile_public.defines.len(), 2);
    }
}
