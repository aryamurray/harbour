//! Surface contract - what a target exports/requires.
//!
//! The Surface is the core abstraction for C/C++ dependency management.
//! It defines what compile-time and link-time requirements a package
//! exports (public) vs uses internally (private).
//!
//! Key principle: public surfaces propagate to dependents, private don't.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::core::features::FeatureSet;
use crate::core::target::{CppStandard, TargetTriple};

/// Complete surface contract for a target.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct CompileSurface {
    /// Requirements that propagate to dependents
    #[serde(default)]
    pub public: CompileRequirements,

    /// Internal-only requirements
    #[serde(default)]
    pub private: CompileRequirements,

    /// Minimum C++ standard required by this library's public API.
    /// When set, dependents must compile with at least this standard.
    /// Only meaningful for the public surface - private requirements don't affect dependents.
    #[serde(default)]
    pub requires_cpp: Option<CppStandard>,
}

/// Compile-time requirements.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
    ///
    /// For string format like "FOO=1", extracts the part before the `=`.
    pub fn name(&self) -> &str {
        match self {
            Define::Flag(s) => {
                // Handle "FOO=value" string format - return part before =
                s.split('=').next().unwrap_or(s)
            }
            Define::KeyValue { name, .. } => name,
        }
    }

    /// Get the define value, if any.
    ///
    /// For string format like "FOO=1", extracts the part after the `=`.
    /// For simple flags like "FOO", returns None.
    pub fn value(&self) -> Option<&str> {
        match self {
            Define::Flag(s) => {
                // Handle "FOO=value" string format - return part after =
                if let Some(idx) = s.find('=') {
                    Some(&s[idx + 1..])
                } else {
                    None
                }
            }
            Define::KeyValue { value, .. } => Some(value),
        }
    }

    /// Convert to compiler flag format.
    ///
    /// Both `"FOO=1"` string format and `{ name = "FOO", value = "1" }` object
    /// format produce the same output: `-DFOO=1`.
    pub fn to_flag(&self) -> String {
        match self {
            Define::Flag(s) => format!("-D{}", s),
            Define::KeyValue { name, value } => format!("-D{}={}", name, value),
        }
    }
}

/// Link-time surface (public vs private).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
///
/// Supports both object and string shorthand formats:
/// - Object: `{ kind = "system", name = "m" }`
/// - String: `"m"` (parsed as system lib), `"-lm"` (same), `"-framework Security"` (framework)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LibRef {
    /// String shorthand: "m", "-lm", "-framework Security"
    Shorthand(String),

    /// Object format with explicit kind
    Object(LibRefObject),
}

/// Object-format library reference with explicit kind tag.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum LibRefObject {
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
        LibRef::Object(LibRefObject::System { name: name.into() })
    }

    /// Create a framework reference.
    pub fn framework(name: impl Into<String>) -> Self {
        LibRef::Object(LibRefObject::Framework { name: name.into() })
    }

    /// Create a path library reference.
    pub fn path(path: impl Into<PathBuf>) -> Self {
        LibRef::Object(LibRefObject::Path { path: path.into() })
    }

    /// Create a package library reference.
    pub fn package(name: impl Into<String>, target: impl Into<String>) -> Self {
        LibRef::Object(LibRefObject::Package {
            name: name.into(),
            target: target.into(),
        })
    }

    /// Parse string shorthand into a proper LibRef variant.
    fn parse_shorthand(s: &str) -> (LibRefKind, String) {
        let s = s.trim();

        // Handle -l prefix: "-lpthread" -> ("pthread", System)
        if let Some(name) = s.strip_prefix("-l") {
            return (LibRefKind::System, name.to_string());
        }

        // Handle -framework prefix: "-framework Security" -> ("Security", Framework)
        if let Some(rest) = s.strip_prefix("-framework") {
            let name = rest.trim();
            return (LibRefKind::Framework, name.to_string());
        }

        // Plain name: "pthread" -> System library
        (LibRefKind::System, s.to_string())
    }

    /// Convert to linker flag(s).
    pub fn to_flags(&self) -> Vec<String> {
        match self {
            LibRef::Shorthand(s) => {
                let (kind, name) = Self::parse_shorthand(s);
                match kind {
                    LibRefKind::System => vec![format!("-l{}", name)],
                    LibRefKind::Framework => vec!["-framework".to_string(), name],
                }
            }
            LibRef::Object(obj) => match obj {
                LibRefObject::System { name } => vec![format!("-l{}", name)],
                LibRefObject::Framework { name } => vec!["-framework".to_string(), name.clone()],
                LibRefObject::Path { path } => vec![path.display().to_string()],
                LibRefObject::Package { .. } => {
                    // Resolved during build planning
                    vec![]
                }
            },
        }
    }

    /// Get the library name if this is a system or framework library.
    pub fn name(&self) -> Option<&str> {
        match self {
            LibRef::Shorthand(s) => Some(s.trim_start_matches("-l").trim()),
            LibRef::Object(obj) => match obj {
                LibRefObject::System { name } => Some(name),
                LibRefObject::Framework { name } => Some(name),
                _ => None,
            },
        }
    }
}

/// Internal enum for shorthand parsing.
enum LibRefKind {
    System,
    Framework,
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
#[serde(deny_unknown_fields)]
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
// `deny_unknown_fields` cannot be used here: it does not coexist with the
// `flatten` below, since serde routes unrecognised keys into
// `PlatformCondition`. The `unknown` catch-all below restores the same
// protection by hand -- see `ConditionalSurface::validate`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConditionalSurface {
    /// Platform condition
    #[serde(flatten)]
    pub condition: PlatformCondition,

    /// Additional compile requirements (public)
    #[serde(default, rename = "compile.public")]
    pub compile_public: Option<CompileRequirements>,

    /// Additional compile requirements applying only to this target's own
    /// sources.
    #[serde(default, rename = "compile.private")]
    pub compile_private: Option<CompileRequirements>,

    /// Additional link requirements (public)
    #[serde(default, rename = "link.public")]
    pub link_public: Option<LinkRequirements>,

    /// Additional link requirements applying only to this target.
    #[serde(default, rename = "link.private")]
    pub link_private: Option<LinkRequirements>,

    /// Anything else written in the block.
    ///
    /// `PlatformCondition` absorbs unrecognised keys because it is
    /// flattened, which silently swallowed whole tables: `compile.private`
    /// parsed cleanly and did nothing, and since `harbour new` scaffolds
    /// `-Wall -Wextra` (and `/W4`) into exactly that table, no generated
    /// project had ever been compiled with warnings enabled. Collecting the
    /// remainder here lets `validate` reject it instead.
    #[serde(flatten, default)]
    pub unknown: std::collections::BTreeMap<String, toml::Value>,
}

impl ConditionalSurface {
    /// Reject keys that are neither a condition nor a requirement table.
    ///
    /// The condition fields are flattened into this struct, so serde cannot
    /// tell an unrecognised key from a condition it has not been taught
    /// about; both land in `unknown`. Filtering the known condition names
    /// out is what leaves genuine mistakes behind.
    pub fn validate(&self) -> anyhow::Result<()> {
        const CONDITION_KEYS: [&str; 5] = ["os", "arch", "env", "compiler", "feature"];

        let unexpected: Vec<&str> = self
            .unknown
            .keys()
            .map(|k| k.as_str())
            .filter(|k| !CONDITION_KEYS.contains(k))
            .collect();

        if !unexpected.is_empty() {
            anyhow::bail!(
                "unknown key(s) in a `surface.when` block: {}\n\
                 hint: a `when` block takes the conditions `os`, `arch`, `env`, \
                 `compiler`, `feature`, and the tables `compile.public`, \
                 `compile.private`, `link.public`, `link.private`",
                unexpected.join(", ")
            );
        }
        Ok(())
    }
}

/// Platform condition for conditional surfaces.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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

    /// A feature name that must be enabled for this condition to match.
    ///
    /// Feature matching is deliberately folded into the same
    /// [`PlatformCondition`] used for arch/os/env/compiler rather than
    /// living behind a second condition type: a feature toggle and a
    /// platform toggle are the same kind of fact ("only apply this patch
    /// when X is true about the build"), and giving them separate
    /// mechanisms would mean every consumer (`Target::when`,
    /// `Surface::when`) has to learn two ways to ask "does this apply?"
    /// instead of one. Only a single feature name is supported (not a
    /// list): a condition needing feature A *and* feature B is
    /// sufficiently rare that it can be expressed by testing the
    /// intersection differently (or simply isn't needed yet), while
    /// wanting A *or* B is already expressible today with two `[[when]]`
    /// blocks. Matching `os`/`arch`/`env`/`compiler`, this field is a
    /// single value, not a set.
    #[serde(default)]
    pub feature: Option<String>,
}

impl PlatformCondition {
    /// Check if this condition matches the current platform and enabled
    /// feature set.
    pub fn matches(&self, target: &TargetPlatform, features: &FeatureSet) -> bool {
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
        if let Some(ref feature) = self.feature {
            if !features.contains(feature.as_str()) {
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
    /// Derive the surface-evaluation platform from a target triple.
    ///
    /// This is the single derivation path: which flags/defines/includes apply
    /// is a property of the target being compiled *for*, never of the host
    /// running Harbour. `host()` is just this applied to `TargetTriple::host()`.
    ///
    /// ## Bare metal (`os`)
    ///
    /// [`TargetPlatform::os`] is a plain (non-`Option`) `String`, so it cannot
    /// represent "no OS" as a distinct third state the way
    /// [`TargetTriple::os`] can (`None`). For a freestanding target
    /// (`TargetTriple::is_bare_metal()` -- covers both an absent OS component,
    /// e.g. `thumbv7em-none-eabi`, and an explicit `none`, e.g.
    /// `riscv32imac-unknown-none-elf`) this derives the empty string.
    ///
    /// That is a deliberate choice, not a lazy default: no real target's OS
    /// name is ever the empty string, so a manifest condition such as
    /// `os = "linux"` can never match it, and a bare-metal target can never
    /// accidentally satisfy a condition written for a hosted platform. The
    /// empty string only ever compares equal to itself, and no author would
    /// write `os = ""` in a manifest to mean "bare metal" -- so in practice
    /// bare-metal-specific surface patches must be written to key off `arch`
    /// (or, once exposed here, a dedicated bare-metal flag) rather than `os`.
    /// If a future manifest format wants an explicit `os = "none"` /
    /// `bare_metal = true` condition, `PlatformCondition`/`TargetPlatform`
    /// will need a real tri-state (or a separate boolean) -- this type
    /// cannot honestly express that today, so it is called out here rather
    /// than faked with a sentinel string.
    ///
    /// ## OS spelling
    ///
    /// [`TargetTriple::os`] preserves LLVM/Rust spelling, which for macOS is
    /// `darwin` (see `x86_64-apple-darwin`), while manifest surface
    /// conditions are documented and written against `"macos"` (see
    /// [`PlatformCondition::os`]). That is normalized here: `darwin` becomes
    /// `macos`; every other OS name passes through unchanged (`linux`,
    /// `windows`, `ios`, `tvos`, ... are already spelled the way conditions
    /// expect).
    pub fn for_target(triple: &TargetTriple) -> Self {
        let os = if triple.is_bare_metal() {
            String::new()
        } else {
            match triple.os() {
                Some("darwin") => "macos".to_string(),
                Some(os) => os.to_string(),
                // Unreachable: `is_bare_metal()` already caught `None`.
                None => String::new(),
            }
        };

        TargetPlatform {
            os,
            arch: triple.arch().to_string(),
            env: triple.env().map(|s| s.to_string()),
            compiler: None,
        }
    }

    /// Detect the current host platform.
    ///
    /// A special case of [`TargetPlatform::for_target`]: the host is simply
    /// the target you get when nobody asked to cross-compile.
    pub fn host() -> Self {
        Self::for_target(&TargetTriple::host())
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

    /// Apply platform/feature conditions and return the effective surface.
    pub fn resolve(&self, platform: &TargetPlatform, features: &FeatureSet) -> ResolvedSurface {
        let mut compile_public = self.compile.public.clone();
        let mut compile_private = self.compile.private.clone();
        let mut link_public = self.link.public.clone();
        let mut link_private = self.link.private.clone();

        // Apply matching conditionals
        for cond in &self.conditionals {
            if cond.condition.matches(platform, features) {
                if let Some(ref cp) = cond.compile_public {
                    compile_public.merge(cp);
                }
                if let Some(ref cp) = cond.compile_private {
                    compile_private.merge(cp);
                }
                if let Some(ref lp) = cond.link_public {
                    link_public.merge(lp);
                }
                if let Some(ref lp) = cond.link_private {
                    link_private.merge(lp);
                }
            }
        }

        ResolvedSurface {
            compile_public,
            compile_private,
            link_public,
            link_private,
            abi: self.abi.clone(),
            requires_cpp: self.compile.requires_cpp,
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
    /// Minimum C++ standard required by this library's public API.
    pub requires_cpp: Option<CppStandard>,
}

#[cfg(test)]
mod tests {

    /// A key that does not belong to a surface table used to be accepted and
    /// silently ignored, so a misplaced or misspelled setting did nothing at
    /// all -- `public_headers` under `compile.public` (it belongs on the
    /// target) parsed cleanly and left the surface unchanged.
    #[test]
    fn unknown_surface_keys_are_rejected() {
        let err = toml::from_str::<CompileRequirements>(
            "include_dirs = [\"include\"]\npublic_headers = [\"include/**/*.h\"]\n",
        )
        .expect_err("a key that is not part of a compile surface must not parse");
        assert!(
            err.to_string().contains("public_headers"),
            "the error must name the offending key, got: {err}"
        );

        // The legitimate keys still parse.
        let ok: CompileRequirements =
            toml::from_str("include_dirs = [\"include\"]\ndefines = [\"A=1\"]\n").unwrap();
        assert_eq!(ok.include_dirs.len(), 1);
    }

    use super::*;

    #[test]
    fn test_define_to_flag() {
        let d1 = Define::flag("DEBUG");
        assert_eq!(d1.to_flag(), "-DDEBUG");

        let d2 = Define::key_value("VERSION", "1");
        assert_eq!(d2.to_flag(), "-DVERSION=1");
    }

    #[test]
    fn test_define_string_shorthand() {
        // Simple flag string: "DEBUG"
        let d1 = Define::Flag("DEBUG".to_string());
        assert_eq!(d1.name(), "DEBUG");
        assert_eq!(d1.value(), None);
        assert_eq!(d1.to_flag(), "-DDEBUG");

        // Key-value string: "VERSION=1"
        let d2 = Define::Flag("VERSION=1".to_string());
        assert_eq!(d2.name(), "VERSION");
        assert_eq!(d2.value(), Some("1"));
        assert_eq!(d2.to_flag(), "-DVERSION=1");

        // Object format still works
        let d3 = Define::KeyValue {
            name: "FOO".to_string(),
            value: "bar".to_string(),
        };
        assert_eq!(d3.name(), "FOO");
        assert_eq!(d3.value(), Some("bar"));
        assert_eq!(d3.to_flag(), "-DFOO=bar");
    }

    #[test]
    fn test_lib_ref_to_flags() {
        let lib = LibRef::system("pthread");
        assert_eq!(lib.to_flags(), vec!["-lpthread"]);

        let framework = LibRef::framework("Security");
        assert_eq!(framework.to_flags(), vec!["-framework", "Security"]);
    }

    #[test]
    fn test_lib_ref_shorthand() {
        // Plain name -> system library
        let lib = LibRef::Shorthand("pthread".to_string());
        assert_eq!(lib.to_flags(), vec!["-lpthread"]);

        // -l prefix -> system library
        let lib2 = LibRef::Shorthand("-lm".to_string());
        assert_eq!(lib2.to_flags(), vec!["-lm"]);

        // -framework prefix -> framework
        let fw = LibRef::Shorthand("-framework Security".to_string());
        assert_eq!(fw.to_flags(), vec!["-framework", "Security"]);
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
        assert!(cond1.matches(&platform, &FeatureSet::new()));

        let cond2 = PlatformCondition {
            os: Some("windows".to_string()),
            ..Default::default()
        };
        assert!(!cond2.matches(&platform, &FeatureSet::new()));

        let cond3 = PlatformCondition {
            os: Some("linux".to_string()),
            arch: Some("x86_64".to_string()),
            ..Default::default()
        };
        assert!(cond3.matches(&platform, &FeatureSet::new()));
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
            compile_private: None,
            link_private: None,
            unknown: Default::default(),
        });

        // Linux platform should not have WIN32
        let linux = TargetPlatform {
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            env: None,
            compiler: None,
        };
        let resolved = surface.resolve(&linux, &FeatureSet::new());
        assert_eq!(resolved.compile_public.defines.len(), 1);

        // Windows platform should have WIN32
        let windows = TargetPlatform {
            os: "windows".to_string(),
            arch: "x86_64".to_string(),
            env: None,
            compiler: None,
        };
        let resolved = surface.resolve(&windows, &FeatureSet::new());
        assert_eq!(resolved.compile_public.defines.len(), 2);
    }

    #[test]
    fn test_platform_condition_feature_matching() {
        let platform = TargetPlatform {
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            env: None,
            compiler: None,
        };

        let cond = PlatformCondition {
            feature: Some("fts5".to_string()),
            ..Default::default()
        };

        let mut features = FeatureSet::new();
        assert!(!cond.matches(&platform, &features));

        features.insert("fts5".to_string());
        assert!(cond.matches(&platform, &features));
    }

    #[test]
    fn test_surface_resolve_gated_by_feature() {
        let mut surface = Surface::empty();

        surface.conditionals.push(ConditionalSurface {
            condition: PlatformCondition {
                feature: Some("fts5".to_string()),
                ..Default::default()
            },
            compile_public: Some(CompileRequirements {
                defines: vec![Define::flag("SQLITE_ENABLE_FTS5")],
                ..Default::default()
            }),
            link_public: None,
            compile_private: None,
            link_private: None,
            unknown: Default::default(),
        });

        let platform = TargetPlatform::host();

        let resolved = surface.resolve(&platform, &FeatureSet::new());
        assert!(resolved.compile_public.defines.is_empty());

        let mut features = FeatureSet::new();
        features.insert("fts5".to_string());
        let resolved = surface.resolve(&platform, &features);
        assert_eq!(resolved.compile_public.defines.len(), 1);
    }

    // --- TargetPlatform::for_target: derivation is a function of the
    // triple, not the host. Every assertion below must hold no matter which
    // machine runs the suite; against the old `host()` (which read
    // `std::env::consts::OS`/`ARCH`), these would only pass by accident on a
    // matching host and fail on every other one.

    #[test]
    fn for_target_windows_is_windows_regardless_of_host() {
        let platform = TargetPlatform::for_target(&TargetTriple::parse("x86_64-pc-windows-msvc"));
        assert_eq!(platform.os, "windows");
        assert_eq!(platform.arch, "x86_64");
        assert_eq!(platform.env, Some("msvc".to_string()));
    }

    #[test]
    fn for_target_macos_normalizes_darwin_spelling() {
        // The triple spells it "darwin"; manifest conditions are written
        // against "macos" (see `PlatformCondition::os` doc comment). If this
        // normalization regresses, an `os = "macos"` condition silently
        // never matches an Apple target again.
        let platform = TargetPlatform::for_target(&TargetTriple::parse("aarch64-apple-darwin"));
        assert_eq!(platform.os, "macos");
        assert_eq!(platform.arch, "aarch64");

        let cond = PlatformCondition {
            os: Some("macos".to_string()),
            ..Default::default()
        };
        assert!(cond.matches(&platform, &FeatureSet::new()));
    }

    #[test]
    fn for_target_linux_is_linux_regardless_of_host() {
        let platform = TargetPlatform::for_target(&TargetTriple::parse("x86_64-unknown-linux-gnu"));
        assert_eq!(platform.os, "linux");
        assert_eq!(platform.arch, "x86_64");
        assert_eq!(platform.env, Some("gnu".to_string()));
    }

    #[test]
    fn for_target_bare_metal_has_empty_os_and_never_matches_a_hosted_condition() {
        let platform = TargetPlatform::for_target(&TargetTriple::parse("thumbv7em-none-eabi"));
        assert_eq!(platform.os, "");
        assert_eq!(platform.arch, "thumbv7em");
        assert_eq!(platform.env, Some("eabi".to_string()));

        // A bare-metal target must not accidentally satisfy a condition
        // written for a hosted platform.
        for os in ["linux", "windows", "macos"] {
            let cond = PlatformCondition {
                os: Some(os.to_string()),
                ..Default::default()
            };
            assert!(
                !cond.matches(&platform, &FeatureSet::new()),
                "bare metal matched os = {os}"
            );
        }

        // Nor is there any legitimate way for a manifest to spell "bare
        // metal" and match it via `os` today -- see the doc comment on
        // `for_target`. An explicitly empty condition os would be absurd to
        // author, but confirm the sentinel isn't accidentally reachable.
        let cond = PlatformCondition {
            os: Some(String::new()),
            ..Default::default()
        };
        assert!(cond.matches(&platform, &FeatureSet::new()));
    }

    #[test]
    fn host_is_derived_via_for_target() {
        // host() must be exactly for_target(&TargetTriple::host()) -- not a
        // second, independently-maintained derivation path.
        let host = TargetPlatform::host();
        let expected = TargetPlatform::for_target(&TargetTriple::host());
        assert_eq!(host.os, expected.os);
        assert_eq!(host.arch, expected.arch);
        assert_eq!(host.env, expected.env);
    }
}
