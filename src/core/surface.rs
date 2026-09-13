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
use crate::core::target::{canonical_arch, CppStandard, TargetTriple};

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

    /// Resolve a relative `kind = "path"` library against `root`.
    ///
    /// Every other variant is returned unchanged: a link *name* has no
    /// directory to anchor, and an absolute path is already anchored.
    ///
    /// This exists because a `libs` entry is declared in a manifest and
    /// consumed at link time, and by then the package it was declared in is
    /// no longer in hand. `include_dirs` has always been anchored to the
    /// declaring package's root for exactly this reason
    /// (`SurfaceResolver::add_compile_requirements` takes a `root`);
    /// `add_link_requirements` took none, so a relative archive path was
    /// passed to the linker verbatim and resolved against the process
    /// working directory -- the *root* package's directory when the manifest
    /// that named it is a dependency.
    ///
    /// The failure is not merely "file not found". Given a same-named
    /// archive anywhere the root package's relative path happens to reach,
    /// the link succeeds against the wrong file: proved by giving a root
    /// package its own `vendor/libvend.a` and watching the binary return the
    /// root's value instead of the dependency's.
    ///
    /// Anchoring lives on `LibRef` rather than in the resolver so that
    /// "where does a relative library path resolve from" has one answer no
    /// matter which fold, command or backend is asking.
    pub fn anchored(&self, root: &std::path::Path) -> LibRef {
        match self {
            LibRef::Object(LibRefObject::Path { path }) if path.is_relative() => {
                LibRef::Object(LibRefObject::Path {
                    path: root.join(path),
                })
            }
            other => other.clone(),
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

impl LinkRequirements {
    /// Reject link settings that parse but reach no command line.
    ///
    /// The policy is that a declared-but-unimplemented setting must fail
    /// loudly rather than be accepted in silence. Harbour has repeatedly
    /// shipped fields that parse, merge, propagate through the resolver and
    /// then emit nothing, and because the crate's root `pub mod`s suppress
    /// `dead_code` there is no warning from anywhere -- the only signal a
    /// user gets is a build that quietly does not do what the manifest says.
    ///
    /// `where_` names the table being validated, e.g.
    /// `` `surface.link.public` ``, so the message can point at the line.
    pub fn validate_implemented(&self, target: &str, where_: &str) -> anyhow::Result<()> {
        if !self.groups.is_empty() {
            anyhow::bail!(
                "target `{target}`: `groups` in `{where_}` is not implemented\n\
                 hint: link groups parse and are then discarded -- no \
                 `--start-group`, `--end-group` or `--whole-archive` is ever \
                 emitted, so declaring one changes nothing about the link. \
                 Remove it. Circular static libraries usually link if the \
                 archives are listed in dependency order, which \
                 `[targets.NAME.deps]` already does.\n\
                 tracking: https://github.com/aryamurray/harbour/issues/95"
            );
        }

        reject_unimplemented_libs(&self.libs, target, where_)
    }
}

/// Reject library references that parse but emit nothing.
///
/// Shared between the `surface.link.*` tables, the `surface.when` tables and
/// the `[targets.X.public]`/`[targets.X.private]` shorthand, all three of
/// which accept a `libs` list. A check in only one of them is how
/// `surface.when` came to accept what the unconditional table rejected.
pub fn reject_unimplemented_libs(
    libs: &[LibRef],
    target: &str,
    where_: &str,
) -> anyhow::Result<()> {
    for lib in libs {
        if let LibRef::Object(LibRefObject::Package { name, .. }) = lib {
            anyhow::bail!(
                "target `{target}`: `{{ kind = \"package\" }}` in `{where_}` \
                 is not implemented\n\
                 hint: this entry parses and emits nothing -- not even an \
                 error for a package that does not exist. To link another \
                 Harbour package, declare it in `[dependencies]` and name it \
                 in `[targets.{target}.deps]`; that is what puts its archive \
                 on the link line. (offending entry: name = \"{name}\")\n\
                 tracking: https://github.com/aryamurray/harbour/issues/96"
            );
        }
    }
    Ok(())
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
                 `compiler`, `feature`, and four tables, whose names are \
                 *quoted literal keys* rather than nesting:\n    \
                 [targets.NAME.surface.when.\"compile.public\"]\n\
                 and likewise \"compile.private\", \"link.public\" and \"link.private\". \
                 Neither `compile.public = {{ ... }}` nor \
                 `compile = {{ public = ... }}` is accepted.",
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

    /// Compiler family: `"gcc"`, `"clang"`, `"apple-clang"`, `"msvc"`.
    ///
    /// Matched by *family*, not by string equality -- see
    /// [`PlatformCondition::compiler_matches`] for which values imply which.
    /// The short version: `"clang"` also matches `apple-clang`; nothing else
    /// widens.
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
    /// Does a manifest's `compiler = "..."` cover the detected family?
    ///
    /// `compiler_family` (`builder::context`) produces four values: `gcc`,
    /// `clang`, `apple-clang`, `msvc`. String equality meant
    /// `compiler = "clang"` never fired on macOS, where the value is always
    /// `apple-clang` -- a silent no-op, with a manifest that looks correct.
    /// `harbour new`'s own scaffold worked around it by emitting an extra
    /// `apple-clang` block.
    ///
    /// So `clang` is treated as a family that `apple-clang` belongs to, and
    /// `apple-clang` stays available for the narrower match. What is
    /// deliberately *not* done matters more than what is:
    ///
    /// - **`gcc` does not match either clang.** On macOS `/usr/bin/gcc` is
    ///   clang in disguise, but the detected value does not come from the
    ///   name of the binary -- `detect_compiler_identity` probes the
    ///   toolchain, so a Mac reports `apple-clang` no matter which name
    ///   invoked it. A `compiler = "gcc"` block therefore already means
    ///   "really GCC", and it must keep meaning that: it is where
    ///   GCC-only flags live (`--param=`, `-fno-tree-*`, GCC-only `-W`
    ///   spellings), and clang rejects unknown `-f`/`--param` flags as
    ///   errors. Widening `gcc` would convert a silent no-op into a hard
    ///   build failure -- strictly worse.
    /// - **`clang` does not match `msvc`, and must not come to match
    ///   `clang-cl`** if that is ever added as a fifth family. `clang-cl` is
    ///   clang, but it takes MSVC flag *syntax* (`/W4`, `/std:c++17`), so
    ///   every `compiler = "clang"` block in existence -- full of `-W...`
    ///   and `-f...` -- would be wrong for it. It belongs with `msvc` on the
    ///   only axis a manifest cares about, which is which flags parse.
    ///
    /// The unifying rule, stated once so a future family can be placed by
    /// it: **two families are in the same group when a flag written for one
    /// is accepted by the other.** That is a property of the driver's
    /// command line, not of the compiler's lineage, and it is the only
    /// property a `[[when]]` block's contents depend on.
    fn compiler_matches(condition: &str, detected: &str) -> bool {
        if condition == detected {
            return true;
        }
        /// `(condition value, the detected families it also covers)`. Only
        /// clang has anything to add; the table exists so that adding a
        /// family is a decision made here rather than an omission made
        /// elsewhere.
        const FAMILIES: [(&str, &[&str]); 1] = [("clang", &["apple-clang"])];

        FAMILIES
            .iter()
            .any(|(name, members)| *name == condition && members.contains(&detected))
    }

    /// Check if this condition matches the current platform and enabled
    /// feature set.
    pub fn matches(&self, target: &TargetPlatform, features: &FeatureSet) -> bool {
        if let Some(ref os) = self.os {
            if os != &target.os {
                return false;
            }
        }
        if let Some(ref arch) = self.arch {
            // Compared canonically, so `arm64` and `aarch64` -- one
            // architecture with two names -- are one condition. Only true
            // synonyms are collapsed; ISA generations (`arm` vs `armv7`,
            // `i686` vs `i386`) stay distinct on purpose. See
            // [`canonical_arch`] for the rule and for why widening further
            // would turn a silently-slow build into a silently-wrong one.
            if canonical_arch(arch) != canonical_arch(&target.arch) {
                return false;
            }
        }
        if let Some(ref env) = self.env {
            if Some(env.as_str()) != target.env.as_deref() {
                return false;
            }
        }
        if let Some(ref compiler) = self.compiler {
            match target.compiler.as_deref() {
                Some(detected) if Self::compiler_matches(compiler, detected) => {}
                _ => return false,
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
    ///
    /// ## Arch spelling
    ///
    /// Normalized on the same principle, through
    /// [`canonical_arch`](crate::core::target::canonical_arch): `arm64`
    /// becomes `aarch64`, `amd64` becomes `x86_64`, `ppc64le` becomes
    /// `powerpc64le`. Only true synonyms -- two names for one architecture
    /// -- are collapsed; ISA generations (`arm` vs `armv7`) are left alone,
    /// and that distinction is argued at length on `canonical_arch`.
    ///
    /// Normalized *here* rather than only inside
    /// [`PlatformCondition::matches`] so that this field is the single
    /// answer to "which architecture is this build for". `plan::generator_env`
    /// hands it to a generator as `HARBOUR_TARGET_ARCH` and documents it as
    /// the value a `when` condition matches; if a condition matched
    /// canonically while this stayed literal, a generator on
    /// `arm64-apple-darwin` would be told `arm64` by the environment and
    /// `aarch64` by the block that selected it. `HARBOUR_TARGET_TRIPLE` is
    /// deliberately *not* normalized -- it is the triple as spelled, for
    /// handing back to a compiler -- so the two answer different questions
    /// on purpose.
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
            arch: canonical_arch(triple.arch()).to_string(),
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

    /// The `surface.when` hint has to name the spelling that works.
    ///
    /// It listed the four tables as `compile.public`, `compile.private`,
    /// `link.public`, `link.private` -- literally accurate, and read by two
    /// people as though nesting would work. It does not: in TOML,
    /// `compile.public = { ... }` *is* `compile = { public = ... }`, so both
    /// arrive here as an unknown key `compile`, and the only accepted form
    /// is the quoted literal key
    /// `[targets.X.surface.when."compile.public"]`. Verified by running all
    /// three spellings: the quoted one puts its define on the compile line,
    /// the other two are errors.
    #[test]
    fn the_surface_when_hint_names_the_quoted_key_form() {
        let err = toml::from_str::<super::ConditionalSurface>(
            "os = \"linux\"\ncompile = { public = { defines = [\"X=1\"] } }\n",
        )
        .expect("the catch-all absorbs it, so the message is ours to write")
        .validate()
        .expect_err("a nested `compile` table is not a surface.when table")
        .to_string();

        assert!(
            err.contains("\"compile.public\""),
            "the hint must show the quoted-key spelling: {err}"
        );
        assert!(
            err.contains("compile = {") || err.contains("public = "),
            "and name the nesting that does not work: {err}"
        );
    }

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

    /// `kind = "path"` anchors to the declaring package's root; nothing else
    /// is touched.
    #[test]
    fn a_relative_path_library_is_anchored_and_nothing_else_is() {
        let root = std::path::Path::new("/pkgs/mylib");

        // The bug: a relative archive path used to reach the linker
        // verbatim and resolve against the process working directory.
        let rel = LibRef::path("vendor/libfoo.a");
        assert_eq!(
            rel.anchored(root).to_flags(),
            vec![root.join("vendor/libfoo.a").display().to_string()],
            "a relative `kind = \"path\"` must resolve inside the package \
             that declared it"
        );

        // An absolute path is already anchored and must not be rewritten.
        // Spelled per-platform: `/opt/...` has no drive letter, so Windows
        // would classify it as relative and the case would not be tested.
        let abs_path = if cfg!(windows) {
            "C:\\opt\\vendor\\libfoo.a"
        } else {
            "/opt/vendor/libfoo.a"
        };
        let abs = LibRef::path(abs_path);
        assert_eq!(abs.anchored(root).to_flags(), vec![abs_path.to_string()]);

        // Everything else is a link *name*: there is no directory to
        // anchor, and joining one would corrupt the flag.
        for lib in [
            LibRef::system("m"),
            LibRef::framework("Security"),
            LibRef::Shorthand("-lpthread".to_string()),
            LibRef::Shorthand("pthread".to_string()),
            LibRef::package("other", "other"),
        ] {
            assert_eq!(
                lib.anchored(root).to_flags(),
                lib.to_flags(),
                "anchoring must not touch {lib:?}"
            );
        }
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

    /// `compiler` is matched by family, so `compiler = "clang"` fires on a
    /// Mac -- where the detected value is always `apple-clang`, and where
    /// string equality made the condition a silent no-op.
    ///
    /// The negative cases are the point of the test as much as the positive
    /// one. `gcc` must not widen (GCC-only flags are hard errors under
    /// clang, so widening turns a no-op into a failed build) and `msvc` must
    /// not (different flag syntax entirely).
    #[test]
    fn compiler_conditions_match_by_family() {
        let with = |detected: &str| TargetPlatform {
            os: "macos".to_string(),
            arch: "aarch64".to_string(),
            env: None,
            compiler: Some(detected.to_string()),
        };
        let asking = |condition: &str| PlatformCondition {
            compiler: Some(condition.to_string()),
            ..Default::default()
        };
        let fires = |condition: &str, detected: &str| {
            asking(condition).matches(&with(detected), &FeatureSet::new())
        };

        // The bug: `clang` must cover Apple's clang.
        assert!(
            fires("clang", "apple-clang"),
            "`compiler = \"clang\"` must fire on macOS, where the detected \
             family is always `apple-clang`"
        );
        // And still cover plain clang.
        assert!(fires("clang", "clang"));
        // `apple-clang` stays the narrower match.
        assert!(fires("apple-clang", "apple-clang"));
        assert!(
            !fires("apple-clang", "clang"),
            "`apple-clang` must stay narrower than `clang`, or there is no \
             way left to say `only Apple's`"
        );

        // Nothing else widens.
        for (condition, detected) in [
            ("gcc", "clang"),
            ("gcc", "apple-clang"),
            ("clang", "gcc"),
            ("clang", "msvc"),
            ("msvc", "clang"),
            ("msvc", "apple-clang"),
            ("apple-clang", "gcc"),
        ] {
            assert!(
                !fires(condition, detected),
                "`compiler = \"{condition}\"` must not fire on `{detected}`: \
                 a flag written for one is not accepted by the other"
            );
        }

        // Every family matches itself, including the two that group.
        for family in ["gcc", "clang", "apple-clang", "msvc"] {
            assert!(fires(family, family), "`{family}` must match itself");
        }

        // A condition naming a family Harbour never produces matches
        // nothing, rather than matching everything.
        assert!(!fires("clang-cl", "msvc"));
        assert!(!fires("icc", "clang"));
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

    #[test]
    fn arch_condition_matches_a_synonym_spelling_both_ways() {
        // `arm64` and `aarch64` are one architecture with two names: Apple
        // and the Linux kernel say `arm64`, LLVM and Rust say `aarch64`, and
        // `arm64-apple-darwin` is a triple clang accepts. A manifest keyed
        // on either must apply to a build through the other -- otherwise a
        // package's entire aarch64 assembly layer silently disappears and
        // the portable C is compiled instead, which is a correct, slower
        // library with no witness.
        let arm64 = TargetPlatform::for_target(&TargetTriple::parse("arm64-apple-darwin"));
        let aarch64 = TargetPlatform::for_target(&TargetTriple::parse("aarch64-apple-darwin"));

        for platform in [&arm64, &aarch64] {
            for spelling in ["arm64", "aarch64"] {
                let cond = PlatformCondition {
                    arch: Some(spelling.to_string()),
                    ..Default::default()
                };
                assert!(
                    cond.matches(platform, &FeatureSet::new()),
                    "arch = {spelling:?} must match {:?}",
                    platform.arch
                );
            }
        }

        // Same for amd64/x86_64.
        let amd64 = TargetPlatform::for_target(&TargetTriple::parse("amd64-unknown-linux-gnu"));
        let cond = PlatformCondition {
            arch: Some("x86_64".to_string()),
            ..Default::default()
        };
        assert!(cond.matches(&amd64, &FeatureSet::new()));
    }

    #[test]
    fn arch_condition_does_not_match_a_different_isa_generation() {
        // The other half of the decision, and the more important one: a
        // generation is not a synonym. `arch = "armv7"` means "ARMv7, not
        // ARMv4T" -- it is where NEON assembly lives -- so widening it
        // would select instructions the target cannot execute. A block that
        // does not match produces a slower build; a block that matches the
        // wrong machine produces an illegal instruction.
        let arm = TargetPlatform::for_target(&TargetTriple::parse("arm-unknown-linux-gnueabihf"));
        for spelling in ["armv7", "armv4t", "aarch64", "thumbv7m"] {
            let cond = PlatformCondition {
                arch: Some(spelling.to_string()),
                ..Default::default()
            };
            assert!(
                !cond.matches(&arm, &FeatureSet::new()),
                "arch = {spelling:?} must not match a plain `arm` target"
            );
        }

        let i686 = TargetPlatform::for_target(&TargetTriple::parse("i686-unknown-linux-gnu"));
        let cond = PlatformCondition {
            arch: Some("i386".to_string()),
            ..Default::default()
        };
        assert!(!cond.matches(&i686, &FeatureSet::new()));
    }

    #[test]
    fn for_target_normalizes_a_synonym_arch_spelling() {
        // The field, not just the comparison. `plan::generator_env` hands
        // this to a `prebuild` generator as `HARBOUR_TARGET_ARCH` and
        // documents it as "the value a `when` condition matches", so if a
        // condition matched canonically while this stayed literal, a
        // generator on `arm64-apple-darwin` would be told `arm64` by its
        // environment and `aarch64` by the block that selected it.
        for raw in ["arm64-apple-darwin", "aarch64-apple-darwin"] {
            let platform = TargetPlatform::for_target(&TargetTriple::parse(raw));
            assert_eq!(platform.arch, "aarch64", "{raw}");
        }
        assert_eq!(
            TargetPlatform::for_target(&TargetTriple::parse("amd64-unknown-linux-gnu")).arch,
            "x86_64"
        );

        // And a generation is still itself.
        assert_eq!(
            TargetPlatform::for_target(&TargetTriple::parse("armv7-unknown-linux-gnueabihf")).arch,
            "armv7"
        );
        assert_eq!(
            TargetPlatform::for_target(&TargetTriple::parse("thumbv7em-none-eabi")).arch,
            "thumbv7em"
        );
    }
}
