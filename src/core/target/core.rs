//! Core target types.
//!
//! This module contains the main Target struct and related types
//! for defining build targets.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::core::features::FeatureSet;
use crate::core::surface::{
    CompileRequirements, Define, PlatformCondition, Surface, TargetPlatform,
};
use crate::util::InternedString;

use super::ffi::FfiConfig;
use super::language::{CStandard, CppStandard, Language};

/// The kind of target being built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum TargetKind {
    /// Executable binary
    #[serde(alias = "bin")]
    #[default]
    Exe,

    /// Static library (.a / .lib)
    #[serde(alias = "lib", alias = "static")]
    StaticLib,

    /// Shared/dynamic library (.so / .dylib / .dll)
    #[serde(alias = "dylib", alias = "dynamic")]
    SharedLib,

    /// Header-only library (no compile/link steps)
    #[serde(alias = "header-only", alias = "interface")]
    HeaderOnly,
}

impl TargetKind {
    /// Get the typical file extension for this target kind.
    pub fn extension(&self, os: &str) -> &'static str {
        match self {
            TargetKind::Exe => {
                if os == "windows" {
                    "exe"
                } else {
                    ""
                }
            }
            TargetKind::StaticLib => {
                if os == "windows" {
                    "lib"
                } else {
                    "a"
                }
            }
            TargetKind::SharedLib => match os {
                "windows" => "dll",
                "macos" => "dylib",
                _ => "so",
            },
            TargetKind::HeaderOnly => "",
        }
    }

    /// Get the typical file prefix for this target kind.
    pub fn prefix(&self, os: &str) -> &'static str {
        match self {
            TargetKind::Exe | TargetKind::HeaderOnly => "",
            TargetKind::StaticLib | TargetKind::SharedLib => {
                if os == "windows" {
                    ""
                } else {
                    "lib"
                }
            }
        }
    }

    /// Get the output filename for a target.
    pub fn output_filename(&self, name: &str, os: &str) -> String {
        let prefix = self.prefix(os);
        let ext = self.extension(os);
        if ext.is_empty() {
            format!("{}{}", prefix, name)
        } else {
            format!("{}{}.{}", prefix, name, ext)
        }
    }

    /// Check if this is a library (static, shared, or header-only).
    pub fn is_library(&self) -> bool {
        matches!(
            self,
            TargetKind::StaticLib | TargetKind::SharedLib | TargetKind::HeaderOnly
        )
    }

    /// Check if this produces a linkable artifact.
    pub fn is_linkable(&self) -> bool {
        matches!(self, TargetKind::StaticLib | TargetKind::SharedLib)
    }

    /// Check if this is a header-only library.
    pub fn is_header_only(&self) -> bool {
        matches!(self, TargetKind::HeaderOnly)
    }
}

/// A build target with its configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Target {
    /// Target name (usually same as package name for single-target packages)
    pub name: InternedString,

    /// What kind of artifact to produce
    #[serde(default)]
    pub kind: TargetKind,

    /// Source file patterns (globs)
    #[serde(default)]
    pub sources: Vec<String>,

    /// Patterns to drop from `sources` after expansion.
    ///
    /// For libraries that ship example or test programs alongside their
    /// library sources, where a single glob would otherwise pull in a second
    /// `main()`.
    #[serde(default)]
    pub exclude: Vec<String>,

    /// Platform-conditional source selection.
    ///
    /// Each entry adds `sources`/`exclude` patterns to the base lists above
    /// when its [`PlatformCondition`] matches the target platform being
    /// built for (never the host -- see [`TargetPlatform::for_target`]).
    /// This lives on the target itself, not inside `surface.when`: source
    /// selection is a build-input concern private to how this target is
    /// compiled, whereas `Surface` (and its own `when` conditionals) models
    /// the contract a target exports to *dependents*. Conflating the two
    /// would mean a change to what files get compiled could accidentally be
    /// interpreted as a change to the dependency surface.
    #[serde(default, rename = "when")]
    pub when: Vec<ConditionalSources>,

    /// Code generators to run before this target's sources are resolved,
    /// e.g. to materialize a generated header (`configure`-style codegen)
    /// that source files `#include`, or a whole generated translation unit
    /// that the target then compiles.
    ///
    /// These are the *unconditional* generators. Platform- or
    /// feature-specific ones go in a `[[targets.X.when]]` block; read the
    /// full set through [`Target::resolved_prebuild`] rather than this
    /// field, or per-platform generators will be silently skipped.
    ///
    /// They run once per build, always (never skipped), and always before
    /// source resolution and fingerprinting, so that generated files exist
    /// before anything globs or hashes them. See
    /// `BuildPlan::with_root_packages`.
    #[serde(default)]
    pub prebuild: Vec<CustomCommand>,

    /// Public header patterns (for libraries)
    #[serde(default)]
    pub public_headers: Vec<String>,

    /// Surface contract (compile/link requirements)
    #[serde(default)]
    pub surface: Surface,

    /// Target-specific dependencies (keyed by package name for O(1) lookup)
    #[serde(default)]
    pub deps: HashMap<InternedString, TargetDepSpec>,

    /// Build recipe override
    #[serde(default)]
    pub recipe: Option<BuildRecipe>,

    /// Source language (C or C++)
    #[serde(default)]
    pub lang: Language,

    /// C standard version (only meaningful when lang = C)
    #[serde(default)]
    pub c_std: Option<CStandard>,

    /// C++ standard version (only meaningful when lang = C++)
    #[serde(default)]
    pub cpp_std: Option<CppStandard>,

    /// Backend-specific configuration
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<crate::core::manifest::BackendConfig>,

    /// FFI binding generation configuration
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ffi: Option<FfiConfig>,

    /// Build this target's own sources without assuming a hosted C
    /// implementation: `-ffreestanding` when compiling, `-nostdlib` when
    /// linking.
    ///
    /// A *target-level* switch rather than a new [`TargetKind`], and
    /// deliberately separate from `[package] requires`. Those answer
    /// different questions: `requires = "freestanding"` is a claim about
    /// what the package's code *can* run on, checked across the whole
    /// dependency graph, while this says how *this* artifact is built. A
    /// freestanding image is linked exactly like an `exe` -- objects in,
    /// one file out, same driver, same output naming -- so a separate kind
    /// would buy nothing and would silently fall out of every existing
    /// `kind == TargetKind::Exe` test in the builder.
    ///
    /// Note that this applies to this target's *own* translation units.
    /// A dependency is compiled from its own manifest, so a library
    /// intended for bare metal has to say so itself (private `cflags`,
    /// plus `requires = "freestanding"` to be checked).
    #[serde(default)]
    pub freestanding: bool,

    /// Linker script for this target, resolved against the *package root*.
    ///
    /// Package-relative, not cwd-relative. During a build the process
    /// working directory is the *root* package's directory, so a bare
    /// relative path would resolve against the wrong package the moment
    /// this package is consumed as a dependency -- the same trap that
    /// `include_dirs` in a `when` block and a recipe's `source_dir` were
    /// both fixed for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linker_script: Option<PathBuf>,

    /// Entry symbol to pass to the linker (`-Wl,--entry=NAME`).
    ///
    /// Independent of `freestanding`: a hosted program can override its
    /// entry point, and a freestanding one may get its entry from the
    /// linker script's `ENTRY()` instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
}

impl Target {
    /// Create a new target with the given name and kind.
    pub fn new(name: impl Into<InternedString>, kind: TargetKind) -> Self {
        Target {
            name: name.into(),
            kind,
            sources: Vec::new(),
            exclude: Vec::new(),
            when: Vec::new(),
            prebuild: Vec::new(),
            public_headers: Vec::new(),
            surface: Surface::default(),
            deps: HashMap::new(),
            recipe: None,
            lang: Language::default(),
            c_std: None,
            cpp_std: None,
            backend: None,
            ffi: None,
            freestanding: false,
            linker_script: None,
            entry: None,
        }
    }

    /// Create a new executable target.
    pub fn exe(name: impl Into<InternedString>) -> Self {
        Self::new(name, TargetKind::Exe)
    }

    /// Create a new static library target.
    pub fn staticlib(name: impl Into<InternedString>) -> Self {
        Self::new(name, TargetKind::StaticLib)
    }

    /// Create a new shared library target.
    pub fn sharedlib(name: impl Into<InternedString>) -> Self {
        Self::new(name, TargetKind::SharedLib)
    }

    /// Create a new header-only library target.
    pub fn headeronly(name: impl Into<InternedString>) -> Self {
        Self::new(name, TargetKind::HeaderOnly)
    }

    /// Validate target configuration.
    ///
    /// Checks for:
    /// - Header-only targets must not have sources or recipes
    /// - C++ source extensions must match lang=c++ setting
    pub fn validate(&self) -> Result<()> {
        // Header-only validation
        if self.kind == TargetKind::HeaderOnly {
            if !self.sources.is_empty() {
                bail!(
                    "header-only target '{}' must not have sources\n\
                     hint: remove the sources field or change kind to staticlib/sharedlib",
                    self.name
                );
            }
            if self.recipe.is_some() {
                bail!(
                    "header-only target '{}' must not have a recipe\n\
                     hint: remove the recipe field",
                    self.name
                );
            }
        }

        // A header-only target is never compiled and never linked, and a
        // static library is archived rather than linked, so these keys have
        // nowhere to go. Silently dropping them is how `frameworks` managed
        // to be parsed, propagated and reported for months without ever
        // reaching the linker; refusing them keeps that from repeating.
        if self.kind == TargetKind::HeaderOnly {
            for (field, present) in [
                ("freestanding", self.freestanding),
                ("linker_script", self.linker_script.is_some()),
                ("entry", self.entry.is_some()),
            ] {
                if present {
                    bail!(
                        "header-only target '{}' must not set `{}`: it is neither \
                         compiled nor linked, so the setting would have no effect\n\
                         hint: set it on the exe target that links these headers",
                        self.name,
                        field
                    );
                }
            }
        }

        // `-Wl,` hands the driver a comma-separated list, so a comma
        // anywhere in the script path splits it into two bogus linker
        // arguments. That is a silent corruption -- the linker gets
        // `-T`, a truncated path, and a stray fragment -- so it is refused
        // rather than emitted. Checked on the declared path: the package
        // root is prepended later and is outside the manifest's control,
        // but a comma there would fail the same way and shows up as the
        // linker's own error.
        if let Some(script) = &self.linker_script {
            if script.to_string_lossy().contains(',') {
                bail!(
                    "target '{}' names linker script `{}`, whose path contains a \
                     comma\n\
                     hint: the script is passed as `-Wl,-T,<path>` and `-Wl,` \
                     splits its argument on commas, so the path would reach the \
                     linker in two pieces; rename the file or directory",
                    self.name,
                    script.display()
                );
            }
        }

        if self.kind == TargetKind::StaticLib {
            for (field, present) in [
                ("linker_script", self.linker_script.is_some()),
                ("entry", self.entry.is_some()),
            ] {
                if present {
                    bail!(
                        "static library target '{}' must not set `{}`: a static \
                         library is archived with `ar`, not linked, so no linker \
                         ever sees it\n\
                         hint: set it on the exe target that links this library. \
                         `freestanding = true` is accepted here -- it affects how \
                         this library's own sources compile.",
                        self.name,
                        field
                    );
                }
            }
        }

        // Source extension validation: C++ extensions require lang=c++
        if self.lang == Language::C {
            let cpp_extensions = [".cc", ".cpp", ".cxx", ".C", ".c++"];
            for pattern in &self.sources {
                if cpp_extensions.iter().any(|ext| pattern.ends_with(ext)) {
                    bail!(
                        "target '{}' has lang=c but sources match C++ extensions\n\
                         hint: set lang = 'c++' in [targets.{}]",
                        self.name,
                        self.name
                    );
                }
            }
        }

        // A `libs` entry is a link *name*, so a filename becomes a flag that
        // can never resolve: `libs = ["libssl.a"]` compiles to `-llibssl.a`,
        // which the linker looks for as `liblibssl.a.a`. This was silent, and
        // the resulting undefined symbols pointed nowhere near the manifest.
        for reqs in [&self.surface.link.public, &self.surface.link.private] {
            for lib in &reqs.libs {
                let Some(name) = lib.name() else {
                    continue;
                };
                // `-l:libfoo.a` is real GNU ld syntax for an exact filename.
                if name.starts_with(':') {
                    continue;
                }
                let looks_like_a_file = name.contains('/')
                    || name.contains('\\')
                    || [".a", ".so", ".dylib", ".lib", ".tbd"]
                        .iter()
                        .any(|ext| name.ends_with(ext));
                if looks_like_a_file {
                    let stripped = name
                        .rsplit('/')
                        .next()
                        .unwrap_or(name)
                        .trim_start_matches("lib")
                        .split('.')
                        .next()
                        .unwrap_or(name);
                    bail!(
                        "target '{}' lists `{}` in a link surface's `libs`, but `libs` \
                         takes link names, not filenames -- this would be passed as \
                         `-l{}`\n\
                         hint: write `libs = [\"{}\"]`, or use \
                         `{{ kind = \"path\", path = \"{}\" }}` to link that exact file",
                        self.name,
                        name,
                        name,
                        stripped,
                        name
                    );
                }
            }
        }

        Ok(())
    }

    /// Check if this target requires C++ compilation or linking.
    pub fn requires_cpp(&self) -> bool {
        self.lang == Language::Cxx || self.cpp_std.is_some()
    }

    /// Extra compiler flags implied by `freestanding = true`.
    ///
    /// Only `-ffreestanding`. `-nostdlib` is a *link* flag and lives in
    /// [`Target::link_control_flags`]; the two halves are separate on
    /// purpose, since a static library has a compile step and no link step.
    pub fn freestanding_cflags(&self) -> Vec<String> {
        if self.freestanding {
            vec!["-ffreestanding".to_string()]
        } else {
            Vec::new()
        }
    }

    /// Extra linker flags implied by `freestanding`, `linker_script` and
    /// `entry`, with the script resolved against `package_root`.
    ///
    /// Every flag is a *single* argv token, which is load-bearing rather
    /// than stylistic: the effective link surface is sorted and deduplicated
    /// (`SurfaceResolver::resolve_link_surface`), so a two-token form such
    /// as `["-T", "layout.ld"]` or `["-e", "_start"]` would be split apart
    /// and the operand handed to the linker as a free-standing argument.
    /// `-Wl,-T,PATH` and `-Wl,--entry=NAME` survive sorting intact.
    pub fn link_control_flags(&self, package_root: &Path) -> Vec<String> {
        let mut flags = Vec::new();

        if self.freestanding {
            // Implies -nostartfiles and -nodefaultlibs. Note it also drops
            // libgcc, so a target needing the compiler's runtime helpers
            // (64-bit division, `__aeabi_*`) must ask for it explicitly.
            flags.push("-nostdlib".to_string());
        }

        if let Some(script) = &self.linker_script {
            let abs = if script.is_absolute() {
                script.clone()
            } else {
                package_root.join(script)
            };
            flags.push(format!("-Wl,-T,{}", abs.display()));
        }

        if let Some(entry) = &self.entry {
            flags.push(format!("-Wl,--entry={}", entry));
        }

        flags
    }

    /// The absolute path of this target's linker script, if it has one.
    ///
    /// Same package-root anchoring as [`Target::link_control_flags`], which
    /// is the only reason this is a method rather than a field read.
    pub fn resolved_linker_script(&self, package_root: &Path) -> Option<PathBuf> {
        self.linker_script.as_ref().map(|script| {
            if script.is_absolute() {
                script.clone()
            } else {
                package_root.join(script)
            }
        })
    }

    /// Add source patterns.
    pub fn with_sources(mut self, patterns: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.sources = patterns.into_iter().map(|p| p.into()).collect();
        self
    }

    /// Add public header patterns.
    pub fn with_public_headers(
        mut self,
        patterns: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.public_headers = patterns.into_iter().map(|p| p.into()).collect();
        self
    }

    /// Get the output filename for this target.
    pub fn output_filename(&self, os: &str) -> String {
        self.kind.output_filename(&self.name, os)
    }

    /// Resolve the effective `sources`/`exclude` glob patterns for a given
    /// target platform and enabled feature set, applying any matching
    /// `when` entries on top of the unconditional base lists.
    ///
    /// Evaluated against the *build target's* platform (see
    /// [`TargetPlatform::for_target`]), never the host, so cross-compiling
    /// selects the source set for the target triple, not the machine
    /// running Harbour. `features` is this target's *own* package's
    /// resolved feature set (see
    /// `builder::surface_resolver::compute_feature_sets`) -- never a
    /// dependent's -- since a `when` entry keyed on `feature = "..."` is
    /// only meaningful against the feature set the package compiling this
    /// target was itself built with.
    pub fn resolved_sources(
        &self,
        platform: &TargetPlatform,
        features: &FeatureSet,
    ) -> (Vec<String>, Vec<String>) {
        let mut sources = self.sources.clone();
        let mut exclude = self.exclude.clone();
        for cond in &self.when {
            if cond.condition.matches(platform, features) {
                sources.extend(cond.sources.iter().cloned());
                exclude.extend(cond.exclude.iter().cloned());
            }
        }
        (sources, exclude)
    }

    /// Resolve the additional *private* compile requirements (defines,
    /// cflags) contributed by matching `when` entries.
    ///
    /// Lives on the same [`ConditionalSources`] entries as
    /// [`Target::resolved_sources`] rather than in `Surface::when`: a
    /// feature commonly needs to add a source file *and* the define that
    /// makes the amalgamated sources compile that code in (sqlite's
    /// `SQLITE_ENABLE_FTS5` is exactly this shape), and splitting those two
    /// effects of one feature toggle across two separate `[[...]]` blocks
    /// in the manifest (`targets.X.when` for the source, `targets.X.surface
    /// .when` for the define) would be strictly worse ergonomics for no
    /// benefit -- both are private, target-local build inputs, not part of
    /// the dependency surface. A feature that *also* needs to affect what a
    /// dependent sees (e.g. a public header guarded by the same define)
    /// still has `Surface::conditionals` (`[[targets.X.surface.when]]`)
    /// available, gated by the identical `feature = "..."` predicate.
    pub fn resolved_extra_compile(
        &self,
        platform: &TargetPlatform,
        features: &FeatureSet,
    ) -> CompileRequirements {
        let mut extra = CompileRequirements::default();
        for cond in &self.when {
            if cond.condition.matches(platform, features) {
                extra.defines.extend(cond.defines.iter().cloned());
                extra.cflags.extend(cond.cflags.iter().cloned());
                extra.include_dirs.extend(cond.include_dirs.iter().cloned());
            }
        }
        extra
    }

    /// Resolve the full list of code generators to run for this target:
    /// the unconditional [`Target::prebuild`] entries, then those from every
    /// matching `[[targets.X.when]]` block, in manifest order.
    ///
    /// A generator is frequently the *most* platform-specific thing a
    /// package does, so an unconditional-only `prebuild` cannot express the
    /// real cases. openssl runs perlasm scripts with `flavour elf` on Linux
    /// x86_64 and a different set with `flavour macosx` on Darwin; picking
    /// one and running it everywhere does not degrade gracefully, it emits
    /// assembly the assembler rejects.
    ///
    /// Conditional generators come last, and the unconditional ones first,
    /// so a `when` block can rely on shared setup an unconditional step did.
    pub fn resolved_prebuild(
        &self,
        platform: &TargetPlatform,
        features: &FeatureSet,
    ) -> Vec<CustomCommand> {
        let mut prebuild = self.prebuild.clone();
        for cond in &self.when {
            if cond.condition.matches(platform, features) {
                prebuild.extend(cond.prebuild.iter().cloned());
            }
        }
        prebuild
    }
}

/// A platform-conditional patch to a target's source list, private compile
/// flags, and code generators.
///
/// Selects additional `sources`/`exclude` patterns, `defines`/`cflags`/
/// `include_dirs`, and `prebuild` generators when [`condition`] matches the
/// platform being built for. See the doc comment on
/// [`Target::when`] for why this is a target-level list rather than living
/// inside `surface.when`.
///
/// [`condition`]: ConditionalSources::condition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConditionalSources {
    /// Platform condition controlling whether this entry applies.
    #[serde(flatten)]
    pub condition: PlatformCondition,

    /// Additional source glob patterns to include when the condition matches.
    #[serde(default)]
    pub sources: Vec<String>,

    /// Additional exclude glob patterns to apply when the condition matches.
    #[serde(default)]
    pub exclude: Vec<String>,

    /// Additional preprocessor defines to apply (privately, to this
    /// target's own compilation only) when the condition matches. See
    /// [`Target::resolved_extra_compile`].
    #[serde(default)]
    pub defines: Vec<Define>,

    /// Additional compiler flags to apply (privately) when the condition
    /// matches. See [`Target::resolved_extra_compile`].
    #[serde(default)]
    pub cflags: Vec<String>,

    /// Additional include directories to search (privately) when the
    /// condition matches, relative to the package root like every other
    /// `include_dirs`.
    ///
    /// This exists for generated headers that differ per platform. A
    /// configure-derived `config.h` is the common case: curl's is 793 lines
    /// of probe results, so a shim vendors one per platform and points at
    /// the right directory here. Expressing that through `cflags` instead
    /// (`-Iharbour-config/linux-x86_64`) does not work, because a bare
    /// relative `-I` resolves against the process working directory -- the
    /// *root* package's directory when this package is a dependency -- and
    /// so silently finds nothing.
    #[serde(default)]
    pub include_dirs: Vec<PathBuf>,

    /// Code generators to run when the condition matches, in addition to
    /// the target's unconditional [`Target::prebuild`] ones.
    ///
    /// This is what makes a per-platform generator expressible at all: the
    /// script to run, and the flags to run it with, are routinely different
    /// per os/arch (openssl's perlasm `flavour`) rather than being one
    /// script that happens to need a different source list. Read via
    /// [`Target::resolved_prebuild`].
    #[serde(default)]
    pub prebuild: Vec<CustomCommand>,
}

/// Specification for a target-level dependency with visibility settings.
///
/// Target-level deps allow fine-grained control over which surfaces
/// propagate from dependencies. When specified, they override the
/// package-level dependency list for surface resolution.
///
/// The package name is stored as the key in `Target.deps` HashMap,
/// enabling O(1) lookup by dependency name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetDepSpec {
    /// Target name within the package (defaults to package name)
    #[serde(default)]
    pub target: Option<String>,

    /// Compile surface visibility - controls whether the dependency's
    /// public compile surface propagates to this target
    #[serde(default = "default_visibility")]
    pub compile: Visibility,

    /// Link surface visibility - controls whether the dependency's
    /// public link surface propagates to this target
    #[serde(default = "default_visibility")]
    pub link: Visibility,
}

fn default_visibility() -> Visibility {
    Visibility::Public
}

impl TargetDepSpec {
    /// Create a new target dependency spec with default (public) visibility.
    pub fn new() -> Self {
        TargetDepSpec {
            target: None,
            compile: Visibility::Public,
            link: Visibility::Public,
        }
    }

    /// Set the target name.
    pub fn with_target(mut self, target: impl Into<String>) -> Self {
        self.target = Some(target.into());
        self
    }

    /// Set compile visibility.
    pub fn with_compile(mut self, visibility: Visibility) -> Self {
        self.compile = visibility;
        self
    }

    /// Set link visibility.
    pub fn with_link(mut self, visibility: Visibility) -> Self {
        self.link = visibility;
        self
    }

    /// Get the effective target name given the package name.
    pub fn target_name<'a>(&'a self, package_name: &'a str) -> &'a str {
        self.target.as_deref().unwrap_or(package_name)
    }
}

impl Default for TargetDepSpec {
    fn default() -> Self {
        Self::new()
    }
}

/// Visibility of a dependency's surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum Visibility {
    /// Propagates to dependents
    #[default]
    Public,
    /// Internal only
    Private,
}

/// Build recipe for a target.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum BuildRecipe {
    /// Use the native Harbour builder
    Native,

    /// Use CMake
    CMake {
        /// CMakeLists.txt directory (defaults to package root)
        #[serde(default)]
        source_dir: Option<PathBuf>,

        /// Additional CMake arguments
        #[serde(default)]
        args: Vec<String>,

        /// CMake targets to build
        #[serde(default)]
        targets: Vec<String>,
    },

    /// Use Meson
    Meson {
        /// meson.build directory (defaults to package root)
        #[serde(default)]
        source_dir: Option<PathBuf>,

        /// Additional Meson options (-D flags)
        #[serde(default)]
        options: Vec<String>,

        /// Meson targets to build
        #[serde(default)]
        targets: Vec<String>,
    },

    /// Custom build steps (structured, not shell commands)
    Custom {
        /// Steps to execute
        steps: Vec<CustomCommand>,
    },
}

/// A structured custom command (not a shell string).
///
/// This is safer than shell strings because:
/// - No shell injection vulnerabilities
/// - Cross-platform (no shell-specific syntax)
/// - Easier to analyze for caching/fingerprinting
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomCommand {
    /// Program to execute
    pub program: String,

    /// Arguments to pass
    #[serde(default)]
    pub args: Vec<String>,

    /// Working directory (relative to package root)
    #[serde(default)]
    pub cwd: Option<PathBuf>,

    /// Environment variables to set for this command
    #[serde(default)]
    pub env: BTreeMap<String, String>,

    /// Output files this command produces (for fingerprinting)
    #[serde(default)]
    pub outputs: Vec<PathBuf>,

    /// Input files this command depends on (for fingerprinting)
    #[serde(default)]
    pub inputs: Vec<PathBuf>,
}

impl CustomCommand {
    /// Create a new custom command.
    pub fn new(program: impl Into<String>) -> Self {
        CustomCommand {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            env: BTreeMap::new(),
            outputs: Vec::new(),
            inputs: Vec::new(),
        }
    }

    /// Add an argument.
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Add multiple arguments.
    pub fn args(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.args.extend(args.into_iter().map(|a| a.into()));
        self
    }

    /// Set the working directory.
    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// Add an output file.
    pub fn output(mut self, output: impl Into<PathBuf>) -> Self {
        self.outputs.push(output.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image() -> Target {
        let mut t = Target::exe("payload");
        t.freestanding = true;
        t.linker_script = Some(PathBuf::from("boot/layout.ld"));
        t.entry = Some("_start".to_string());
        t
    }

    /// `-ffreestanding` is a compile flag and `-nostdlib` a link flag; a
    /// target with only an archive step (staticlib) still needs the first
    /// and has nowhere to put the second, which is why they are separate
    /// methods rather than one list.
    #[test]
    fn freestanding_splits_across_the_compile_and_link_halves() {
        let t = image();
        assert_eq!(t.freestanding_cflags(), vec!["-ffreestanding"]);
        assert!(t
            .link_control_flags(Path::new("/pkg"))
            .contains(&"-nostdlib".to_string()));

        let plain = Target::exe("app");
        assert!(plain.freestanding_cflags().is_empty());
        assert!(plain.link_control_flags(Path::new("/pkg")).is_empty());
    }

    /// The path carried by the `-Wl,-T,` flag, as a [`Path`].
    ///
    /// Asserting on the flag's *payload* rather than on the whole formatted
    /// string is what keeps these tests honest across platforms:
    /// `Path::join` inserts the platform separator and leaves any separator
    /// already inside the joined component alone, so on Windows a
    /// `linker_script` of `boot/layout.ld` under root `/pkg` renders as
    /// `/pkg\boot/layout.ld` -- mixed, and not equal to any hardcoded
    /// spelling. Comparing `Path`s compares components, which is exactly
    /// the separator-insensitive question worth asking.
    fn script_path_in(flags: &[String]) -> PathBuf {
        let payload = flags
            .iter()
            .find_map(|f| f.strip_prefix("-Wl,-T,"))
            .expect("a target with a linker_script must emit exactly one -Wl,-T, flag");
        PathBuf::from(payload)
    }

    /// The path a relative `linker_script` resolves to must be the package's
    /// own root, never the process working directory. During a build the cwd
    /// is the *root* package's directory, so a dependency's relative path
    /// resolved against the cwd silently names a file in the wrong package
    /// -- the bug already fixed once for `include_dirs` in a `when` block
    /// and once for a recipe's `source_dir`.
    #[test]
    fn a_relative_linker_script_anchors_to_the_package_root() {
        let root = Path::new("/deps/payload-0.1.0");
        let t = image();

        for resolved in [
            t.resolved_linker_script(root).expect("script is set"),
            script_path_in(&t.link_control_flags(root)),
        ] {
            assert!(
                resolved.starts_with(root),
                "`{}` is not anchored to the package root",
                resolved.display()
            );
            assert_eq!(
                resolved.strip_prefix(root),
                Ok(Path::new("boot/layout.ld")),
                "the package root must be the only thing prepended"
            );
        }
    }

    /// A rooted script path replaces the package root rather than being
    /// appended to it. Holds on Windows too, though not via the
    /// `is_absolute` check: `/etc/...` is *not* absolute there (no drive
    /// prefix), but `PathBuf::push` has a dedicated "has a root but no
    /// prefix" branch that truncates the receiver, so the outcome is the
    /// same. Written with a rooted-but-prefixless path on purpose, since
    /// that is the case where the two mechanisms differ.
    #[test]
    fn a_rooted_linker_script_replaces_the_package_root() {
        let mut t = Target::exe("payload");
        t.linker_script = Some(PathBuf::from("/etc/harbour/layout.ld"));

        assert_eq!(
            t.resolved_linker_script(Path::new("/pkg")),
            Some(PathBuf::from("/etc/harbour/layout.ld"))
        );
    }

    /// Load-bearing, not cosmetic: `SurfaceResolver::resolve_link_surface`
    /// sorts and deduplicates the effective ldflags, so a two-token
    /// `["-T", "layout.ld"]` would be reordered into nonsense and the path
    /// handed to the driver as a free-standing argument. Every flag has to
    /// survive an arbitrary permutation on its own.
    #[test]
    fn every_link_control_flag_is_a_single_sort_safe_argv_token() {
        let flags = image().link_control_flags(Path::new("/pkg"));
        assert_eq!(flags.len(), 3, "{flags:?}");
        for flag in &flags {
            assert!(
                flag.starts_with('-') && !flag.contains(char::is_whitespace),
                "`{flag}` would not survive sorting the link surface"
            );
        }
    }

    /// A header-only target is neither compiled nor linked. Accepting these
    /// keys there would drop them silently -- exactly how `frameworks` came
    /// to be parsed, propagated and reported without ever reaching the
    /// linker.
    #[test]
    fn header_only_targets_reject_build_mode_keys() {
        for mutate in [
            (|t: &mut Target| t.freestanding = true) as fn(&mut Target),
            |t: &mut Target| t.linker_script = Some(PathBuf::from("l.ld")),
            |t: &mut Target| t.entry = Some("_start".to_string()),
        ] {
            let mut t = Target::headeronly("hdrs");
            mutate(&mut t);
            let err = t.validate().unwrap_err().to_string();
            assert!(err.contains("header-only"), "{err}");
        }
    }

    /// A static library is archived by `ar`, so no linker ever sees it: a
    /// linker script or entry symbol declared there would vanish.
    /// `freestanding` is different -- it changes how the library's own
    /// translation units compile -- and stays allowed.
    #[test]
    fn a_static_library_may_be_freestanding_but_has_no_linker() {
        let mut ok = Target::staticlib("bare");
        ok.freestanding = true;
        ok.validate()
            .expect("freestanding affects this library's compile");

        let mut with_script = Target::staticlib("bare");
        with_script.linker_script = Some(PathBuf::from("l.ld"));
        let err = with_script.validate().unwrap_err().to_string();
        assert!(err.contains("archived"), "{err}");

        let mut with_entry = Target::staticlib("bare");
        with_entry.entry = Some("_start".to_string());
        assert!(with_entry.validate().is_err());
    }

    /// `-Wl,-T,a,b.ld` reaches the driver as `-T`, `a`, `b.ld`: the path
    /// arrives in pieces and the linker is handed a stray argument. Silent
    /// corruption, so it is a manifest error rather than something to emit
    /// and hope about.
    #[test]
    fn a_linker_script_path_containing_a_comma_is_refused() {
        let mut t = Target::exe("payload");
        t.linker_script = Some(PathBuf::from("boot/rev,2/layout.ld"));

        let err = t.validate().unwrap_err().to_string();
        assert!(err.contains("comma"), "{err}");
    }

    #[test]
    fn test_target_kind_extensions() {
        assert_eq!(TargetKind::Exe.extension("linux"), "");
        assert_eq!(TargetKind::Exe.extension("windows"), "exe");
        assert_eq!(TargetKind::StaticLib.extension("linux"), "a");
        assert_eq!(TargetKind::StaticLib.extension("windows"), "lib");
        assert_eq!(TargetKind::SharedLib.extension("linux"), "so");
        assert_eq!(TargetKind::SharedLib.extension("macos"), "dylib");
        assert_eq!(TargetKind::SharedLib.extension("windows"), "dll");
    }

    #[test]
    fn test_output_filename() {
        assert_eq!(TargetKind::Exe.output_filename("myapp", "linux"), "myapp");
        assert_eq!(
            TargetKind::Exe.output_filename("myapp", "windows"),
            "myapp.exe"
        );
        assert_eq!(
            TargetKind::StaticLib.output_filename("mylib", "linux"),
            "libmylib.a"
        );
        assert_eq!(
            TargetKind::SharedLib.output_filename("mylib", "macos"),
            "libmylib.dylib"
        );
    }

    #[test]
    fn test_target_builder() {
        let target = Target::staticlib("mylib")
            .with_sources(["src/**/*.c"])
            .with_public_headers(["include/**/*.h"]);

        assert_eq!(target.name.as_str(), "mylib");
        assert_eq!(target.kind, TargetKind::StaticLib);
        assert_eq!(target.sources, vec!["src/**/*.c"]);
        assert_eq!(target.public_headers, vec!["include/**/*.h"]);
    }
}
