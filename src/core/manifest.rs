//! Harbour.toml manifest parsing and schema.
//!
//! The manifest is the central configuration file for a Harbour package.
//! Supports both `Harbour.toml` (canonical) and `Harbor.toml` (alias).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use indexmap::IndexMap;
use semver::Version;
use serde::{Deserialize, Serialize};

use crate::core::dependency::DependencySpec;
use crate::core::features::FeatureMap;
use crate::core::probe::{ProbeSet, RawProbeSet};
use crate::core::surface::{
    AbiToggles, CompileRequirements, CompileSurface, ConditionalSurface, LinkRequirements,
    LinkSurface, Surface,
};
use crate::core::target::{
    BuildRecipe, ConditionalSources, CppStandard, CustomCommand, FfiConfig, Language, Target,
    TargetDepSpec, TargetKind,
};
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
#[serde(deny_unknown_fields)]
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
    ///
    /// Declaration-ordered for the same reason as
    /// [`Manifest::dependencies`]: these feed the same seeding path.
    #[serde(default)]
    pub dependencies: DeclOrderMap<String, DependencySpec>,
}

/// A map that iterates in the order the manifest author wrote the keys.
///
/// Used for every manifest section whose declaration order is observable in
/// the build output. Rust randomises `HashMap` iteration per process, so a
/// `HashMap` in any of those positions makes the *same* manifest produce
/// different compile and link command lines on different runs -- the
/// `default_target` coin flip (a package with two libraries contributed a
/// randomly chosen one) and randomly permuted `-l`/archive link order both
/// came from exactly that.
pub type DeclOrderMap<K, V> = IndexMap<K, V>;

/// Workspace-level build configuration.
///
/// These settings apply to the entire build graph and are specified
/// in the `[build]` section of Harbour.toml.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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

/// Hand-written rather than derived, and that is the whole point.
///
/// `RawManifest.build` is `#[serde(default)]`, so a manifest with no
/// `[build]` section at all gets `BuildConfig::default()` -- serde's
/// per-field `default = "default_true"` only fires for a key missing from a
/// table that *is* present. A derived `Default` therefore made
/// `exceptions`/`rtti` false for every manifest that never mentioned
/// `[build]`, and `-fno-exceptions -fno-rtti` reached the compiler: a C++
/// package could not use `throw` or `dynamic_cast` until it added an
/// otherwise-pointless `[build]` table. The two defaults have to agree, so
/// they are written once here.
impl Default for BuildConfig {
    fn default() -> Self {
        BuildConfig {
            cpp_std: None,
            cpp_runtime: None,
            msvc_runtime: None,
            exceptions: default_true(),
            rtti: default_true(),
        }
    }
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

    /// Top-level dependencies, in the order `[dependencies]` declares them.
    ///
    /// Order is load-bearing: `ops::resolve` walks this map to seed the
    /// solver, which fixes the node insertion order of the resolve graph,
    /// which fixes the topological order the linker sees. A `HashMap` here
    /// meant four sibling static libraries came out in a different link
    /// order on almost every run.
    pub dependencies: DeclOrderMap<String, DependencySpec>,

    /// Build targets, in the order `[targets.*]` declares them.
    ///
    /// `default_target` is positional ("first library, else first"), so
    /// this ordering is what makes that rule well-defined.
    pub targets: Vec<Target>,

    /// Build profiles
    pub profiles: HashMap<String, Profile>,

    /// Build configuration (C++ settings, etc.)
    pub build: BuildConfig,

    /// Feature declarations from `[features]`.
    ///
    /// Maps a feature name to the list of *other* feature names it
    /// additionally enables, exactly like Cargo's `[features]` table (see
    /// `crate::core::features`). Empty for packages that don't declare any
    /// features -- selecting a feature nobody declared is a hard error at
    /// resolve time (`features::resolve_features`), not a silent no-op.
    pub features: FeatureMap,

    /// The directory containing this manifest
    pub manifest_dir: PathBuf,
}

/// Package metadata from [package] section.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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

    /// The execution environment this package's code needs.
    ///
    /// C standardises exactly one split here (C §4): a *freestanding*
    /// implementation guarantees only `<float.h>`, `<limits.h>`, `<stdarg.h>`,
    /// `<stddef.h>` and the C11 additions, while a *hosted* one adds the rest
    /// of libc. It is the nearest thing C has to Rust's `core`/`std`
    /// distinction, and unlike almost everything else about a target it is a
    /// guarantee rather than a claim -- which is why this one is enforced.
    ///
    /// Optional on purpose. Absent means the package makes no claim, and
    /// nothing is enforced: defaulting to `hosted` would reject a
    /// freestanding build of a package that is perfectly capable of one and
    /// simply never said so.
    #[serde(default)]
    pub requires: Option<TargetEnvironment>,

    /// Target triples this package is known to build for, as glob patterns
    /// (`*-*-linux-gnu`, `x86_64-pc-windows-msvc`).
    ///
    /// Advisory, and deliberately so. Above the freestanding/hosted line C
    /// offers no guarantees worth enforcing -- glibc, musl, MSVC and newlib
    /// disagree on POSIX coverage, threads and sockets -- so this records
    /// what someone has actually built, not what can work. Building for an
    /// unlisted triple warns; a hard list would have Harbour reject working
    /// builds as targets proliferate, and C's triple space is effectively
    /// unbounded.
    #[serde(default)]
    pub supports: Vec<String>,

    /// The target a dependent gets when it does not name one, i.e. when
    /// there is no `target = "..."` under `[targets.X.deps.<this package>]`.
    ///
    /// Spelled as a *package*-level key naming a target, rather than a
    /// `default = true` flag on a target, for one reason: a flag can be set
    /// on two targets at once, which would replace a hash-order coin flip
    /// with a new ambiguity that has to be errored on. One key in one place
    /// cannot be ambiguous, so "which target is the default" is answerable
    /// by reading `[package]` alone, and a typo is caught by
    /// [`Manifest::parse`] rather than by a confusing link failure.
    ///
    /// Absent means the positional rule still applies: the first declared
    /// library target, else the first declared target. That rule is
    /// well-defined now that `[targets.*]` iterates in declaration order,
    /// so this key is an *override*, not a fix -- existing multi-library
    /// manifests keep working untouched.
    ///
    /// Workspaces: this lives under `[package]`, so it is a property of one
    /// package and a workspace member sets its own independently of the
    /// root. A virtual workspace has no `[package]` and so cannot set it,
    /// which is correct -- a workspace has members, not targets --  and
    /// `[workspace] default_target` is rejected outright by
    /// `deny_unknown_fields` on [`WorkspaceConfig`].
    #[serde(default)]
    pub default_target: Option<String>,
}

/// The execution environment a package's code requires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TargetEnvironment {
    /// Needs a hosted implementation: libc, and normally an OS.
    Hosted,

    /// Runs without libc; safe for bare-metal targets.
    #[serde(alias = "bare-metal", alias = "bare_metal")]
    Freestanding,
}

impl TargetEnvironment {
    /// Whether this requirement is satisfied by `triple`.
    ///
    /// Freestanding code runs anywhere; hosted code needs an OS to host it.
    pub fn is_satisfied_by(&self, triple: &crate::core::target::TargetTriple) -> bool {
        match self {
            TargetEnvironment::Freestanding => true,
            TargetEnvironment::Hosted => !triple.is_bare_metal(),
        }
    }

    /// Name as written in a manifest.
    pub fn as_str(&self) -> &'static str {
        match self {
            TargetEnvironment::Hosted => "hosted",
            TargetEnvironment::Freestanding => "freestanding",
        }
    }
}

/// Whether `triple` matches a `supports` glob such as `*-*-linux-gnu`.
///
/// Compared against the triple's canonical spelling so that an
/// abbreviation in the manifest and a fully-qualified target agree.
pub fn triple_matches_pattern(pattern: &str, triple: &str) -> bool {
    let mut pos = 0usize;
    let parts: Vec<&str> = pattern.split('*').collect();
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        match triple[pos..].find(part) {
            Some(at) => {
                // A leading literal must match at the start; a trailing one
                // must reach the end.
                if i == 0 && at != 0 {
                    return false;
                }
                pos += at + part.len();
            }
            None => return false,
        }
    }
    if parts.last().is_some_and(|p| !p.is_empty()) {
        return triple.ends_with(parts.last().unwrap());
    }
    true
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
#[serde(deny_unknown_fields)]
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

/// Raw backend configuration from TOML (strings, before validation).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawBackendConfig {
    /// Backend identifier as string
    pub backend: Option<String>,

    /// Backend-specific options
    #[serde(default)]
    pub options: toml::Table,
}

/// Validated backend configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendConfig {
    /// Validated backend identifier
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<crate::builder::shim::BackendId>,

    /// Backend-specific options (opaque to manifest)
    #[serde(default)]
    pub options: toml::Table,
}

impl RawBackendConfig {
    /// Validate the raw config and produce a validated BackendConfig.
    pub fn validate(&self) -> anyhow::Result<BackendConfig> {
        let backend = self
            .backend
            .as_ref()
            .map(|s| s.parse::<crate::builder::shim::BackendId>())
            .transpose()
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        Ok(BackendConfig {
            backend,
            options: self.options.clone(),
        })
    }
}

impl Default for BackendConfig {
    fn default() -> Self {
        BackendConfig {
            backend: None,
            options: toml::Table::new(),
        }
    }
}

/// Raw manifest as deserialized from TOML.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    #[serde(default)]
    package: Option<PackageMetadata>,

    #[serde(default)]
    workspace: Option<WorkspaceConfig>,

    #[serde(default)]
    dependencies: DeclOrderMap<String, DependencySpec>,

    #[serde(default)]
    targets: DeclOrderMap<String, RawTarget>,

    #[serde(default)]
    profile: HashMap<String, Profile>,

    #[serde(default)]
    build: BuildConfig,

    /// `[features]` section: feature name -> other feature names it enables.
    #[serde(default)]
    features: FeatureMap,
}

/// Raw target from TOML (before processing).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTarget {
    kind: Option<TargetKind>,

    #[serde(default)]
    sources: Vec<String>,

    #[serde(default)]
    exclude: Vec<String>,

    /// Platform-conditional additions to `sources`/`exclude`.
    #[serde(default, rename = "when")]
    when: Vec<ConditionalSources>,

    /// Steps to run before native compilation (e.g. to generate a header).
    #[serde(default)]
    prebuild: Vec<CustomCommand>,

    /// Configure-style probes: questions to ask the target toolchain, whose
    /// answers become defines on this target's compile surface.
    #[serde(default)]
    probes: Option<RawProbeSet>,

    #[serde(default)]
    public_headers: Vec<String>,

    #[serde(default)]
    surface: Option<RawSurface>,

    /// Shorthand: [targets.X.public] - flattened public surface
    #[serde(default)]
    public: Option<SurfaceShorthand>,

    /// Shorthand: [targets.X.private] - flattened private surface
    #[serde(default)]
    private: Option<SurfaceShorthand>,

    #[serde(default)]
    lang: Language,

    #[serde(default)]
    c_std: Option<crate::core::target::CStandardSpec>,

    #[serde(default)]
    cpp_std: Option<CppStandard>,

    #[serde(default)]
    deps: Option<DeclOrderMap<String, RawTargetDep>>,

    /// Held as a raw value, not a `BuildRecipe`, so the keys written can be
    /// checked before they are thrown away. See `Manifest::parse_recipe`.
    #[serde(default)]
    recipe: Option<toml::Value>,

    /// Backend-specific configuration
    #[serde(default)]
    backend: Option<RawBackendConfig>,

    /// FFI binding generation configuration
    #[serde(default)]
    ffi: Option<FfiConfig>,

    /// Build without a hosted C implementation: `-ffreestanding` +
    /// `-nostdlib`.
    #[serde(default)]
    freestanding: bool,

    /// Linker script, resolved against this package's root.
    #[serde(default)]
    linker_script: Option<PathBuf>,

    /// Entry symbol (`-Wl,--entry=NAME`).
    #[serde(default)]
    entry: Option<String>,
}

/// Shorthand surface format for [targets.X.public] and [targets.X.private].
///
/// This provides a flatter, more ergonomic alternative to the full
/// [targets.X.surface.compile.public] nesting.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct SurfaceShorthand {
    // Compile requirements
    #[serde(default)]
    include_dirs: Vec<PathBuf>,

    #[serde(default)]
    defines: Vec<DefineShorthand>,

    #[serde(default)]
    cflags: Vec<String>,

    // Link requirements
    #[serde(default)]
    libs: Vec<crate::core::surface::LibRef>,

    /// Shorthand for system libraries: system_libs = ["pthread", "m"]
    #[serde(default)]
    system_libs: Vec<String>,

    /// Shorthand for macOS frameworks: frameworks = ["Security", "Foundation"]
    #[serde(default)]
    frameworks: Vec<String>,

    #[serde(default)]
    ldflags: Vec<String>,
}

/// Shorthand define format - supports both "FOO" and "FOO=value" strings.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum DefineShorthand {
    /// String format: "FOO" or "FOO=value"
    String(String),
    /// Object format: { name = "FOO", value = "1" }
    Object { name: String, value: Option<String> },
}

impl DefineShorthand {
    fn to_define(&self) -> crate::core::surface::Define {
        match self {
            DefineShorthand::String(s) => {
                if let Some((name, value)) = s.split_once('=') {
                    crate::core::surface::Define::KeyValue {
                        name: name.to_string(),
                        value: value.to_string(),
                    }
                } else {
                    crate::core::surface::Define::Flag(s.clone())
                }
            }
            DefineShorthand::Object { name, value } => {
                if let Some(v) = value {
                    crate::core::surface::Define::KeyValue {
                        name: name.clone(),
                        value: v.clone(),
                    }
                } else {
                    crate::core::surface::Define::Flag(name.clone())
                }
            }
        }
    }
}

impl SurfaceShorthand {
    fn to_compile_requirements(&self) -> CompileRequirements {
        CompileRequirements {
            include_dirs: self.include_dirs.clone(),
            defines: self.defines.iter().map(|d| d.to_define()).collect(),
            cflags: self.cflags.clone(),
        }
    }

    fn to_link_requirements(&self) -> LinkRequirements {
        let mut libs = self.libs.clone();

        // Add system_libs shorthand
        for name in &self.system_libs {
            libs.push(crate::core::surface::LibRef::system(name.clone()));
        }

        // Add frameworks shorthand
        for name in &self.frameworks {
            libs.push(crate::core::surface::LibRef::framework(name.clone()));
        }

        LinkRequirements {
            libs,
            ldflags: self.ldflags.clone(),
            groups: Vec::new(),
            frameworks: Vec::new(), // Already added as LibRef::Framework
        }
    }

    fn is_empty(&self) -> bool {
        self.include_dirs.is_empty()
            && self.defines.is_empty()
            && self.cflags.is_empty()
            && self.libs.is_empty()
            && self.system_libs.is_empty()
            && self.frameworks.is_empty()
            && self.ldflags.is_empty()
    }
}

/// Raw surface from TOML.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
    Detailed(RawTargetDepDetailed),
}

/// The table form of a target dependency: `{ target, compile, link }`.
///
/// A separate struct rather than an inline enum variant because it needs an
/// `unknown` catch-all and a `validate` step, exactly like
/// [`ConditionalSurface`] and [`ConditionalSources`].
///
/// `deny_unknown_fields` cannot do the job here, for a different reason than
/// at those two sites: this struct is reached through an `untagged` enum, and
/// an untagged variant that fails to deserialize is not an error, it is a
/// signal to try the next variant. So `deny_unknown_fields` would turn a
/// misspelled key into "data did not match any variant of untagged enum
/// RawTargetDep", which names neither the key nor the target. Collecting the
/// remainder and rejecting it by hand is what produces an error a manifest
/// author can act on.
///
/// [`ConditionalSurface`]: crate::core::surface::ConditionalSurface
/// [`ConditionalSources`]: crate::core::target::ConditionalSources
#[derive(Debug, Deserialize)]
struct RawTargetDepDetailed {
    #[serde(default)]
    target: Option<String>,

    #[serde(default)]
    compile: Option<String>,

    #[serde(default)]
    link: Option<String>,

    /// Anything else written in the table.
    #[serde(flatten, default)]
    unknown: std::collections::BTreeMap<String, toml::Value>,
}

impl RawTargetDepDetailed {
    /// Reject unknown keys and non-`public`/`private` visibilities.
    ///
    /// Both halves guard the same hazard, and it is not a cosmetic one: the
    /// fallback for anything unrecognised used to be `Visibility::Public`, so
    /// `compil = "private"` and `compile = "PRIVATE"` each turned a request
    /// for a *private* dependency into a public one, leaking the
    /// dependency's include dirs and defines to everything downstream. A
    /// silent widening of visibility is the one direction that must never be
    /// a typo's default.
    ///
    /// Values are matched case-sensitively and anything else is refused
    /// rather than coerced. Every other enumerated value in the schema is
    /// lowercase and case-sensitive (`os = "macos"`, `kind = "staticlib"`,
    /// `compiler = "clang"`), so accepting `"PRIVATE"` here would make this
    /// one field the exception, and TOML manifests in this ecosystem are
    /// uniformly lowercase. Rejecting is also the only option that fails
    /// *safe*: a case-insensitive match fixes `"PRIVATE"` but still lets
    /// `"privte"` mean public, whereas refusing unknown values closes both.
    fn validate(&self, target_name: &str, dep_name: &str) -> Result<()> {
        /// Keys that are real, but belong to the package-level
        /// `[dependencies]` table. A manifest reaching for these has
        /// confused the two tables rather than misspelled anything: this
        /// table says *how a target consumes* a dependency, not where the
        /// dependency comes from.
        const DEPENDENCY_KEYS: [&str; 9] = [
            "path",
            "version",
            "git",
            "branch",
            "tag",
            "rev",
            "registry",
            "features",
            "default-features",
        ];

        if !self.unknown.is_empty() {
            let unexpected: Vec<&str> = self.unknown.keys().map(|k| k.as_str()).collect();
            let misplaced: Vec<&str> = unexpected
                .iter()
                .copied()
                .filter(|k| DEPENDENCY_KEYS.contains(k))
                .collect();

            let mut hint = String::from(
                "hint: a `targets.<name>.deps` entry takes `target`, `compile` and `link`",
            );
            if !misplaced.is_empty() {
                hint.push_str(&format!(
                    "\nnote: `{}` belongs in the package-level `[dependencies]` table, \
                     which says where a dependency comes from; \
                     `[targets.{}.deps]` only says how this target consumes it",
                    misplaced.join("`, `"),
                    target_name
                ));
            }

            bail!(
                "unknown key(s) in `[targets.{}.deps]` entry `{}`: {}\n{}",
                target_name,
                dep_name,
                unexpected.join(", "),
                hint
            );
        }

        for (field, value) in [("compile", &self.compile), ("link", &self.link)] {
            if let Some(value) = value {
                if value != "public" && value != "private" {
                    bail!(
                        "`[targets.{}.deps]` entry `{}` sets `{} = \"{}\"`, which is not a \
                         visibility\n\
                         hint: write `\"public\"` or `\"private\"` (lowercase); anything \
                         else used to be silently treated as `\"public\"`, which is the \
                         wrong direction to guess -- it exports the dependency's include \
                         dirs and defines to everything that depends on `{}`",
                        target_name,
                        dep_name,
                        field,
                        value,
                        target_name
                    );
                }
            }
        }

        Ok(())
    }
}

/// Format a TOML parse error with context showing the offending line.
fn format_toml_error(err: toml::de::Error, content: &str, path: &Path) -> anyhow::Error {
    let message = err.message();

    // Try to extract span information
    if let Some(span) = err.span() {
        // Convert byte offset to line/column
        let (line_num, col) = byte_offset_to_line_col(content, span.start);
        let lines: Vec<&str> = content.lines().collect();

        let mut error_msg = format!(
            "failed to parse {}\n --> {}:{}:{}\n",
            path.display(),
            path.display(),
            line_num,
            col
        );

        // Show context: line before, error line, line after
        let start_line = line_num.saturating_sub(2);
        let end_line = (line_num + 1).min(lines.len());

        // Calculate gutter width
        let gutter_width = format!("{}", end_line).len();

        error_msg.push_str(&format!("{:width$} |\n", "", width = gutter_width));

        for i in start_line..end_line {
            let line_content = lines.get(i).unwrap_or(&"");
            let current_line = i + 1;

            if current_line == line_num {
                // Error line with pointer
                error_msg.push_str(&format!(
                    "{:>width$} | {}\n",
                    current_line,
                    line_content,
                    width = gutter_width
                ));
                // Add pointer to error column
                error_msg.push_str(&format!(
                    "{:width$} | {:>col$}^\n",
                    "",
                    "",
                    width = gutter_width,
                    col = col.saturating_sub(1)
                ));
            } else {
                error_msg.push_str(&format!(
                    "{:>width$} | {}\n",
                    current_line,
                    line_content,
                    width = gutter_width
                ));
            }
        }

        error_msg.push_str(&format!("{:width$} |\n", "", width = gutter_width));
        error_msg.push_str(&format!(" = {}", message));

        anyhow::anyhow!("{}", error_msg)
    } else {
        // No span available, fall back to simple message
        anyhow::anyhow!("failed to parse {}: {}", path.display(), message)
    }
}

/// Convert a byte offset into (line_number, column) tuple.
/// Line numbers are 1-indexed.
fn byte_offset_to_line_col(content: &str, offset: usize) -> (usize, usize) {
    let mut line = 1;
    let mut col = 1;

    for (i, ch) in content.char_indices() {
        if i >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }

    (line, col)
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
            toml::from_str(content).map_err(|e| format_toml_error(e, content, path))?;

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
            // Name the manifest. Every manifest in the graph is parsed here,
            // dependencies included, so a target-level error with no file in
            // it leaves the user guessing which package it came from -- and
            // for a dependency, whether they can even edit it.
            targets.push(
                Self::convert_target(name, raw_target)
                    .with_context(|| format!("in {}", path.display()))?,
            );
        }

        // If no targets defined and we have a package, create a default one based on package name
        if targets.is_empty() {
            if let Some(ref pkg) = raw.package {
                targets.push(Target::new(pkg.name.clone(), TargetKind::StaticLib));
            }
            // Virtual workspaces (workspace without package) have no default targets
        }

        // `[package] default_target` must name a target that exists. A
        // silent fallback to the positional rule here would be the worst
        // of both worlds: the manifest says one thing, the build does
        // another, and the only symptom is a link error in a *consumer*
        // pointing at the wrong archive.
        if let Some(ref pkg) = raw.package {
            if let Some(ref wanted) = pkg.default_target {
                if !targets.iter().any(|t| t.name.as_str() == wanted) {
                    let available: Vec<&str> = targets.iter().map(|t| t.name.as_str()).collect();
                    anyhow::bail!(
                        "{}: `[package] default_target = \"{wanted}\"` names a target that \
                         does not exist. Declared targets: {}",
                        path.display(),
                        if available.is_empty() {
                            "(none)".to_string()
                        } else {
                            available.join(", ")
                        }
                    );
                }
            }
        }

        // Only `debug` and `release` can ever be selected: `Manifest::profiles`
        // is read through `debug_profile()`/`release_profile()`, chosen by
        // `Workspace::is_release()`, and there is no `--profile` flag for
        // anything else to reach. `[profile.asan]` therefore parsed, passed
        // `deny_unknown_fields`, and was discarded -- including a
        // dependency's, which is worse, because its author cannot see the
        // consumer's build.
        for name in raw.profile.keys() {
            if name != "debug" && name != "release" {
                anyhow::bail!(
                    "{}: `[profile.{name}]` is not implemented\n\
                     hint: only `debug` and `release` can be selected -- \
                     `harbour build` offers `--release` and no `--profile`, so \
                     a profile under any other name parses and is then \
                     discarded. Put these settings in `[profile.debug]` or \
                     `[profile.release]`, or pass the flags directly \
                     (`cflags`/`ldflags` in a profile, or \
                     `[targets.NAME.private] cflags`).\n\
                     tracking: https://github.com/aryamurray/harbour/issues/106",
                    path.display()
                );
            }
        }

        // `optional = true` is refused for the same reason. Nothing in the
        // resolver, the lockfile or the builder reads it: the dependency is
        // resolved, fetched, built and linked exactly as if the key were
        // absent, and unlike Cargo it does not implicitly define a feature.
        for (name, spec) in raw.dependencies.iter() {
            spec.validate_implemented(name)
                .with_context(|| format!("in {}", path.display()))?;
        }
        if let Some(ref ws) = raw.workspace {
            for (name, spec) in ws.dependencies.iter() {
                spec.validate_implemented(name)
                    .with_context(|| format!("in {}", path.display()))?;
            }
        }

        Ok(Manifest {
            package: raw.package,
            workspace: raw.workspace,
            dependencies: raw.dependencies,
            targets,
            profiles: raw.profile,
            build: raw.build,
            features: raw.features,
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

    /// Parse a `[targets.X.recipe]` table, rejecting keys it does not use.
    ///
    /// Serde cannot do this one: `deny_unknown_fields` is ignored on an
    /// internally tagged enum, so `type = "cmake"` with `optinos = [...]`
    /// parsed and ran cmake with no options -- a successful build of
    /// something the manifest did not describe.
    ///
    /// The accepted key set is *derived* from the enum rather than listed
    /// here: each of `BuildRecipe::samples()` is serialized back to a table
    /// and its keys collected. A hand-written list next to a struct is the
    /// shape that drifts, and this file already carries three comments about
    /// exactly that.
    fn parse_recipe(target_name: &str, value: toml::Value) -> Result<BuildRecipe> {
        let recipe: BuildRecipe = value
            .clone()
            .try_into()
            .with_context(|| format!("target `{target_name}`: invalid `recipe`"))?;

        let table = match value {
            toml::Value::Table(table) => table,
            // `try_into` above would have failed already; a recipe is always
            // a table.
            _ => return Ok(recipe),
        };

        let sample = BuildRecipe::samples()
            .into_iter()
            .find(|s| s.type_name() == recipe.type_name())
            .expect("every variant has a sample");
        let accepted: Vec<String> = match toml::Value::try_from(&sample) {
            Ok(toml::Value::Table(t)) => t.keys().cloned().collect(),
            // Serializing a sample cannot fail, but a panic here would be a
            // worse outcome than skipping the check.
            _ => return Ok(recipe),
        };

        let unknown: Vec<&str> = table
            .keys()
            .map(|k| k.as_str())
            .filter(|k| !accepted.iter().any(|a| a == k))
            .collect();
        if !unknown.is_empty() {
            anyhow::bail!(
                "unknown key(s) in `[targets.{}.recipe]` for `type = \"{}\"`: {}\n\
                 hint: this recipe takes {}",
                target_name,
                recipe.type_name(),
                unknown.join(", "),
                if accepted.len() == 1 {
                    "no keys other than `type`".to_string()
                } else {
                    accepted.join(", ")
                }
            );
        }

        Ok(recipe)
    }

    fn convert_target(name: String, raw: RawTarget) -> Result<Target> {
        let kind = raw.kind.unwrap_or(TargetKind::StaticLib);

        // Same flatten problem as `surface.when` below, and the same manual
        // check: a target-level `when` block's conditions are flattened, so
        // serde absorbs an unrecognised key as a condition it does not know
        // rather than rejecting it. `ldflags`/`libs` written here -- which
        // read perfectly naturally next to `cflags`, but only exist on
        // `surface.when` -- were accepted and dropped in silence.
        for cond in &raw.when {
            cond.validate()
                .with_context(|| format!("target `{name}`: invalid `when` block"))?;
        }

        // Build surface from either nested format or shorthand (or both merged)
        let mut surface = if let Some(raw_surface) = raw.surface {
            // Declared-but-unimplemented link settings are a hard error, not
            // a warning. `groups` used to warn "platform support varies",
            // which reads as "this is emitted somewhere" -- it is emitted
            // nowhere -- and `kind = "package"` did not even warn. Both are
            // checked in every table that can carry them, including the
            // conditional ones: a check in only one table is how
            // `surface.when` came to accept what the unconditional table
            // rejected.
            if let Some(ref link) = raw_surface.link {
                if let Some(ref public) = link.public {
                    public.validate_implemented(&name, "surface.link.public")?;
                }
                if let Some(ref private) = link.private {
                    private.validate_implemented(&name, "surface.link.private")?;
                }
            }
            for cond in &raw_surface.conditionals {
                if let Some(ref public) = cond.link_public {
                    public.validate_implemented(&name, "surface.when.\"link.public\"")?;
                }
                if let Some(ref private) = cond.link_private {
                    private.validate_implemented(&name, "surface.when.\"link.private\"")?;
                }
            }

            // A `when` block's condition fields are flattened, so serde
            // cannot reject an unrecognised key -- it absorbs it as a
            // condition it does not know. Checking by hand is what stops a
            // misspelled or misplaced table from parsing cleanly and doing
            // nothing, which is how the scaffold's own `-Wall -Wextra` went
            // unapplied in every generated project.
            for cond in &raw_surface.conditionals {
                cond.validate()
                    .with_context(|| format!("target `{name}`: invalid `surface.when` block"))?;
            }

            tracing::debug!(
                "target `{}`: {} conditional surface entries will be applied",
                name,
                raw_surface.conditionals.len()
            );

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
                    requires_cpp: raw_surface.compile.as_ref().and_then(|c| c.requires_cpp),
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

        // Merge shorthand [targets.X.public] into surface
        if let Some(ref public_shorthand) = raw.public {
            if !public_shorthand.is_empty() {
                let compile_reqs = public_shorthand.to_compile_requirements();
                let link_reqs = public_shorthand.to_link_requirements();
                link_reqs.validate_implemented(&name, &format!("targets.{name}.public"))?;

                // Merge compile requirements
                surface
                    .compile
                    .public
                    .include_dirs
                    .extend(compile_reqs.include_dirs);
                surface.compile.public.defines.extend(compile_reqs.defines);
                surface.compile.public.cflags.extend(compile_reqs.cflags);

                // Merge link requirements
                surface.link.public.libs.extend(link_reqs.libs);
                surface.link.public.ldflags.extend(link_reqs.ldflags);
            }
        }

        // Merge shorthand [targets.X.private] into surface
        if let Some(ref private_shorthand) = raw.private {
            if !private_shorthand.is_empty() {
                let compile_reqs = private_shorthand.to_compile_requirements();
                let link_reqs = private_shorthand.to_link_requirements();
                link_reqs.validate_implemented(&name, &format!("targets.{name}.private"))?;

                // Merge compile requirements
                surface
                    .compile
                    .private
                    .include_dirs
                    .extend(compile_reqs.include_dirs);
                surface.compile.private.defines.extend(compile_reqs.defines);
                surface.compile.private.cflags.extend(compile_reqs.cflags);

                // Merge link requirements
                surface.link.private.libs.extend(link_reqs.libs);
                surface.link.private.ldflags.extend(link_reqs.ldflags);
            }
        }

        let deps = if let Some(raw_deps) = raw.deps {
            raw_deps
                .into_iter()
                .map(|(pkg, dep)| {
                    let spec = Self::convert_target_dep(dep, &name, &pkg)?;
                    Ok((InternedString::new(&pkg), spec))
                })
                .collect::<Result<DeclOrderMap<_, _>>>()?
        } else {
            DeclOrderMap::new()
        };

        // Validate backend config if present
        // `[targets.NAME.backend]` is refused rather than validated-then-
        // ignored. `RawBackendConfig::validate` rejecting an unknown backend
        // id is exactly what made this table look live: nothing reads
        // `Target.backend`. `harbour build` takes its backend from
        // `opts.backend` (the `--backend` flag / `.harbour/config.toml`) and
        // dispatches per target on `recipe`, so `backend = "cmake"` built
        // natively and said `Finished debug [native]`.
        if let Some(ref backend) = raw.backend {
            anyhow::bail!(
                "target `{name}`: `[targets.{name}.backend]` is not implemented\n\
                 hint: this table parses -- including validating the backend \
                 name, which is why it looks as though it works -- and is then \
                 read by nothing, so the target is still built natively. To \
                 build this target with another build system use \
                 `[targets.{name}.recipe]`, which does dispatch:\n\
                 \n    \
                 [targets.{name}.recipe]\n    \
                 type = \"{}\"\n\
                 \n\
                 To choose the backend for a whole build instead, pass \
                 `--backend`.\n\
                 tracking: https://github.com/aryamurray/harbour/issues/107",
                backend.backend.as_deref().unwrap_or("cmake")
            );
        }
        let backend = None;

        // Apply default source patterns if not specified (except for header-only)
        let sources = if raw.sources.is_empty() && kind != TargetKind::HeaderOnly {
            match raw.lang {
                Language::Cxx => vec![
                    "src/**/*.cpp".to_string(),
                    "src/**/*.cc".to_string(),
                    "src/**/*.cxx".to_string(),
                ],
                Language::C => vec!["src/**/*.c".to_string()],
                // Only ever reached if a manifest sets `lang = "asm"`
                // explicitly; assembly is normally mixed into a C or C++
                // target and dispatched per file.
                Language::Asm => vec!["src/**/*.S".to_string(), "src/**/*.s".to_string()],
            }
        } else {
            raw.sources
        };

        // Desugared here, in the parser, so that the bulk `check_*` lists
        // and the explicitly named probes become one representation before
        // anything downstream sees them. A second consumer of "a probe" is
        // exactly the shape of all ten defects in the 2026-09-07 audit.
        let probes = match raw.probes {
            Some(raw_probes) => raw_probes.into_probe_set(&name)?,
            None => ProbeSet::default(),
        };

        // The `ffi` table's one live field is `header_files`; the other nine
        // parsed and reached nothing. Rejected here rather than in the `ffi`
        // command, so `harbour build` says it too -- a manifest author does
        // not necessarily run `harbour ffi` at all.
        if let Some(ref ffi) = raw.ffi {
            ffi.validate_implemented(&name)?;
        }

        let recipe = match raw.recipe {
            Some(value) => Some(Self::parse_recipe(&name, value)?),
            None => None,
        };

        let target = Target {
            exclude: raw.exclude.clone(),
            name: InternedString::new(name),
            kind,
            sources,
            when: raw.when,
            prebuild: raw.prebuild,
            probes,
            public_headers: raw.public_headers,
            surface,
            deps,
            recipe,
            lang: raw.lang,
            c_std: raw.c_std,
            cpp_std: raw.cpp_std,
            backend,
            ffi: raw.ffi,
            freestanding: raw.freestanding,
            linker_script: raw.linker_script,
            entry: raw.entry,
        };

        // Validate target configuration
        target.validate()?;

        Ok(target)
    }

    fn convert_target_dep(
        raw: RawTargetDep,
        target_name: &str,
        dep_name: &str,
    ) -> Result<TargetDepSpec> {
        use crate::core::target::Visibility;

        // Only ever called on a value `validate` has already accepted, so
        // the `else` arm is unreachable rather than a silent default.
        fn visibility(value: Option<String>) -> Visibility {
            match value.as_deref() {
                Some("private") => Visibility::Private,
                _ => Visibility::Public,
            }
        }

        match raw {
            RawTargetDep::Simple(target) => Ok(TargetDepSpec {
                target: Some(target),
                compile: Visibility::Public,
                link: Visibility::Public,
            }),
            RawTargetDep::Detailed(detailed) => {
                detailed.validate(target_name, dep_name)?;
                Ok(TargetDepSpec {
                    target: detailed.target,
                    compile: visibility(detailed.compile),
                    link: visibility(detailed.link),
                })
            }
        }
    }

    /// Get the package name (panics if this is a virtual workspace).
    ///
    /// # Panics
    /// Panics if this manifest has no `[package]` section (virtual workspace).
    /// Use `try_name()` or `package_name()` for a non-panicking alternative.
    pub fn name(&self) -> &str {
        self.try_name().expect(
            "called name() on virtual workspace manifest - \
             use try_name() or check manifest.package.is_some() first",
        )
    }

    /// Get the package name if this manifest has a package section.
    ///
    /// Returns `None` for virtual workspace manifests.
    pub fn try_name(&self) -> Option<&str> {
        self.package.as_ref().map(|p| p.name.as_str())
    }

    /// Alias for `try_name()` for backwards compatibility.
    pub fn package_name(&self) -> Option<&str> {
        self.try_name()
    }

    /// Get the package version (panics if this is a virtual workspace).
    ///
    /// # Panics
    /// Panics if this manifest has no `[package]` section (virtual workspace).
    /// Use `try_version()` or `package_version()` for a non-panicking alternative.
    pub fn version(&self) -> Result<Version> {
        self.try_version().expect(
            "called version() on virtual workspace manifest - \
             use try_version() or check manifest.package.is_some() first",
        )
    }

    /// Get the package version if this manifest has a package section.
    ///
    /// Returns `None` for virtual workspace manifests.
    pub fn try_version(&self) -> Option<Result<Version>> {
        self.package.as_ref().map(|p| p.version())
    }

    /// Alias for `try_version()` for backwards compatibility.
    pub fn package_version(&self) -> Option<Result<Version>> {
        self.try_version()
    }

    /// Get a target by name.
    pub fn target(&self, name: &str) -> Option<&Target> {
        self.targets.iter().find(|t| t.name.as_str() == name)
    }

    /// The target a dependent gets when it does not name one.
    ///
    /// `[package] default_target` if set, else the positional rule: the
    /// first declared library target, else the first declared target.
    ///
    /// `Manifest::parse` has already rejected a `default_target` naming a
    /// target that does not exist, so the `find` below cannot miss for a
    /// manifest that came through `parse`. It still falls through to the
    /// positional rule rather than panicking, for `Manifest` values
    /// constructed directly in tests and by the registry shim.
    pub fn default_target(&self) -> Option<&Target> {
        if let Some(name) = self.explicit_default_target_name() {
            if let Some(t) = self.targets.iter().find(|t| t.name.as_str() == name) {
                return Some(t);
            }
        }

        self.targets
            .iter()
            .find(|t| t.kind.is_library())
            .or_else(|| self.targets.first())
    }

    /// The name from `[package] default_target`, if the author set one.
    ///
    /// Lets callers distinguish "the author chose this target" from "the
    /// positional rule happened to land here" -- the multi-library warning
    /// needs that distinction so it does not nag about an ambiguity the
    /// author has already resolved.
    pub fn explicit_default_target_name(&self) -> Option<&str> {
        self.package
            .as_ref()
            .and_then(|p| p.default_target.as_deref())
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
    let sources = if is_lib { "src/**/*.c" } else { "src/main.c" };

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

[[targets.{name}.surface.when]]
compiler = "msvc"
[targets.{name}.surface.when."compile.private"]
cflags = ["/W4"]

[[targets.{name}.surface.when]]
compiler = "gcc"
[targets.{name}.surface.when."compile.private"]
cflags = ["-Wall", "-Wextra"]

[[targets.{name}.surface.when]]
compiler = "clang"
[targets.{name}.surface.when."compile.private"]
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

[[targets.{name}.surface.when]]
compiler = "msvc"
[targets.{name}.surface.when."compile.private"]
cflags = ["/W4"]

[[targets.{name}.surface.when]]
compiler = "gcc"
[targets.{name}.surface.when."compile.private"]
cflags = ["-Wall", "-Wextra"]

[[targets.{name}.surface.when]]
compiler = "clang"
[targets.{name}.surface.when."compile.private"]
cflags = ["-Wall", "-Wextra"]
"#
    )
}

#[cfg(test)]
mod tests {
    use crate::core::target::TargetTriple;

    /// Freestanding code runs anywhere; hosted code needs an OS to host it.
    /// This is the one target property C actually guarantees (C §4), which is
    /// why it is the only one enforced rather than warned about.
    #[test]
    fn hosted_requirement_rejects_only_bare_metal_targets() {
        let hosted = super::TargetEnvironment::Hosted;
        let freestanding = super::TargetEnvironment::Freestanding;

        for t in [
            "x86_64-unknown-linux-gnu",
            "aarch64-apple-darwin",
            "x86_64-pc-windows-msvc",
            "aarch64-unknown-linux-musl",
        ] {
            let triple = TargetTriple::parse(t);
            assert!(hosted.is_satisfied_by(&triple), "{t} is hosted");
            assert!(
                freestanding.is_satisfied_by(&triple),
                "{t} accepts freestanding"
            );
        }

        for t in ["thumbv7em-none-eabi", "riscv32imac-unknown-none-elf"] {
            let triple = TargetTriple::parse(t);
            assert!(!hosted.is_satisfied_by(&triple), "{t} has no libc");
            assert!(
                freestanding.is_satisfied_by(&triple),
                "{t} is exactly what freestanding is for"
            );
        }
    }

    #[test]
    fn supports_patterns_match_on_triple_shape() {
        use super::triple_matches_pattern as m;

        assert!(m("*-*-linux-gnu", "x86_64-unknown-linux-gnu"));
        assert!(m("*-*-linux-gnu", "aarch64-unknown-linux-gnu"));
        assert!(m("*-apple-darwin", "aarch64-apple-darwin"));
        assert!(m("x86_64-pc-windows-msvc", "x86_64-pc-windows-msvc"));
        assert!(m("*", "anything-at-all"));

        // A trailing literal has to reach the end: gnu must not match gnueabihf.
        assert!(!m("*-*-linux-gnu", "armv7-unknown-linux-gnueabihf"));
        // A leading literal has to match at the start.
        assert!(!m("x86_64-*", "aarch64-apple-darwin"));
        assert!(!m("*-*-linux-musl", "x86_64-unknown-linux-gnu"));
    }

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

    /// `[build] exceptions`/`rtti` are documented to default to `true`, and
    /// they did -- but only for a manifest that actually had a `[build]`
    /// table. serde's per-field `default = "..."` fills a *missing key in a
    /// present table*; a missing table falls to `BuildConfig::default()`,
    /// which was derived and so gave `false`. The result was
    /// `-fno-exceptions -fno-rtti` on every C++ package that had no reason
    /// to write `[build]` at all, so `throw` and `dynamic_cast` did not
    /// compile until an empty-ish `[build]` section was added.
    #[test]
    fn exceptions_and_rtti_default_true_with_and_without_a_build_section() {
        let parse = |body: &str| {
            let content = format!(
                "[package]\nname = \"t\"\nversion = \"1.0.0\"\n\n\
                 [targets.t]\nkind = \"exe\"\nlang = \"c++\"\n{body}"
            );
            Manifest::parse(&content, Path::new("Harbour.toml")).unwrap()
        };

        // No `[build]` table: the path that was broken.
        let no_section = parse("");
        assert!(
            no_section.build.exceptions,
            "a manifest with no [build] section must still get exceptions"
        );
        assert!(
            no_section.build.rtti,
            "a manifest with no [build] section must still get RTTI"
        );

        // A `[build]` table that does not mention them: already worked, and
        // must keep agreeing with the case above.
        let other_keys = parse("\n[build]\ncpp_std = \"17\"\n");
        assert!(other_keys.build.exceptions);
        assert!(other_keys.build.rtti);

        // Turning them off explicitly still works.
        let off = parse("\n[build]\nexceptions = false\nrtti = false\n");
        assert!(!off.build.exceptions);
        assert!(!off.build.rtti);

        // `Default` and the serde defaults must not be able to drift apart.
        let d = BuildConfig::default();
        assert_eq!(d.exceptions, other_keys.build.exceptions);
        assert_eq!(d.rtti, other_keys.build.rtti);
    }

    /// A target-level `[[targets.X.when]]` block flattens its conditions,
    /// so serde absorbed anything it did not recognise as a condition it had
    /// not been taught about -- accepting it and dropping it in silence.
    /// `surface.when` was hardened against exactly this after its
    /// `compile.private` table turned out to have never reached a compiler;
    /// the target-level block was left open.
    ///
    /// The sharp case is not a typo. `ldflags` and `libs` read perfectly
    /// naturally next to `cflags`, but they only exist on `surface.when`, so
    /// a manifest declaring a per-platform linker flag in the block it
    /// already uses for per-platform sources got no flag and no complaint.
    #[test]
    fn unknown_keys_in_a_target_level_when_block_are_rejected() {
        let manifest = |body: &str| {
            let content = format!(
                "[package]\nname = \"t\"\nversion = \"1.0.0\"\n\n\
                 [targets.t]\nkind = \"staticlib\"\nsources = [\"src/a.c\"]\n\n\
                 [[targets.t.when]]\nos = \"linux\"\n{body}"
            );
            Manifest::parse(&content, Path::new("Harbour.toml"))
        };

        // A misspelling.
        let err = manifest("cflagz = [\"-Wall\"]\n")
            .expect_err("a key that no `when` block has must not parse");
        let err = format!("{err:#}");
        assert!(err.contains("cflagz"), "must name the offending key: {err}");

        // A key that exists, but on the other `when` block. The error has to
        // say where it lives, or the fix is a guess.
        let err = manifest("ldflags = [\"-fuse-ld=lld\"]\n")
            .expect_err("`ldflags` is not a target-level `when` key");
        let err = format!("{err:#}");
        assert!(
            err.contains("ldflags") && err.contains("link.private"),
            "must point at `surface.when`'s `link.private`: {err}"
        );

        // Every key the block really does take still parses, alongside a
        // condition, so the catch-all has not swallowed the schema.
        let ok = manifest(
            "sources = [\"src/l.c\"]\nexclude = [\"src/x.c\"]\n\
             defines = [\"A=1\"]\ncflags = [\"-Wall\"]\n\
             include_dirs = [\"cfg/linux\"]\n\
             prebuild = [{ program = \"true\", args = [] }]\n",
        )
        .expect("the documented keys must still parse");
        let when = &ok.targets[0].when[0];
        assert_eq!(when.condition.os.as_deref(), Some("linux"));
        assert_eq!(when.sources.len(), 1);
        assert_eq!(when.exclude.len(), 1);
        assert_eq!(when.defines.len(), 1);
        assert_eq!(when.cflags.len(), 1);
        assert_eq!(when.include_dirs.len(), 1);
        assert_eq!(when.prebuild.len(), 1);
    }

    /// The `libs`-takes-a-link-name check inspected only the two
    /// unconditional link tables, so `libs = ["libssl.a"]` was rejected in
    /// `[targets.X.private]` and accepted one table deeper in
    /// `[[targets.X.surface.when]]`. Proved by running before this fix:
    /// `harbour flags t1` printed `-llibssl.a`, which makes the linker look
    /// for `liblibssl.a.a`.
    #[test]
    fn a_filename_in_libs_is_rejected_in_conditional_link_tables_too() {
        let manifest = |body: &str| {
            let content = format!(
                "[package]\nname = \"t\"\nversion = \"1.0.0\"\n\n\
                 [targets.t]\nkind = \"exe\"\nsources = [\"src/a.c\"]\n\n{body}"
            );
            Manifest::parse(&content, Path::new("Harbour.toml"))
        };

        // The two tables that were already checked, kept here so a
        // refactor cannot quietly drop them.
        for table in ["surface.link.public", "surface.link.private"] {
            let err = match manifest(&format!("[targets.t.{table}]\nlibs = [\"libssl.a\"]\n")) {
                Ok(_) => panic!("`libssl.a` must not parse in `{table}`"),
                Err(e) => format!("{e:#}"),
            };
            assert!(
                err.contains("libssl.a") && err.contains(table),
                "must name the value and the table for `{table}`: {err}"
            );
        }

        // The two that were not.
        for table in ["link.public", "link.private"] {
            let err = match manifest(&format!(
                "[[targets.t.surface.when]]\nos = \"linux\"\n\
                 [targets.t.surface.when.\"{table}\"]\nlibs = [\"libssl.a\"]\n"
            )) {
                Ok(_) => panic!("`libssl.a` must not parse in `surface.when`'s `{table}`"),
                Err(e) => format!("{e:#}"),
            };
            assert!(
                err.contains("libssl.a") && err.contains(&format!("surface.when's {table}")),
                "must name the value and the conditional table for `{table}`: {err}"
            );
        }

        // A real link name in a conditional table still parses -- the check
        // must reject filenames, not conditionals.
        let ok = manifest(
            "[[targets.t.surface.when]]\nos = \"linux\"\n\
             [targets.t.surface.when.\"link.private\"]\n\
             libs = [\"ssl\", \":libcrypto.a\", { kind = \"path\", path = \"vendor/libz.a\" }]\n",
        )
        .expect("link names, `-l:` syntax and `kind = \"path\"` must still parse");
        let cond = &ok.targets[0].surface.conditionals[0];
        assert_eq!(
            cond.link_private
                .as_ref()
                .expect("link.private survives")
                .libs
                .len(),
            3
        );
    }

    /// A mistyped key or a miscased value on a `targets.X.deps` entry used to
    /// mean `compile = "public"`, silently turning a request for a private
    /// dependency into a public one. Proved against the real build before
    /// this fix: with `compil = "private"`, `mylib`'s public
    /// `-DMYLIB_PUBLIC=1` appeared in `compile_commands.json` for the
    /// dependent's own source; with `compile = "private"` it did not.
    #[test]
    fn unknown_keys_and_miscased_visibilities_on_a_target_dep_are_rejected() {
        let manifest = |entry: &str| {
            let content = format!(
                "[package]\nname = \"app\"\nversion = \"1.0.0\"\n\n\
                 [dependencies]\nmylib = {{ path = \"../lib\" }}\n\n\
                 [targets.app]\nkind = \"exe\"\nsources = [\"src/m.c\"]\n\n\
                 [targets.app.deps]\nmylib = {{ {entry} }}\n"
            );
            Manifest::parse(&content, Path::new("Harbour.toml"))
        };

        // A misspelled key. Silently accepted before, and the value it was
        // carrying was `private`.
        let err = manifest("target = \"mylib\", compil = \"private\"")
            .expect_err("a misspelled key must not parse");
        let err = format!("{err:#}");
        assert!(
            err.contains("compil") && err.contains("app"),
            "must name the offending key and the target: {err}"
        );

        // A key that exists, but on the package-level `[dependencies]`
        // table. The error has to say where it lives, or the fix is a guess.
        let err = manifest("path = \"../lib\"").expect_err("`path` is not a target-dep key");
        let err = format!("{err:#}");
        assert!(
            err.contains("path") && err.contains("[dependencies]"),
            "must point at the package-level `[dependencies]` table: {err}"
        );

        // A miscased value. `Visibility` is lowercase everywhere else in the
        // schema, and guessing `public` for anything unrecognised is the
        // unsafe direction.
        for entry in [
            "compile = \"PRIVATE\"",
            "compile = \"Private\"",
            "link = \"PRIVATE\"",
            "compile = \"privte\"",
        ] {
            let err = match manifest(entry) {
                Ok(_) => panic!("`{entry}` must not parse"),
                Err(e) => format!("{e:#}"),
            };
            assert!(
                err.contains("public") && err.contains("private"),
                "must name the accepted values for `{entry}`: {err}"
            );
        }

        // Both spellings that do exist still parse, and still mean what they
        // say.
        let ok = manifest("target = \"second\", compile = \"private\", link = \"public\"")
            .expect("the documented keys must still parse");
        let dep = ok.targets[0]
            .deps
            .get(&InternedString::new("mylib"))
            .expect("dep survives");
        assert_eq!(dep.target.as_deref(), Some("second"));
        assert_eq!(dep.compile, crate::core::target::Visibility::Private);
        assert_eq!(dep.link, crate::core::target::Visibility::Public);

        // And the string shorthand, which never had a visibility to mistype.
        let content = "[package]\nname = \"app\"\nversion = \"1.0.0\"\n\n\
                       [targets.app]\nkind = \"exe\"\nsources = [\"src/m.c\"]\n\n\
                       [targets.app.deps]\nmylib = \"second\"\n";
        let ok = Manifest::parse(content, Path::new("Harbour.toml"))
            .expect("the string shorthand must still parse");
        let dep = ok.targets[0]
            .deps
            .get(&InternedString::new("mylib"))
            .expect("dep survives");
        assert_eq!(dep.target.as_deref(), Some("second"));
        assert_eq!(dep.compile, crate::core::target::Visibility::Public);
    }

    #[test]
    fn test_parse_manifest_with_surface_shorthand() {
        // Test the flatter [targets.X.public] and [targets.X.private] syntax
        let content = r#"
[package]
name = "mylib"
version = "1.0.0"

[targets.mylib]
kind = "staticlib"
sources = ["src/**/*.c"]
public_headers = ["include/**/*.h"]

[targets.mylib.public]
include_dirs = ["include"]
defines = ["MYLIB_API=1", "VERSION=2"]
system_libs = ["m"]

[targets.mylib.private]
include_dirs = ["src"]
cflags = ["-Wall", "-Wextra"]
defines = ["INTERNAL=1"]
"#;
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("Harbour.toml");

        let manifest = Manifest::parse(content, &path).unwrap();
        let target = &manifest.targets[0];

        // Public compile requirements
        assert_eq!(target.surface.compile.public.include_dirs.len(), 1);
        assert_eq!(
            target.surface.compile.public.include_dirs[0],
            PathBuf::from("include")
        );
        assert_eq!(target.surface.compile.public.defines.len(), 2);

        // Public link requirements (system_libs shorthand)
        assert_eq!(target.surface.link.public.libs.len(), 1);

        // Private compile requirements
        assert_eq!(target.surface.compile.private.include_dirs.len(), 1);
        assert_eq!(
            target.surface.compile.private.include_dirs[0],
            PathBuf::from("src")
        );
        assert_eq!(target.surface.compile.private.cflags.len(), 2);
        assert_eq!(target.surface.compile.private.defines.len(), 1);
    }

    #[test]
    fn test_parse_manifest_with_c_std() {
        // Test C standard selection
        let content = r#"
[package]
name = "mylib"
version = "1.0.0"

[targets.mylib]
kind = "staticlib"
sources = ["src/**/*.c"]
c_std = "11"
"#;
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("Harbour.toml");

        let manifest = Manifest::parse(content, &path).unwrap();
        let target = &manifest.targets[0];

        assert_eq!(
            target.c_std,
            Some(crate::core::target::CStandardSpec::iso(
                crate::core::target::CStandard::C11
            ))
        );
    }

    #[test]
    fn test_parse_manifest_with_c_std_variants() {
        // Test various C standard formats: 99, c99, C99, etc.
        use crate::core::target::{CStandard, CStandardSpec};
        for (input, expected) in [
            ("89", CStandardSpec::iso(CStandard::C89)),
            ("c99", CStandardSpec::iso(CStandard::C99)),
            ("17", CStandardSpec::iso(CStandard::C17)),
            ("c23", CStandardSpec::iso(CStandard::C23)),
            // The GNU dialect is a distinct request, not a spelling of the
            // ISO one: `gnu99` defines `__STDC_VERSION__` to 199901 like
            // `c99` but leaves `__STRICT_ANSI__` undefined, which is what
            // makes `typeof` and statement expressions legal.
            ("gnu89", CStandardSpec::gnu(CStandard::C89)),
            ("gnu99", CStandardSpec::gnu(CStandard::C99)),
            ("GNU11", CStandardSpec::gnu(CStandard::C11)),
            ("gnu-17", CStandardSpec::gnu(CStandard::C17)),
            ("gnu23", CStandardSpec::gnu(CStandard::C23)),
        ] {
            let content = format!(
                r#"
[package]
name = "mylib"
version = "1.0.0"

[targets.mylib]
kind = "staticlib"
sources = ["src/**/*.c"]
c_std = "{}"
"#,
                input
            );
            let tmp = TempDir::new().unwrap();
            let path = tmp.path().join("Harbour.toml");

            let manifest = Manifest::parse(&content, &path).unwrap();
            let target = &manifest.targets[0];

            assert_eq!(target.c_std, Some(expected), "failed for input: {}", input);
        }
    }

    #[test]
    fn test_parse_manifest_with_define_shorthand() {
        // Test different define formats: "FOO", "FOO=value", { name = "FOO", value = "1" }
        let content = r#"
[package]
name = "mylib"
version = "1.0.0"

[targets.mylib]
kind = "staticlib"
sources = ["src/**/*.c"]

[targets.mylib.public]
defines = [
    "FLAG_ONLY",
    "KEY=value",
    { name = "OBJECT_STYLE", value = "123" }
]
"#;
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("Harbour.toml");

        let manifest = Manifest::parse(content, &path).unwrap();
        let target = &manifest.targets[0];

        assert_eq!(target.surface.compile.public.defines.len(), 3);
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
    fn test_parse_manifest_with_features_section() {
        let content = r#"
[package]
name = "sqlike"
version = "1.0.0"

[features]
default = ["fts5"]
fts5 = []
json1 = []
full = ["fts5", "json1"]

[targets.sqlike]
kind = "staticlib"
sources = ["src/**/*.c"]

[[targets.sqlike.when]]
feature = "fts5"
defines = ["ENABLE_FTS5"]
cflags = ["-DSQLITE_ENABLE_FTS5"]
sources = ["src/fts5.c"]
"#;
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("Harbour.toml");

        let manifest = Manifest::parse(content, &path).unwrap();

        assert_eq!(manifest.features.len(), 4);
        assert_eq!(manifest.features["default"], vec!["fts5".to_string()]);
        assert_eq!(
            manifest.features["full"],
            vec!["fts5".to_string(), "json1".to_string()]
        );

        let target = &manifest.targets[0];
        assert_eq!(target.when.len(), 1);
        assert_eq!(target.when[0].condition.feature, Some("fts5".to_string()));
        assert_eq!(target.when[0].defines.len(), 1);
        assert_eq!(target.when[0].cflags.len(), 1);
        assert_eq!(target.when[0].sources, vec!["src/fts5.c".to_string()]);
    }

    #[test]
    fn test_manifest_without_features_section_has_empty_map() {
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
        assert!(manifest.features.is_empty());
    }

    #[test]
    fn test_generate_lib_manifest() {
        let manifest = generate_lib_manifest("mylib");
        assert!(manifest.contains("name = \"mylib\""));
        assert!(manifest.contains("kind = \"staticlib\""));
        assert!(manifest.contains("public_headers"));
    }

    /// The scaffold must not carry the `apple-clang` workaround.
    ///
    /// It existed because `compiler = "clang"` matched by string equality and
    /// so never fired on macOS. `PlatformCondition::compiler_matches` now
    /// treats `clang` as a family, and a workaround left in the tool's own
    /// scaffold is how the next reader learns the wrong rule -- it is what
    /// taught the audit that this bug had already caused damage.
    #[test]
    fn the_scaffold_states_each_compiler_family_once() {
        for manifest in [generate_lib_manifest("mylib"), generate_exe_manifest("app")] {
            for family in ["msvc", "gcc", "clang"] {
                assert_eq!(
                    manifest
                        .matches(&format!("compiler = \"{family}\""))
                        .count(),
                    1,
                    "the scaffold must guard `{family}` exactly once:\n{manifest}"
                );
            }
            assert!(
                !manifest.contains("compiler = \"apple-clang\""),
                "`compiler = \"clang\"` now covers Apple's clang, so the extra \
                 block is dead weight that teaches the wrong rule:\n{manifest}"
            );
        }
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

    #[test]
    fn test_toml_error_shows_line_numbers() {
        // Invalid TOML - missing closing quote
        let content = r#"
[package]
name = "mylib
version = "1.0.0"
"#;
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("Harbour.toml");

        let result = Manifest::parse(content, &path);
        assert!(result.is_err());

        let err = result.unwrap_err().to_string();
        // Should contain line/column info
        assert!(err.contains(":3:"), "error should show line 3: {}", err);
        // Should show the offending line content
        assert!(
            err.contains("name = \"mylib"),
            "error should show line content: {}",
            err
        );
    }

    /// Parse a manifest that is valid apart from `extra`, and return the
    /// error chain as one string.
    ///
    /// The whole chain, not just the top frame: the file name is attached as
    /// context in `parse`, and a `to_string()` on the outer error alone would
    /// silently stop asserting on the message that reaches the user.
    fn parse_err_with(extra: &str) -> String {
        let content = format!(
            r#"
[package]
name = "mylib"
version = "1.0.0"

[targets.mylib]
kind = "staticlib"
sources = ["src/a.c"]
{extra}
"#
        );
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("Harbour.toml");
        let err = Manifest::parse(&content, &path)
            .expect_err("this manifest declares an unimplemented setting and must be rejected");
        format!("{err:#}")
    }

    /// Nine of the ten `[targets.X.ffi]` keys reached nothing.
    ///
    /// Each is rejected by name with the flag to use instead, and the four
    /// with no flag at all (`include_functions`, `exclude_functions`,
    /// `include_types`, `exclude_types` -- binding filtering does not exist
    /// in any form) say so rather than pointing at a flag that is not there.
    #[test]
    fn unread_ffi_keys_are_rejected_with_the_flag_to_use_instead() {
        for (key, value, expect) in [
            ("languages", "[\"typescript\"]", "--lang"),
            ("bundler", "\"koffi\"", "--bundler"),
            ("output_dir", "\"bindings\"", "--output"),
            ("strip_prefix", "\"mylib_\"", "--strip-prefix"),
            ("async_wrappers", "true", "--async-wrappers"),
            ("include_functions", "[\"f\"]", "no equivalent flag"),
            ("exclude_functions", "[\"f\"]", "no equivalent flag"),
            ("include_types", "[\"T\"]", "no equivalent flag"),
            ("exclude_types", "[\"T\"]", "no equivalent flag"),
        ] {
            let err = parse_err_with(&format!("[targets.mylib.ffi]\n{key} = {value}"));
            assert!(
                err.contains(key) && err.contains("not implemented"),
                "`{key}` must be rejected by name: {err}"
            );
            assert!(
                err.contains(expect),
                "`{key}` must say what to do instead (`{expect}`): {err}"
            );
            assert!(err.contains("issues/109"), "{err}");
        }
    }

    /// `header_files` is the one key with a consumer, so it must keep
    /// working -- including alongside `public_headers`, which is what the
    /// `ffi generate` fallback chain reads when it is absent.
    #[test]
    fn the_one_live_ffi_key_still_parses() {
        let content = "[package]\nname = \"p\"\nversion = \"1.0.0\"\n\n\
                       [targets.p]\nkind = \"staticlib\"\nsources = [\"src/a.c\"]\n\
                       public_headers = [\"include/*.h\"]\n\n\
                       [targets.p.ffi]\nheader_files = [\"include/p.h\"]\n";
        let manifest = Manifest::parse(content, Path::new("Harbour.toml"))
            .expect("`header_files` is read by `harbour ffi generate`");
        let ffi = manifest.targets[0].ffi.as_ref().expect("table survives");
        assert_eq!(ffi.header_files, vec!["include/p.h".to_string()]);
    }

    /// A misspelled key on a prebuild step used to parse and vanish, so a
    /// generator's declared outputs were not the ones Harbour checked.
    #[test]
    fn a_typod_key_on_a_prebuild_step_is_rejected() {
        let err = parse_err_with(
            "[[targets.mylib.prebuild]]\n\
             program = \"perl\"\n\
             args = [\"gen.pl\"]\n\
             outpts = [\"gen.S\"]",
        );
        assert!(
            err.contains("outpts") && err.contains("outputs"),
            "the error must name the key written and the one meant: {err}"
        );
    }

    /// The keys that do exist must still be accepted -- including `inputs`,
    /// which is advisory (nothing fingerprints prebuild steps) but is not
    /// rejected: an advisory list makes no promise about the build, so it
    /// cannot mislead anyone the way `c_std` or `optional` did.
    #[test]
    fn every_real_prebuild_key_still_parses() {
        let content = "[package]\nname = \"p\"\nversion = \"1.0.0\"\n\n\
                       [targets.p]\nkind = \"staticlib\"\nsources = [\"src/a.c\"]\n\n\
                       [[targets.p.prebuild]]\n\
                       program = \"perl\"\n\
                       args = [\"gen.pl\"]\n\
                       cwd = \"scripts\"\n\
                       env = { ARCH = \"x86_64\" }\n\
                       outputs = [\"gen.S\"]\n\
                       inputs = [\"gen.pl\"]\n";
        let manifest = Manifest::parse(content, Path::new("Harbour.toml"))
            .expect("all six prebuild keys are real");
        let step = &manifest.targets[0].prebuild[0];
        assert_eq!(step.outputs.len(), 1);
        assert_eq!(step.inputs.len(), 1);
    }

    /// `deny_unknown_fields` is silently ignored on an internally tagged
    /// enum, so `[targets.X.recipe]` accepted typos: `optinos` for `options`
    /// meant cmake ran with no options and the build reported success.
    #[test]
    fn a_typod_key_in_a_recipe_is_rejected_for_every_type() {
        for (type_name, typo, rest) in [
            ("native", "optinos", ""),
            ("cmake", "arg", ""),
            ("meson", "optinos", ""),
            // `custom` needs its required key present, or deserialization
            // fails on that first and never reaches the key check.
            ("custom", "step", "steps = [{ program = \"true\" }]\n"),
        ] {
            let err = parse_err_with(&format!(
                "[targets.mylib.recipe]\ntype = \"{type_name}\"\n{rest}{typo} = []"
            ));
            assert!(
                err.contains(typo) && err.contains(type_name),
                "the error must name the key and the recipe type: {err}"
            );
        }
    }

    /// The accepted key set is derived from `BuildRecipe::samples()`, so
    /// this pins that the derivation actually covers each variant's real
    /// fields rather than accidentally accepting nothing.
    #[test]
    fn every_real_recipe_key_still_parses() {
        let bodies = [
            "type = \"native\"",
            "type = \"cmake\"\nsource_dir = \"vendor\"\nargs = [\"-DX=ON\"]\n\
             targets = [\"all\"]",
            "type = \"meson\"\nsource_dir = \"vendor\"\noptions = [\"-Dx=true\"]\n\
             targets = [\"all\"]",
            "type = \"custom\"\nsteps = [{ program = \"true\" }]",
        ];
        for body in bodies {
            let content = format!(
                "[package]\nname = \"p\"\nversion = \"1.0.0\"\n\n\
                 [targets.p]\nkind = \"staticlib\"\nsources = [\"src/a.c\"]\n\n\
                 [targets.p.recipe]\n{body}\n"
            );
            Manifest::parse(&content, Path::new("Harbour.toml"))
                .unwrap_or_else(|e| panic!("every key here is real: {e:#}\n{content}"));
        }
    }

    /// A profile under any name but `debug` or `release` is unreachable.
    ///
    /// `Manifest::profiles` is read only through `debug_profile()` and
    /// `release_profile()`, chosen by `Workspace::is_release()`, and there is
    /// no `--profile` flag. `[profile.asan]` parsed, passed
    /// `deny_unknown_fields`, and was discarded -- so a manifest asking for
    /// a sanitizer build got an ordinary one.
    #[test]
    fn a_profile_that_cannot_be_selected_is_rejected() {
        for name in ["asan", "dev", "bench", "Release"] {
            let err = parse_err_with(&format!(
                "[profile.{name}]\nopt_level = \"1\"\nsanitizers = [\"address\"]"
            ));
            assert!(
                err.contains(&format!("`[profile.{name}]`")) && err.contains("not implemented"),
                "`[profile.{name}]` must be rejected by name: {err}"
            );
            assert!(
                err.contains("issues/106"),
                "the rejection must point at the tracking issue: {err}"
            );
        }
    }

    /// The two that do work must keep working, including every key on them.
    #[test]
    fn the_two_selectable_profiles_still_parse() {
        let content = "[package]\nname = \"p\"\nversion = \"1.0.0\"\n\n\
                       [profile.debug]\nopt_level = \"0\"\ndebug = \"full\"\n\n\
                       [profile.release]\nopt_level = \"3\"\nlto = true\n\
                       cflags = [\"-DNDEBUG\"]\n";
        let manifest = Manifest::parse(content, Path::new("Harbour.toml"))
            .expect("debug and release are the profiles that exist");
        assert_eq!(manifest.profiles.len(), 2);
        assert_eq!(manifest.release_profile().lto, Some(true));
    }

    /// `[targets.X.backend]` validates its backend name and is then read by
    /// nothing: the build dispatches on `recipe`, so `backend = "cmake"`
    /// built natively and reported `[native]`.
    #[test]
    fn the_backend_table_is_rejected_and_points_at_recipe() {
        let err = parse_err_with("[targets.mylib.backend]\nbackend = \"cmake\"");
        assert!(
            err.contains("backend") && err.contains("not implemented"),
            "the table must be rejected by name: {err}"
        );
        assert!(
            err.contains("recipe"),
            "the rejection must name the table that does dispatch: {err}"
        );
        assert!(
            err.contains("issues/107"),
            "the rejection must point at the tracking issue: {err}"
        );

        // Including with no `backend` key at all: `[targets.X.backend]` with
        // only `options` was just as inert.
        let err = parse_err_with("[targets.mylib.backend]\noptions = { X = 1 }");
        assert!(err.contains("not implemented"), "{err}");
    }

    /// And the alternative the error points at has to actually parse, or the
    /// hint is worse than no hint.
    #[test]
    fn the_recipe_the_backend_rejection_suggests_parses() {
        let content = "[package]\nname = \"p\"\nversion = \"1.0.0\"\n\n\
                       [targets.p]\nkind = \"staticlib\"\nsources = [\"src/a.c\"]\n\n\
                       [targets.p.recipe]\ntype = \"cmake\"\nargs = [\"-DX=ON\"]\n";
        let manifest = Manifest::parse(content, Path::new("Harbour.toml"))
            .expect("`recipe` is the live spelling and must parse");
        assert!(manifest.targets[0].recipe.is_some());
    }

    /// `optional = true` is resolved, fetched, built and linked like any
    /// other dependency, and does not define a feature that could switch it
    /// off. Refused in both tables that take a dependency spec.
    #[test]
    fn an_optional_dependency_is_rejected_in_both_dependency_tables() {
        let package = "[package]\nname = \"p\"\nversion = \"1.0.0\"\n\n\
                       [dependencies]\nlib = { path = \"../lib\", optional = true }\n";
        let err = format!(
            "{:#}",
            Manifest::parse(package, Path::new("Harbour.toml"))
                .expect_err("`optional = true` must be rejected")
        );
        assert!(
            err.contains("optional") && err.contains("not implemented"),
            "{err}"
        );
        assert!(
            err.contains("`lib`"),
            "the error must name the dependency: {err}"
        );
        assert!(err.contains("issues/108"), "{err}");

        // A workspace's shared dependencies feed the same seeding path, so a
        // check in only one table would leave the other silent.
        let workspace = "[workspace]\nmembers = [\"a\"]\n\n\
                         [workspace.dependencies]\nlib = { path = \"../lib\", optional = true }\n";
        let err = format!(
            "{:#}",
            Manifest::parse(workspace, Path::new("Harbour.toml"))
                .expect_err("`optional = true` must be rejected in [workspace.dependencies] too")
        );
        assert!(
            err.contains("optional") && err.contains("issues/108"),
            "{err}"
        );
    }

    /// `optional = false` is the default and says nothing untrue, so it is
    /// accepted. Rejecting it would break manifests for no reason.
    #[test]
    fn optional_false_is_accepted_because_it_is_the_default() {
        let content = "[package]\nname = \"p\"\nversion = \"1.0.0\"\n\n\
                       [dependencies]\nlib = { path = \"../lib\", optional = false }\n";
        Manifest::parse(content, Path::new("Harbour.toml"))
            .expect("`optional = false` is the default and must stay accepted");
    }

    /// `groups` parses, merges, reaches `EffectiveLinkSurface.groups` and is
    /// then dropped -- no `--start-group` is ever emitted. It used to warn
    /// "platform support varies", which reads as though the flag is emitted
    /// somewhere.
    ///
    /// Declaring it is now a hard error, in every table that accepts it.
    #[test]
    fn test_link_groups_are_rejected_not_silently_dropped() {
        for table in [
            "[targets.mylib.surface.link.public]",
            "[targets.mylib.surface.link.private]",
        ] {
            let err = parse_err_with(&format!(
                "{table}\ngroups = [{{ kind = \"start_end_group\", libs = [\"a\", \"b\"] }}]"
            ));
            assert!(
                err.contains("`groups`") && err.contains("not implemented"),
                "{table} must be rejected by name: {err}"
            );
            assert!(
                err.contains("issues/95"),
                "the rejection must point at the tracking issue: {err}"
            );
        }
    }

    /// The same check has to run inside `surface.when`. A check in only the
    /// unconditional table is how `surface.when` came to accept things the
    /// unconditional table rejects.
    #[test]
    fn test_link_groups_are_rejected_inside_a_when_block() {
        let err = parse_err_with(
            "[[targets.mylib.surface.when]]\n\
             os = \"linux\"\n\
             [targets.mylib.surface.when.\"link.private\"]\n\
             groups = [{ kind = \"whole_archive\", libs = [\"a\"] }]",
        );
        assert!(
            err.contains("`groups`") && err.contains("surface.when"),
            "a `when` block's `groups` must be rejected and the block named: {err}"
        );
    }

    /// `{ kind = "package" }` emits nothing -- `to_flags` returns an empty
    /// vector -- and did not even error for a package that does not exist.
    #[test]
    fn test_package_lib_ref_is_rejected_with_an_alternative() {
        let err = parse_err_with(
            "[targets.mylib.surface.link.public]\n\
             libs = [{ kind = \"package\", name = \"nonexistent\", target = \"nope\" }]",
        );
        assert!(
            err.contains("kind = \\\"package\\\"") || err.contains("kind = \"package\""),
            "the rejection must name the offending spelling: {err}"
        );
        assert!(
            err.contains("nonexistent"),
            "the rejection must quote the offending entry: {err}"
        );
        assert!(
            err.contains("[dependencies]") && err.contains("deps"),
            "the rejection must say what to do instead: {err}"
        );
        assert!(err.contains("issues/96"), "{err}");
    }

    /// The shorthand `[targets.X.public]` table takes `libs` too, so it needs
    /// the same check -- one unchecked entry point is all it takes for the
    /// silent no-op to survive.
    #[test]
    fn test_package_lib_ref_is_rejected_in_shorthand_and_when_tables() {
        for extra in [
            "[targets.mylib.public]\n\
             libs = [{ kind = \"package\", name = \"nonexistent\", target = \"nope\" }]",
            "[targets.mylib.private]\n\
             libs = [{ kind = \"package\", name = \"nonexistent\", target = \"nope\" }]",
            "[[targets.mylib.surface.when]]\n\
             os = \"linux\"\n\
             [targets.mylib.surface.when.\"link.public\"]\n\
             libs = [{ kind = \"package\", name = \"nonexistent\", target = \"nope\" }]",
        ] {
            let err = parse_err_with(extra);
            assert!(
                err.contains("not implemented") && err.contains("nonexistent"),
                "must be rejected here too:\n{extra}\ngot: {err}"
            );
        }
    }

    /// The rejections must not fire on the spellings that do work, or every
    /// manifest in the wild breaks.
    #[test]
    fn test_working_lib_spellings_still_parse() {
        let content = r#"
[package]
name = "mylib"
version = "1.0.0"

[targets.mylib]
kind = "staticlib"
sources = ["src/a.c"]

[targets.mylib.surface.link.public]
libs = [
    "m",
    "-lpthread",
    { kind = "system", name = "dl" },
    { kind = "framework", name = "Security" },
    { kind = "path", path = "vendor/libfoo.a" },
]
"#;
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("Harbour.toml");
        let manifest = Manifest::parse(content, &path).expect("every one of these is implemented");
        assert_eq!(manifest.targets[0].surface.link.public.libs.len(), 5);
    }

    /// A dependency's manifest is parsed by the same code path, so the error
    /// has to say which file it came from -- otherwise a user hits a
    /// rejection in a package they cannot edit with no way to tell.
    #[test]
    fn test_rejection_names_the_manifest_file() {
        let content = r#"
[package]
name = "mylib"
version = "1.0.0"

[targets.mylib]
kind = "staticlib"
sources = ["src/a.c"]

[targets.mylib.surface.link.public]
groups = [{ kind = "start_end_group", libs = ["a"] }]
"#;
        let tmp = TempDir::new().unwrap();
        // `Path::join`, not a literal separator: this assertion compares
        // rendered paths and would test nothing on Windows if the separator
        // were hardcoded.
        let path = tmp.path().join("vendored").join("Harbour.toml");
        let err = format!("{:#}", Manifest::parse(content, &path).unwrap_err());
        assert!(
            err.contains(&path.display().to_string()),
            "the error must name the manifest it came from: {err}"
        );
    }

    #[test]
    fn test_toml_error_bad_value_type() {
        // Test that type errors show line info (e.g., string where array expected)
        let content = r#"
[package]
name = "mylib"
version = "1.0.0"

[targets.mylib]
kind = "staticlib"
sources = "not-an-array"
"#;
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("Harbour.toml");

        let result = Manifest::parse(content, &path);
        assert!(result.is_err());

        let err = result.unwrap_err().to_string();
        // Should have line info pointing to the error
        assert!(
            err.contains(":8:") || err.contains(":9:"),
            "error should show line info: {}",
            err
        );
    }

    #[test]
    fn test_unknown_field_rejected_in_target() {
        // Test that unknown fields produce errors (deny_unknown_fields)
        let content = r#"
[package]
name = "mylib"
version = "1.0.0"

[targets.mylib]
kind = "staticlib"
invalid_field = "oops"
"#;
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("Harbour.toml");

        let result = Manifest::parse(content, &path);
        assert!(result.is_err(), "should reject unknown field");

        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("invalid_field") || err.contains("unknown field"),
            "error should mention invalid_field: {}",
            err
        );
    }

    #[test]
    fn test_unknown_field_rejected_in_package() {
        let content = r#"
[package]
name = "mylib"
version = "1.0.0"
typo_field = "bad"
"#;
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("Harbour.toml");

        let result = Manifest::parse(content, &path);
        assert!(result.is_err(), "should reject unknown field in package");
    }

    #[test]
    fn test_default_source_patterns_c() {
        // When no sources specified, default to src/**/*.c for C
        let content = r#"
[package]
name = "mylib"
version = "1.0.0"

[targets.mylib]
kind = "staticlib"
"#;
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("Harbour.toml");

        let manifest = Manifest::parse(content, &path).unwrap();
        let target = &manifest.targets[0];

        assert_eq!(target.sources, vec!["src/**/*.c"]);
    }

    #[test]
    fn test_default_source_patterns_cpp() {
        // When no sources specified and lang=c++, default to C++ patterns
        let content = r#"
[package]
name = "mylib"
version = "1.0.0"

[targets.mylib]
kind = "staticlib"
lang = "c++"
"#;
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("Harbour.toml");

        let manifest = Manifest::parse(content, &path).unwrap();
        let target = &manifest.targets[0];

        assert_eq!(
            target.sources,
            vec!["src/**/*.cpp", "src/**/*.cc", "src/**/*.cxx"]
        );
    }

    #[test]
    fn test_no_default_source_for_header_only() {
        // Header-only targets should NOT get default sources
        let content = r#"
[package]
name = "mylib"
version = "1.0.0"

[targets.mylib]
kind = "header-only"
"#;
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("Harbour.toml");

        let manifest = Manifest::parse(content, &path).unwrap();
        let target = &manifest.targets[0];

        assert!(target.sources.is_empty());
    }

    #[test]
    fn test_explicit_sources_override_defaults() {
        // When sources are explicitly specified, don't apply defaults
        let content = r#"
[package]
name = "mylib"
version = "1.0.0"

[targets.mylib]
kind = "staticlib"
sources = ["lib/**/*.c"]
"#;
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("Harbour.toml");

        let manifest = Manifest::parse(content, &path).unwrap();
        let target = &manifest.targets[0];

        assert_eq!(target.sources, vec!["lib/**/*.c"]);
    }

    /// Declaration order of `[targets.*]`, `[dependencies]` and
    /// `[targets.X.deps]` is observable in the build output, so these three
    /// sections are `DeclOrderMap`, not `HashMap`.
    ///
    /// Each case uses a long, deliberately non-alphabetical key list. That
    /// is the point: Rust randomises `HashMap` iteration per process, so a
    /// two-key fixture would have passed roughly half the time on the old
    /// code and proved nothing. With twelve keys, the probability that a
    /// `HashMap` happens to yield declaration order is 1/12! -- under one
    /// in four hundred million -- so a single run is a real assertion.
    const ORDERED_KEYS: [&str; 12] = [
        "zulu", "alpha", "mike", "bravo", "yankee", "charlie", "november", "delta", "xray", "echo",
        "oscar", "foxtrot",
    ];

    #[test]
    fn targets_iterate_in_declaration_order() {
        let mut content = String::from("[package]\nname = \"p\"\nversion = \"1.0.0\"\n");
        for k in ORDERED_KEYS {
            content.push_str(&format!(
                "\n[targets.{k}]\nkind = \"staticlib\"\nsources = [\"src/{k}.c\"]\n"
            ));
        }

        let tmp = TempDir::new().unwrap();
        let manifest = Manifest::parse(&content, &tmp.path().join("Harbour.toml")).unwrap();

        let names: Vec<&str> = manifest.targets.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ORDERED_KEYS.to_vec());
    }

    /// The bug this exists to prevent: a package with two library targets
    /// contributed a randomly chosen one to its dependents, so the same
    /// manifest produced `-DFROM_FIRST_TARGET=1` on some runs and
    /// `-DFROM_SECOND_TARGET=1` on others.
    #[test]
    fn default_target_is_the_first_declared_library() {
        let mut content = String::from("[package]\nname = \"p\"\nversion = \"1.0.0\"\n");
        // An executable first, so "first library" and "first target"
        // disagree and the test pins the library rule specifically.
        content.push_str("\n[targets.tool]\nkind = \"exe\"\nsources = [\"src/tool.c\"]\n");
        for k in ORDERED_KEYS {
            content.push_str(&format!(
                "\n[targets.{k}]\nkind = \"staticlib\"\nsources = [\"src/{k}.c\"]\n"
            ));
        }

        let tmp = TempDir::new().unwrap();
        let manifest = Manifest::parse(&content, &tmp.path().join("Harbour.toml")).unwrap();

        assert_eq!(manifest.default_target().unwrap().name.as_str(), "zulu");
    }

    /// No library at all: the rule falls through to the first *declared*
    /// target, which is only well-defined because the map is ordered.
    #[test]
    fn default_target_falls_back_to_first_declared_target() {
        let mut content = String::from("[package]\nname = \"p\"\nversion = \"1.0.0\"\n");
        for k in ORDERED_KEYS {
            content.push_str(&format!(
                "\n[targets.{k}]\nkind = \"exe\"\nsources = [\"src/{k}.c\"]\n"
            ));
        }

        let tmp = TempDir::new().unwrap();
        let manifest = Manifest::parse(&content, &tmp.path().join("Harbour.toml")).unwrap();

        assert_eq!(manifest.default_target().unwrap().name.as_str(), "zulu");
    }

    /// `[dependencies]` order fixes the solver's seeding order, which fixes
    /// the resolve graph's node order, which fixes static-archive link
    /// order. Four sibling libraries came out in a different order on
    /// nearly every run before this.
    #[test]
    fn dependencies_iterate_in_declaration_order() {
        let mut content =
            String::from("[package]\nname = \"p\"\nversion = \"1.0.0\"\n\n[dependencies]\n");
        for k in ORDERED_KEYS {
            content.push_str(&format!("{k} = {{ path = \"../{k}\" }}\n"));
        }

        let tmp = TempDir::new().unwrap();
        let manifest = Manifest::parse(&content, &tmp.path().join("Harbour.toml")).unwrap();

        let names: Vec<&str> = manifest.dependencies.keys().map(String::as_str).collect();
        assert_eq!(names, ORDERED_KEYS.to_vec());
    }

    #[test]
    fn target_deps_iterate_in_declaration_order() {
        let mut content =
            String::from("[package]\nname = \"p\"\nversion = \"1.0.0\"\n\n[dependencies]\n");
        for k in ORDERED_KEYS {
            content.push_str(&format!("{k} = {{ path = \"../{k}\" }}\n"));
        }
        content.push_str(
            "\n[targets.app]\nkind = \"exe\"\nsources = [\"src/m.c\"]\n\n[targets.app.deps]\n",
        );
        for k in ORDERED_KEYS {
            content.push_str(&format!("{k} = \"{k}\"\n"));
        }

        let tmp = TempDir::new().unwrap();
        let manifest = Manifest::parse(&content, &tmp.path().join("Harbour.toml")).unwrap();

        let names: Vec<&str> = manifest.targets[0]
            .deps
            .keys()
            .map(|k| k.as_str())
            .collect();
        assert_eq!(names, ORDERED_KEYS.to_vec());
    }

    /// `[workspace.dependencies]` feeds the same seeding path as
    /// `[dependencies]`, so it carries the same guarantee.
    #[test]
    fn workspace_dependencies_iterate_in_declaration_order() {
        let mut content =
            String::from("[workspace]\nmembers = [\"a\"]\n\n[workspace.dependencies]\n");
        for k in ORDERED_KEYS {
            content.push_str(&format!("{k} = {{ path = \"../{k}\" }}\n"));
        }

        let tmp = TempDir::new().unwrap();
        let manifest = Manifest::parse(&content, &tmp.path().join("Harbour.toml")).unwrap();

        let ws = manifest.workspace.as_ref().unwrap();
        let names: Vec<&str> = ws.dependencies.keys().map(String::as_str).collect();
        assert_eq!(names, ORDERED_KEYS.to_vec());
    }

    /// `[package] default_target` overrides the positional rule.
    #[test]
    fn explicit_default_target_wins_over_the_positional_rule() {
        let content = r#"
[package]
name = "mylib"
version = "1.0.0"
default_target = "second"

[targets.mylib]
kind = "staticlib"
sources = ["src/first.c"]

[targets.second]
kind = "staticlib"
sources = ["src/second.c"]
"#;
        let tmp = TempDir::new().unwrap();
        let manifest = Manifest::parse(content, &tmp.path().join("Harbour.toml")).unwrap();

        assert_eq!(manifest.default_target().unwrap().name.as_str(), "second");
        assert_eq!(manifest.explicit_default_target_name(), Some("second"));
    }

    /// It can also name a target the positional rule would never reach,
    /// which is the point of having it at all.
    #[test]
    fn explicit_default_target_may_name_a_non_library() {
        let content = r#"
[package]
name = "mylib"
version = "1.0.0"
default_target = "tool"

[targets.mylib]
kind = "staticlib"
sources = ["src/lib.c"]

[targets.tool]
kind = "exe"
sources = ["src/tool.c"]
"#;
        let tmp = TempDir::new().unwrap();
        let manifest = Manifest::parse(content, &tmp.path().join("Harbour.toml")).unwrap();

        assert_eq!(manifest.default_target().unwrap().name.as_str(), "tool");
    }

    /// Absent means the positional rule, unchanged -- existing
    /// multi-library manifests keep working without edits.
    #[test]
    fn absent_default_target_leaves_the_positional_rule_alone() {
        let content = r#"
[package]
name = "mylib"
version = "1.0.0"

[targets.mylib]
kind = "staticlib"
sources = ["src/first.c"]

[targets.second]
kind = "staticlib"
sources = ["src/second.c"]
"#;
        let tmp = TempDir::new().unwrap();
        let manifest = Manifest::parse(content, &tmp.path().join("Harbour.toml")).unwrap();

        assert_eq!(manifest.default_target().unwrap().name.as_str(), "mylib");
        assert_eq!(manifest.explicit_default_target_name(), None);
    }

    /// A typo is a hard error at parse time. Silently falling back to the
    /// positional rule is exactly the class of bug this key exists to end.
    #[test]
    fn default_target_naming_a_missing_target_is_an_error() {
        let content = r#"
[package]
name = "mylib"
version = "1.0.0"
default_target = "secodn"

[targets.mylib]
kind = "staticlib"
sources = ["src/first.c"]

[targets.second]
kind = "staticlib"
sources = ["src/second.c"]
"#;
        let tmp = TempDir::new().unwrap();
        let err = Manifest::parse(content, &tmp.path().join("Harbour.toml"))
            .expect_err("a default_target that names nothing must not parse");
        let msg = err.to_string();

        assert!(msg.contains("secodn"), "names the bad value: {msg}");
        assert!(
            msg.contains("mylib") && msg.contains("second"),
            "lists what is actually declared: {msg}"
        );
    }

    /// A workspace has members, not targets, so the key has no meaning
    /// there and `deny_unknown_fields` rejects it rather than ignoring it.
    #[test]
    fn default_target_is_rejected_in_the_workspace_section() {
        let content = r#"
[workspace]
members = ["a"]
default_target = "a"
"#;
        let tmp = TempDir::new().unwrap();
        let err = Manifest::parse(content, &tmp.path().join("Harbour.toml"))
            .expect_err("[workspace] has no default_target");

        assert!(
            err.to_string().contains("default_target"),
            "error points at the offending key: {err}"
        );
    }

    /// A workspace member's default is its own: setting one on the root
    /// package says nothing about the member, and vice versa.
    #[test]
    fn workspace_root_and_member_defaults_are_independent() {
        let root = r#"
[workspace]
members = ["member"]

[package]
name = "root"
version = "1.0.0"
default_target = "root_b"

[targets.root_a]
kind = "staticlib"
sources = ["src/a.c"]

[targets.root_b]
kind = "staticlib"
sources = ["src/b.c"]
"#;
        let member = r#"
[package]
name = "member"
version = "1.0.0"

[targets.member_a]
kind = "staticlib"
sources = ["src/a.c"]

[targets.member_b]
kind = "staticlib"
sources = ["src/b.c"]
"#;
        let tmp = TempDir::new().unwrap();
        let root_m = Manifest::parse(root, &tmp.path().join("Harbour.toml")).unwrap();
        let member_m =
            Manifest::parse(member, &tmp.path().join("member").join("Harbour.toml")).unwrap();

        assert_eq!(root_m.default_target().unwrap().name.as_str(), "root_b");
        // Member never opted in, so it still gets the positional rule.
        assert_eq!(member_m.default_target().unwrap().name.as_str(), "member_a");
        assert_eq!(member_m.explicit_default_target_name(), None);
    }
}
