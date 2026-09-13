//! Dependency specification.
//!
//! A Dependency describes what a package requires from another package,
//! including version constraints and source information.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use semver::VersionReq;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::core::source_id::{GitReference, SourceId};
use crate::util::InternedString;

/// A dependency specification.
#[derive(Debug, Clone)]
pub struct Dependency {
    /// Package name
    name: InternedString,

    /// Version requirement
    version_req: VersionReq,

    /// Where to find the package
    source_id: SourceId,

    /// Whether this is optional
    optional: bool,

    /// Features to enable
    features: Vec<String>,

    /// Whether default features are enabled
    default_features: bool,
}

impl Dependency {
    /// Create a new dependency.
    pub fn new(name: impl Into<InternedString>, source_id: SourceId) -> Self {
        Dependency {
            name: name.into(),
            version_req: VersionReq::STAR,
            source_id,
            optional: false,
            features: Vec::new(),
            default_features: true,
        }
    }

    /// Create a dependency with a version requirement.
    pub fn with_version_req(mut self, req: VersionReq) -> Self {
        self.version_req = req;
        self
    }

    /// Set whether this dependency is optional.
    pub fn optional(mut self, optional: bool) -> Self {
        self.optional = optional;
        self
    }

    /// Set features to enable.
    pub fn with_features(mut self, features: Vec<String>) -> Self {
        self.features = features;
        self
    }

    /// Set whether default features are enabled.
    pub fn with_default_features(mut self, enabled: bool) -> Self {
        self.default_features = enabled;
        self
    }

    /// Get the package name.
    pub fn name(&self) -> InternedString {
        self.name
    }

    /// Get the version requirement.
    pub fn version_req(&self) -> &VersionReq {
        &self.version_req
    }

    /// Get the source ID.
    pub fn source_id(&self) -> SourceId {
        self.source_id
    }

    /// Check if this is an optional dependency.
    pub fn is_optional(&self) -> bool {
        self.optional
    }

    /// Get the features to enable.
    pub fn features(&self) -> &[String] {
        &self.features
    }

    /// Check if default features are enabled.
    pub fn uses_default_features(&self) -> bool {
        self.default_features
    }

    /// Check if a version matches this dependency's requirement.
    pub fn matches_version(&self, version: &semver::Version) -> bool {
        self.version_req.matches(version)
    }

    /// Check if this is a path dependency.
    pub fn is_path(&self) -> bool {
        self.source_id.is_path()
    }

    /// Check if this is a git dependency.
    pub fn is_git(&self) -> bool {
        self.source_id.is_git()
    }

    /// Check if this is a registry dependency.
    pub fn is_registry(&self) -> bool {
        self.source_id.is_registry()
    }
}

/// Dependency specification as it appears in Harbor.toml.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DependencySpec {
    /// Simple version string: `foo = "1.0"`
    Simple(String),

    /// Detailed specification.
    ///
    /// Boxed because it is an order of magnitude larger than `Simple`, and an
    /// un-boxed variant made every `DependencySpec` -- including the common
    /// `foo = "1.0"` case -- pay the detailed variant's footprint.
    Detailed(Box<DetailedDependencySpec>),
}

impl DependencySpec {
    /// Build a detailed spec, boxing the payload.
    pub fn detailed(spec: DetailedDependencySpec) -> Self {
        DependencySpec::Detailed(Box::new(spec))
    }
}

/// Detailed dependency specification.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DetailedDependencySpec {
    /// Version requirement
    #[serde(default)]
    pub version: Option<String>,

    /// Path to local dependency
    #[serde(default)]
    pub path: Option<PathBuf>,

    /// Git repository URL
    #[serde(default)]
    pub git: Option<String>,

    /// Git branch
    #[serde(default)]
    pub branch: Option<String>,

    /// Git tag
    #[serde(default)]
    pub tag: Option<String>,

    /// Git revision
    #[serde(default)]
    pub rev: Option<String>,

    /// Registry URL (uses default registry if not specified)
    #[serde(default)]
    pub registry: Option<String>,

    /// Vcpkg dependency (port name is the dependency key)
    #[serde(default)]
    pub vcpkg: Option<bool>,

    /// Vcpkg triplet override
    #[serde(default)]
    pub triplet: Option<String>,

    /// Vcpkg library name override
    #[serde(default)]
    pub libs: Option<Vec<String>>,

    /// Vcpkg features to enable (e.g., ["wayland", "x11"])
    #[serde(default)]
    pub vcpkg_features: Option<Vec<String>>,

    /// Vcpkg baseline commit for reproducibility
    #[serde(default)]
    pub vcpkg_baseline: Option<String>,

    /// Vcpkg registry name (references [vcpkg.registries.NAME] in config)
    #[serde(default)]
    pub vcpkg_registry: Option<String>,

    /// Whether this dependency is optional
    #[serde(default)]
    pub optional: Option<bool>,

    /// Features to enable
    #[serde(default)]
    pub features: Option<Vec<String>>,

    /// Whether to use default features.
    ///
    /// Spelled with a hyphen, as Cargo does and as the rest of this schema
    /// does (`default-members`). It was not, and there was no rename, so
    /// `default-features = false` -- the only spelling anyone writes, the
    /// one this crate's own tests use, and the one `manifest.rs` lists in
    /// its "you meant the package-level table" hint -- was absorbed as an
    /// unknown key and thrown away: the dependency was built with its
    /// default features on regardless. Verified by running: with
    /// `default-features = false` the dependency's opt-in define was still
    /// on the compile line, and with `default_features = false` it was not.
    /// The underscore form stays accepted as an alias, because manifests
    /// written against the working spelling must keep working.
    #[serde(default, rename = "default-features", alias = "default_features")]
    pub default_features: Option<bool>,

    /// Inherit from [workspace.dependencies]
    #[serde(default)]
    pub workspace: Option<bool>,

    /// Anything else written in the table.
    ///
    /// `deny_unknown_fields` cannot do this job: this struct is reached
    /// through the `untagged` [`DependencySpec`], and an untagged variant
    /// that fails to deserialize is not an error -- it is a signal to try
    /// the next variant. So `deny_unknown_fields` here turns `brnach = "x"`
    /// into "data did not match any variant of untagged enum
    /// DependencySpec", which names neither the key nor the dependency.
    /// Collecting the remainder and rejecting it by hand is what produces an
    /// error a manifest author can act on. Same reasoning, and same shape,
    /// as `RawTargetDepDetailed` in `manifest.rs`.
    #[serde(flatten, default)]
    pub unknown: std::collections::BTreeMap<String, toml::Value>,
}

impl DependencySpec {
    /// Reject keys that parse and reach nothing. See
    /// [`DetailedDependencySpec::validate_implemented`].
    pub fn validate_implemented(&self, name: &str) -> anyhow::Result<()> {
        match self {
            // The string form is a bare version requirement; there is
            // nowhere in it to write a key that does nothing.
            DependencySpec::Simple(_) => Ok(()),
            DependencySpec::Detailed(spec) => spec.validate_implemented(name),
        }
    }

    /// Reject keys that mean nothing in `[workspace.dependencies]`. See
    /// [`DetailedDependencySpec::validate_no_optional_in_workspace_table`].
    pub fn validate_no_optional_in_workspace_table(&self, name: &str) -> anyhow::Result<()> {
        match self {
            DependencySpec::Simple(_) => Ok(()),
            DependencySpec::Detailed(spec) => spec.validate_no_optional_in_workspace_table(name),
        }
    }
}

impl DetailedDependencySpec {
    /// Reject keys that parse and reach nothing.
    ///
    /// Misspelled keys are the whole of it now. `optional = true` used to be
    /// refused here, because what it silently did was the opposite of what it
    /// says: the dependency was resolved, fetched, built and linked exactly
    /// as if the key were absent. It is implemented as of
    /// [#108](https://github.com/aryamurray/harbour/issues/108) -- an
    /// optional dependency defines a feature of its own name, and is not
    /// fetched unless some enabled feature activates it (see
    /// `core::features` and `ops::resolve::resolve_fresh`).
    ///
    /// Called on every `[dependencies]` and `[workspace.dependencies]` entry
    /// as the manifest is parsed, so a dependency's own manifest is checked
    /// too -- a package whose author wrote it cannot see the consumer's
    /// build, which is precisely the case a silent no-op serves worst.
    pub fn validate_implemented(&self, name: &str) -> anyhow::Result<()> {
        // A misspelled key used to parse and vanish. `brnach = "main"` meant
        // the default branch, `verison = "1.2"` meant "any version", and
        // `feautres = ["x"]` meant no features -- each a silent, plausible,
        // wrong build rather than an error. This is the highest-impact of
        // the three tables that still accepted typos, because the value it
        // drops decides *which source is fetched*.
        if !self.unknown.is_empty() {
            let unexpected: Vec<&str> = self.unknown.keys().map(|k| k.as_str()).collect();

            // The `-` spellings TOML users reach for out of habit, and the
            // target-dep keys that belong in `[targets.X.deps]`.
            let misplaced: Vec<(&str, &str)> = unexpected
                .iter()
                .filter_map(|k| match *k {
                    "vcpkg-features" => Some((*k, "vcpkg_features")),
                    "vcpkg-baseline" => Some((*k, "vcpkg_baseline")),
                    "vcpkg-registry" => Some((*k, "vcpkg_registry")),
                    "compile" | "link" | "target" => Some((*k, "[targets.NAME.deps]")),
                    _ => None,
                })
                .collect();

            let mut hint = String::from(
                "hint: a `[dependencies]` entry takes `version`, `path`, `git`, \
                 `branch`, `tag`, `rev`, `registry`, `features`, \
                 `default-features`, `workspace`, and the `vcpkg*` keys",
            );
            for (wrong, right) in misplaced {
                if right.starts_with('[') {
                    hint.push_str(&format!(
                        "\nnote: `{wrong}` says how a *target* consumes a \
                         dependency and belongs in `{right}`, not here"
                    ));
                } else {
                    hint.push_str(&format!("\nnote: `{wrong}` is spelled `{right}`"));
                }
            }

            anyhow::bail!(
                "unknown key(s) in `[dependencies]` entry `{name}`: {}\n{hint}",
                unexpected.join(", ")
            );
        }

        Ok(())
    }

    /// Reject `optional` in `[workspace.dependencies]`.
    ///
    /// `optional = true` is not a property of the dependency; it is a
    /// property of the *relationship* between one package and it, and it
    /// only means anything alongside that package's own `[features]` table
    /// -- which is per-member. Cargo draws the line in the same place.
    ///
    /// Refusing it rather than inheriting it is what keeps one field from
    /// having two readers that disagree. `resolve_dependency` applies
    /// workspace inheritance and would hand the resolver
    /// `optional = true`, so the dependency would be pruned; but
    /// `surface_resolver::optional_dependency_names` reads the member's own
    /// *raw* spec, where `{ workspace = true }` says nothing about
    /// optionality, so the member's implicit feature of that name would not
    /// exist. The member's `[features]` could not switch on the dependency
    /// the workspace had made optional.
    ///
    /// The fix is not to thread workspace context into one more consumer.
    /// It is that the key does not belong in that table: write it on the
    /// member's own entry, next to the `[features]` that activates it.
    pub fn validate_no_optional_in_workspace_table(&self, name: &str) -> anyhow::Result<()> {
        if self.optional.is_some() {
            anyhow::bail!(
                "`[workspace.dependencies]` entry `{name}`: `optional` cannot be set here\n\
                 hint: `optional` pairs with the `[features]` table that activates the \
                 dependency, and that table belongs to the member, not the workspace. \
                 Write it on the member's own entry:\n\
                 \n    \
                 [dependencies]\n    \
                 {name} = {{ workspace = true, optional = true }}\n\
                 \n\
                 Everything else -- `version`, `path`, `git`, `features`, \
                 `default-features` -- still inherits."
            );
        }
        Ok(())
    }

    /// Check if this spec has an explicit source selector (path/git/registry).
    pub fn has_explicit_source(&self) -> bool {
        self.path.is_some()
            || self.git.is_some()
            || self.registry.is_some()
            || self.vcpkg == Some(true)
    }

    /// Validate that workspace = true is not combined with explicit sources.
    pub fn validate_workspace_field(&self, name: &str) -> anyhow::Result<()> {
        if self.workspace == Some(true) {
            if self.has_explicit_source() {
                anyhow::bail!(
                    "dependency `{}` cannot specify `workspace = true` with `path`, `git`, `registry`, or `vcpkg`",
                    name
                );
            }
            if self.version.is_some() {
                anyhow::bail!(
                    "dependency `{}` cannot specify `workspace = true` with `version`",
                    name
                );
            }
        }
        Ok(())
    }
}

impl DetailedDependencySpec {
    /// Convert to a `Dependency`, anchoring any relative `path` at
    /// `anchor_dir`.
    ///
    /// Deliberately **private**, and the only remaining half of what used
    /// to be two routes from a spec to a `Dependency`. This one knows
    /// nothing about `[workspace.dependencies]`, so a `{ workspace = true }`
    /// entry reaching it fails with "must specify `path`, `git`,
    /// `registry`, `vcpkg`, or `version`" -- which is exactly what happened
    /// to every workspace-inherited dependency that passed through
    /// `Package::summary` (#133, bug 2). The only public route is now
    /// [`resolve_dependency`], which takes a [`DepContext`] and so forces
    /// every caller to say what workspace (if any) governs the manifest.
    ///
    /// `anchor_dir` is a parameter rather than "the manifest directory"
    /// because the two are not always the same: an entry inherited from
    /// `[workspace.dependencies]` is declared in the workspace root's
    /// manifest, so its relative paths anchor there, not at the member that
    /// wrote `{ workspace = true }` (#133, bug 1).
    fn to_dependency_at(
        &self,
        name: &str,
        anchor_dir: &std::path::Path,
        default_registry: &str,
    ) -> anyhow::Result<Dependency> {
        let source_id = if let Some(ref path) = self.path {
            // Path dependency
            let full_path = if path.is_absolute() {
                path.clone()
            } else {
                anchor_dir.join(path)
            };
            SourceId::for_path(&full_path)?
        } else if let Some(ref git_url) = self.git {
            // Git dependency
            let url = Url::parse(git_url)?;
            let reference = if let Some(ref branch) = self.branch {
                GitReference::Branch(branch.clone())
            } else if let Some(ref tag) = self.tag {
                GitReference::Tag(tag.clone())
            } else if let Some(ref rev) = self.rev {
                GitReference::Rev(rev.clone())
            } else {
                GitReference::DefaultBranch
            };
            SourceId::for_git(&url, reference)?
        } else if self.vcpkg == Some(true) {
            SourceId::for_vcpkg(
                name,
                self.triplet.as_deref(),
                self.libs.as_deref(),
                self.vcpkg_features.as_deref(),
                self.vcpkg_baseline.as_deref(),
                self.vcpkg_registry.as_deref(),
            )?
        } else if self.registry.is_some() || self.version.is_some() {
            // Registry dependency (explicit registry or version-only implies registry)
            crate::sources::registry::validate_package_name(name)?;

            let registry_url = if let Some(ref url) = self.registry {
                Url::parse(url)?
            } else {
                Url::parse(default_registry)?
            };
            SourceId::for_registry(&registry_url)?
        } else {
            anyhow::bail!(
                "dependency `{}` must specify `path`, `git`, `registry`, `vcpkg`, or `version`",
                name
            );
        };

        let version_req = if let Some(ref v) = self.version {
            v.parse()?
        } else {
            VersionReq::STAR
        };

        let mut dep = Dependency::new(name, source_id).with_version_req(version_req);

        if let Some(opt) = self.optional {
            dep = dep.optional(opt);
        }

        if let Some(ref features) = self.features {
            dep = dep.with_features(features.clone());
        }

        if let Some(default_features) = self.default_features {
            dep = dep.with_default_features(default_features);
        }

        Ok(dep)
    }
}

impl std::fmt::Display for Dependency {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name)?;
        if self.version_req != VersionReq::STAR {
            write!(f, " {}", self.version_req)?;
        }
        Ok(())
    }
}

/// The `[workspace.dependencies]` table a `{ workspace = true }` entry
/// inherits from, paired with the directory its relative paths anchor to.
///
/// The two travel together because they are not independently knowable and
/// getting the second one wrong is invisible until a build fails. The
/// anchor is the **workspace root**, not the inheriting member: the entry
/// is written in the workspace root's manifest, so `path = "vendored"`
/// there means `<workspace root>/vendored`. Harbour used to hand the
/// member's own directory down into the inherited spec, which turned that
/// into `<workspace root>/app/vendored` and failed with
/// "path does not exist" (#133, bug 1) -- and the only spelling that got
/// past it, `path = "../vendored"`, was nonsense from the root's position.
#[derive(Debug, Clone, Copy)]
pub struct WorkspaceDeps<'a> {
    table: &'a crate::core::manifest::DeclOrderMap<String, DependencySpec>,
    root: &'a Path,
}

impl<'a> WorkspaceDeps<'a> {
    /// `table` is `[workspace.dependencies]`; `root` is the directory
    /// holding the manifest that declares it.
    pub fn new(
        table: &'a crate::core::manifest::DeclOrderMap<String, DependencySpec>,
        root: &'a Path,
    ) -> Self {
        WorkspaceDeps { table, root }
    }
}

/// No sibling members, shared by every standalone context.
fn no_members() -> &'static HashMap<InternedString, PathBuf> {
    static EMPTY: std::sync::OnceLock<HashMap<InternedString, PathBuf>> =
        std::sync::OnceLock::new();
    EMPTY.get_or_init(HashMap::new)
}

/// Everything a [`DependencySpec`] needs in order to become a
/// [`Dependency`].
///
/// This type exists to make the workspace question unavoidable. There used
/// to be two routes from a spec to a dependency -- `resolve_dependency`,
/// which knew about `[workspace.dependencies]` and workspace members, and
/// `DependencySpec::to_dependency`, which did not -- and nothing forced a
/// caller to pick the right one. `Package::summary` picked the second, so
/// every `{ workspace = true }` dependency died there with "must specify
/// `path`, `git`, `registry`, `vcpkg`, or `version`" even though the
/// resolver's own pass over the same manifest had inherited it correctly
/// (#133, bug 2). Rather than teach a second consumer about workspaces,
/// the context-free route is now private: the only way in is
/// [`resolve_dependency`], and building a `DepContext` means answering
/// "which workspace governs this manifest?" explicitly -- with
/// [`DepContext::standalone`] if the honest answer is "none".
#[derive(Debug, Clone, Copy)]
pub struct DepContext<'a> {
    /// Directory of the manifest that declared the entry.
    manifest_dir: &'a Path,
    /// The inheritable table, if this manifest is governed by a workspace.
    workspace: Option<WorkspaceDeps<'a>>,
    /// Sibling workspace members, by name, for local-first matching.
    members: &'a HashMap<InternedString, PathBuf>,
    /// Registry a dependency naming none resolves against.
    default_registry: &'a str,
}

impl<'a> DepContext<'a> {
    /// A manifest governed by a workspace. `members` is `None` when there
    /// is no workspace to have members.
    pub fn new(
        manifest_dir: &'a Path,
        workspace: Option<WorkspaceDeps<'a>>,
        members: Option<&'a HashMap<InternedString, PathBuf>>,
        default_registry: &'a str,
    ) -> Self {
        DepContext {
            manifest_dir,
            workspace,
            members: members.unwrap_or(no_members()),
            default_registry,
        }
    }

    /// A manifest with no workspace above it: nothing to inherit from and
    /// no sibling members. A `{ workspace = true }` entry resolved in this
    /// context is an error, and should be.
    pub fn standalone(manifest_dir: &'a Path, default_registry: &'a str) -> Self {
        DepContext {
            manifest_dir,
            workspace: None,
            members: no_members(),
            default_registry,
        }
    }
}

/// Resolve a dependency with workspace context. The **only** route from a
/// [`DependencySpec`] to a [`Dependency`]; see [`DepContext`].
///
/// Resolution order of precedence:
/// 1. Explicit source selector (path/git/registry) → use directly
/// 2. Name matches workspace member → implicit path dependency
/// 3. `workspace = true` → inherit from `[workspace.dependencies]`
/// 4. Else → registry lookup
///
/// Note: `version = "..."` does NOT skip local-first. Version is a constraint only, not a source selector.
pub fn resolve_dependency(
    name: &str,
    spec: &DependencySpec,
    ctx: &DepContext<'_>,
) -> anyhow::Result<Dependency> {
    match spec {
        DependencySpec::Simple(version) => {
            // Check if name matches a workspace member (local-first)
            let member_name = InternedString::new(name);
            if let Some(member_path) = ctx.members.get(&member_name) {
                // Implicit path dependency to sibling
                let source_id = SourceId::for_path(member_path)?;
                let version_req: VersionReq = version.parse()?;
                return Ok(Dependency::new(name, source_id).with_version_req(version_req));
            }

            // Otherwise, it's a registry dependency
            let version_req: VersionReq = version.parse()?;
            crate::sources::registry::validate_package_name(name)?;
            let registry_url = Url::parse(ctx.default_registry)?;
            let source_id = SourceId::for_registry(&registry_url)?;
            Ok(Dependency::new(name, source_id).with_version_req(version_req))
        }
        DependencySpec::Detailed(spec) => resolve_detailed_dependency(name, spec, ctx),
    }
}

/// Resolve a detailed dependency specification with workspace context.
fn resolve_detailed_dependency(
    name: &str,
    spec: &DetailedDependencySpec,
    ctx: &DepContext<'_>,
) -> anyhow::Result<Dependency> {
    // Validate workspace field constraints
    spec.validate_workspace_field(name)?;

    // 1. Explicit source selector takes precedence
    if spec.has_explicit_source() {
        return spec.to_dependency_at(name, ctx.manifest_dir, ctx.default_registry);
    }

    // 2. Check if name matches workspace member (local-first) - only if no explicit source
    let member_name = InternedString::new(name);
    if let Some(member_path) = ctx.members.get(&member_name) {
        let source_id = SourceId::for_path(member_path)?;
        let version_req = if let Some(ref v) = spec.version {
            v.parse()?
        } else {
            VersionReq::STAR
        };

        let mut dep = Dependency::new(name, source_id).with_version_req(version_req);

        if let Some(opt) = spec.optional {
            dep = dep.optional(opt);
        }
        if let Some(ref features) = spec.features {
            dep = dep.with_features(features.clone());
        }
        if let Some(default_features) = spec.default_features {
            dep = dep.with_default_features(default_features);
        }

        return Ok(dep);
    }

    // 3. `workspace = true` → inherit from [workspace.dependencies]
    if spec.workspace == Some(true) {
        let ws = ctx.workspace.ok_or_else(|| {
            anyhow::anyhow!(
                "dependency `{}` specifies `workspace = true` but no workspace dependencies are defined",
                name
            )
        })?;

        let ws_spec = ws.table.get(name).ok_or_else(|| {
            anyhow::anyhow!(
                "dependency `{}` specifies `workspace = true` but `{}` is not in [workspace.dependencies]",
                name,
                name
            )
        })?;

        // Resolve the workspace spec first, anchored at the *workspace
        // root* -- that is where the entry is written, so that is what its
        // relative paths mean. `workspace: None` because the table cannot
        // inherit from itself; a `{ workspace = true }` inside
        // `[workspace.dependencies]` is a cycle, not a redirect, and this
        // is what makes it an error rather than infinite recursion.
        let root_ctx = DepContext {
            manifest_dir: ws.root,
            workspace: None,
            members: ctx.members,
            default_registry: ctx.default_registry,
        };
        let mut dep = resolve_dependency(name, ws_spec, &root_ctx)?;

        // Apply local overrides (features additive, optional additive only)
        if let Some(ref local_features) = spec.features {
            // Merge features (additive)
            let mut features: Vec<String> = dep.features().to_vec();
            for f in local_features {
                if !features.contains(f) {
                    features.push(f.clone());
                }
            }
            dep = dep.with_features(features);
        }

        // Optionality is the member's to decide, and only the member's:
        // `[workspace.dependencies]` refuses `optional` outright (see
        // `validate_no_optional_in_workspace_table`), so there is nothing
        // inherited to reconcile with. This used to be a "can only
        // increase" merge, which was the wrong shape -- it let the
        // workspace declare an optionality that
        // `surface_resolver::optional_dependency_names` could not see,
        // because that reads the member's raw spec and a bare
        // `{ workspace = true }` says nothing about it.
        if let Some(local_optional) = spec.optional {
            dep = dep.optional(local_optional);
        }

        return Ok(dep);
    }

    // 4. Else → registry lookup (via version or default)
    spec.to_dependency_at(name, ctx.manifest_dir, ctx.default_registry)
}

/// Refuse a `[workspace.dependencies]` key that names a workspace member.
///
/// This used to warn that the key "may cause unexpected behavior", which
/// understated it in one direction and overstated it in the other. The
/// behaviour is not unexpected, it is fully determined and it is *nothing*:
/// local-first matching against members is step 2 of
/// [`resolve_dependency`] and inheritance is step 3, so a member of that
/// name always wins and the workspace entry is never read. Its `path`,
/// `version`, `features` and `default-features` are silently discarded.
///
/// A key that cannot take effect is the same defect class as `optional` in
/// this table (#134) and `[targets.X.backend]` (#131): a field that parses,
/// validates and reaches nothing. As there, the fix is to narrow the schema
/// rather than to leave a warning standing in for a rule. The entry is
/// redundant when it points at the member and a lie when it points
/// anywhere else, and neither is worth preserving.
pub fn validate_workspace_deps_do_not_name_members(
    workspace_deps: &crate::core::manifest::DeclOrderMap<String, DependencySpec>,
    workspace_members: &HashMap<InternedString, PathBuf>,
) -> anyhow::Result<()> {
    for dep_name in workspace_deps.keys() {
        let name = InternedString::new(dep_name);
        if let Some(member_path) = workspace_members.get(&name) {
            anyhow::bail!(
                "`[workspace.dependencies]` entry `{dep_name}` names a workspace member \
                 (`{}`)\n\
                 hint: a member of that name always wins, so this entry can never be \
                 used -- its `path`, `version` and `features` are discarded. Delete it; \
                 members depend on each other by name:\n\
                 \n    \
                 [dependencies]\n    \
                 {dep_name} = \"*\"\n\
                 \n\
                 If you meant a *different* package that happens to share the name, \
                 rename one of them.",
                member_path.display()
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::context::DEFAULT_REGISTRY_URL;
    use tempfile::TempDir;

    #[test]
    fn test_dependency_creation() {
        let tmp = TempDir::new().unwrap();
        let source = SourceId::for_path(tmp.path()).unwrap();
        let dep = Dependency::new("mylib", source)
            .with_version_req("^1.0".parse().unwrap())
            .optional(true);

        assert_eq!(dep.name().as_str(), "mylib");
        assert!(dep.is_optional());
        assert!(dep.is_path());
    }

    /// `default-features` is the spelling Cargo uses, the spelling this
    /// crate's own tests use, and -- until this was fixed -- the spelling
    /// Harbour threw away: the field was named `default_features` with no
    /// rename, so the hyphenated key was absorbed as unknown and the
    /// dependency kept its default features. Both spellings now work, and
    /// the hyphen is canonical.
    #[test]
    fn default_features_accepts_the_hyphenated_spelling() {
        for key in ["default-features", "default_features"] {
            let spec: DetailedDependencySpec =
                toml::from_str(&format!("path = \".\"\n{key} = false\n"))
                    .unwrap_or_else(|e| panic!("`{key}` must parse: {e}"));
            assert_eq!(
                spec.default_features,
                Some(false),
                "`{key} = false` must reach the field, not the unknown-key bucket"
            );
            assert!(
                spec.unknown.is_empty(),
                "`{key}` is a real key: {:?}",
                spec.unknown
            );
        }
    }

    /// A misspelled key in a `[dependencies]` entry decides *which source is
    /// fetched*, so dropping it silently is the worst of the three tables
    /// that used to: `brnach` meant the default branch, `verison` meant any
    /// version.
    #[test]
    fn unknown_keys_in_a_dependency_entry_are_collected_and_rejected() {
        let spec: DetailedDependencySpec = toml::from_str(
            "git = \"https://example.com/r\"\nbrnach = \"main\"\nverison = \"1.2\"\n",
        )
        .expect("the catch-all absorbs them rather than failing the untagged enum");
        assert_eq!(spec.unknown.len(), 2, "{:?}", spec.unknown);

        let err = spec.validate_implemented("mylib").unwrap_err().to_string();
        assert!(err.contains("brnach") && err.contains("verison"), "{err}");
        assert!(err.contains("mylib"), "the dependency must be named: {err}");
        assert!(
            err.contains("branch") && err.contains("version"),
            "and the real keys listed: {err}"
        );
    }

    /// A target-dep key written in the package-level table is a confusion of
    /// two tables rather than a typo, and gets its own note -- the mirror of
    /// the hint `[targets.X.deps]` already gives for `path`/`version`.
    #[test]
    fn a_target_dep_key_in_the_package_table_says_which_table_it_belongs_to() {
        let spec: DetailedDependencySpec =
            toml::from_str("path = \".\"\ncompile = \"private\"\n").unwrap();
        let err = spec.validate_implemented("mylib").unwrap_err().to_string();
        assert!(
            err.contains("compile") && err.contains("targets.NAME.deps"),
            "{err}"
        );
    }

    #[test]
    fn test_dependency_spec_detailed() {
        let tmp = TempDir::new().unwrap();
        let spec = DependencySpec::detailed(DetailedDependencySpec {
            path: Some(PathBuf::from(".")),
            version: Some("^1.0".to_string()),
            optional: Some(true),
            ..Default::default()
        });

        let dep = resolve_dependency(
            "test",
            &spec,
            &DepContext::standalone(tmp.path(), DEFAULT_REGISTRY_URL),
        )
        .unwrap();
        assert_eq!(dep.name().as_str(), "test");
        assert!(dep.is_optional());
    }

    /// The default registry used to be process-global (a `OnceLock` written
    /// by the first `GlobalContext`), so within one process every
    /// registry dependency naming no registry resolved against whichever
    /// context happened to be built first -- unobservable in the CLI, but it
    /// made per-context registry configuration untestable and leaked between
    /// unit tests. Threading it as a parameter is what makes this assertion
    /// possible.
    #[test]
    fn registry_dep_honors_the_default_registry_it_is_given() {
        let tmp = TempDir::new().unwrap();
        let spec = DependencySpec::Simple("^1.0".to_string());

        let a = resolve_dependency(
            "zlib",
            &spec,
            &DepContext::standalone(tmp.path(), "https://registry.example/a/"),
        )
        .unwrap();
        let b = resolve_dependency(
            "zlib",
            &spec,
            &DepContext::standalone(tmp.path(), "https://registry.example/b/"),
        )
        .unwrap();

        assert_ne!(
            a.source_id(),
            b.source_id(),
            "the same spec resolved under two different default registries \
             must yield two different sources"
        );
        assert_eq!(a.source_id().url().as_str(), "https://registry.example/a/");
        assert_eq!(b.source_id().url().as_str(), "https://registry.example/b/");
    }

    #[test]
    fn test_dependency_spec_git() {
        let tmp = TempDir::new().unwrap();
        let spec = DependencySpec::detailed(DetailedDependencySpec {
            git: Some("https://github.com/user/repo".to_string()),
            tag: Some("v1.0".to_string()),
            ..Default::default()
        });

        let dep = resolve_dependency(
            "test",
            &spec,
            &DepContext::standalone(tmp.path(), DEFAULT_REGISTRY_URL),
        )
        .unwrap();
        assert!(dep.is_git());
        assert_eq!(
            dep.source_id().git_reference(),
            Some(&GitReference::Tag("v1.0".to_string()))
        );
    }

    #[test]
    fn test_resolve_local_first() {
        let tmp = TempDir::new().unwrap();

        // Set up workspace members
        let mut members = HashMap::new();
        members.insert(InternedString::new("sibling"), tmp.path().to_path_buf());

        // Dependency matching a member name should be local-first
        let spec = DependencySpec::Simple("1.0".to_string());
        let dep = resolve_dependency(
            "sibling",
            &spec,
            &DepContext::new(tmp.path(), None, Some(&members), DEFAULT_REGISTRY_URL),
        )
        .unwrap();

        assert!(dep.is_path());
        assert_eq!(dep.name().as_str(), "sibling");
    }

    #[test]
    fn test_resolve_explicit_registry_overrides_local() {
        let tmp = TempDir::new().unwrap();

        // Set up workspace members
        let mut members = HashMap::new();
        members.insert(InternedString::new("sibling"), tmp.path().to_path_buf());

        // Explicit registry should override local-first
        let spec = DependencySpec::detailed(DetailedDependencySpec {
            registry: Some("https://example.com/registry".to_string()),
            version: Some("1.0".to_string()),
            ..Default::default()
        });
        let dep = resolve_dependency(
            "sibling",
            &spec,
            &DepContext::new(tmp.path(), None, Some(&members), DEFAULT_REGISTRY_URL),
        )
        .unwrap();

        assert!(dep.is_registry());
    }

    #[test]
    fn test_resolve_workspace_inheritance() {
        let tmp = TempDir::new().unwrap();
        let members = HashMap::new();

        // Set up workspace dependencies
        let mut ws_deps = crate::core::manifest::DeclOrderMap::new();
        ws_deps.insert(
            "inherited".to_string(),
            DependencySpec::detailed(DetailedDependencySpec {
                git: Some("https://github.com/user/inherited".to_string()),
                tag: Some("v2.0".to_string()),
                features: Some(vec!["feature1".to_string()]),
                ..Default::default()
            }),
        );

        // Member uses workspace = true
        let spec = DependencySpec::detailed(DetailedDependencySpec {
            workspace: Some(true),
            features: Some(vec!["feature2".to_string()]),
            ..Default::default()
        });

        let dep = resolve_dependency(
            "inherited",
            &spec,
            &DepContext::new(
                tmp.path(),
                Some(WorkspaceDeps::new(&ws_deps, tmp.path())),
                Some(&members),
                DEFAULT_REGISTRY_URL,
            ),
        )
        .unwrap();

        assert!(dep.is_git());
        // Features should be merged
        assert!(dep.features().contains(&"feature1".to_string()));
        assert!(dep.features().contains(&"feature2".to_string()));
    }

    #[test]
    fn test_workspace_true_with_path_error() {
        let tmp = TempDir::new().unwrap();
        let members = HashMap::new();

        let spec = DependencySpec::detailed(DetailedDependencySpec {
            workspace: Some(true),
            path: Some(PathBuf::from("../other")),
            ..Default::default()
        });

        let result = resolve_dependency(
            "test",
            &spec,
            &DepContext::new(tmp.path(), None, Some(&members), DEFAULT_REGISTRY_URL),
        );
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("cannot specify `workspace = true` with `path`"));
    }

    /// `optional` is refused in `[workspace.dependencies]`, so the merge
    /// that used to reconcile an inherited value with a local one is gone.
    /// The member's own entry is the only place it can be written, and it is
    /// taken at face value.
    #[test]
    fn optional_is_refused_in_the_workspace_dependencies_table() {
        let spec = DependencySpec::detailed(DetailedDependencySpec {
            version: Some("1.0".to_string()),
            optional: Some(true),
            ..Default::default()
        });
        let err = spec
            .validate_no_optional_in_workspace_table("optdep")
            .unwrap_err()
            .to_string();
        assert!(err.contains("`optional` cannot be set here"), "{err}");
        assert!(
            err.contains("workspace = true, optional = true"),
            "the hint must show where it goes instead: {err}"
        );

        // Including `optional = false`, which is the default but would
        // still be a key in a table that cannot honour it.
        let spec = DependencySpec::detailed(DetailedDependencySpec {
            version: Some("1.0".to_string()),
            optional: Some(false),
            ..Default::default()
        });
        assert!(spec
            .validate_no_optional_in_workspace_table("optdep")
            .is_err());

        // Everything else in the table is still fine.
        let spec = DependencySpec::detailed(DetailedDependencySpec {
            version: Some("1.0".to_string()),
            features: Some(vec!["x".to_string()]),
            ..Default::default()
        });
        spec.validate_no_optional_in_workspace_table("optdep")
            .expect("only `optional` is refused");
    }

    /// A relative `path` in `[workspace.dependencies]` is anchored at the
    /// workspace root, because that is the manifest it is written in.
    ///
    /// Written so that it fails on the *anchor* and not on existence: both
    /// `<root>/vendored` and `<root>/app/vendored` exist, so the old code
    /// did not error, it succeeded and pointed at the wrong directory.
    /// (With only the root one present the old code failed too, but with
    /// "path does not exist", which is indistinguishable from a typo in the
    /// manifest -- exactly the confusion #133 describes.)
    ///
    /// The expectation is built with `Path::join` rather than a `/`-spelled
    /// literal, and compared as a `Path`, so it means the same thing on
    /// Windows.
    #[test]
    fn an_inherited_relative_path_anchors_at_the_workspace_root() {
        let tmp = TempDir::new().unwrap();
        let ws_root = tmp.path();
        let member_dir = ws_root.join("app");
        std::fs::create_dir_all(&member_dir).unwrap();
        std::fs::create_dir_all(ws_root.join("vendored")).unwrap();
        std::fs::create_dir_all(member_dir.join("vendored")).unwrap();

        let mut ws_deps = crate::core::manifest::DeclOrderMap::new();
        ws_deps.insert(
            "vendored".to_string(),
            DependencySpec::detailed(DetailedDependencySpec {
                path: Some(PathBuf::from("vendored")),
                ..Default::default()
            }),
        );

        let spec = DependencySpec::detailed(DetailedDependencySpec {
            workspace: Some(true),
            ..Default::default()
        });

        let members = HashMap::new();
        let dep = resolve_dependency(
            "vendored",
            &spec,
            &DepContext::new(
                &member_dir,
                Some(WorkspaceDeps::new(&ws_deps, ws_root)),
                Some(&members),
                DEFAULT_REGISTRY_URL,
            ),
        )
        .unwrap();

        assert!(dep.is_path());
        assert_eq!(
            dep.source_id().path().expect("a path dependency"),
            ws_root.join("vendored"),
            "an entry declared in the workspace root's manifest anchors \
             there, not at the member that wrote `workspace = true`"
        );
    }

    /// `{ workspace = true }` inside `[workspace.dependencies]` is a cycle,
    /// not a redirect. The inherited spec is resolved with no workspace of
    /// its own, so this is an error rather than unbounded recursion.
    #[test]
    fn a_workspace_entry_cannot_itself_inherit_from_the_workspace() {
        let tmp = TempDir::new().unwrap();
        let mut ws_deps = crate::core::manifest::DeclOrderMap::new();
        ws_deps.insert(
            "loop".to_string(),
            DependencySpec::detailed(DetailedDependencySpec {
                workspace: Some(true),
                ..Default::default()
            }),
        );

        let spec = DependencySpec::detailed(DetailedDependencySpec {
            workspace: Some(true),
            ..Default::default()
        });

        let members = HashMap::new();
        let err = resolve_dependency(
            "loop",
            &spec,
            &DepContext::new(
                tmp.path(),
                Some(WorkspaceDeps::new(&ws_deps, tmp.path())),
                Some(&members),
                DEFAULT_REGISTRY_URL,
            ),
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("no workspace dependencies are defined"),
            "{err}"
        );
    }

    /// A `[workspace.dependencies]` key that names a member is refused,
    /// because local-first matching means it can never be read.
    #[test]
    fn a_workspace_dependency_naming_a_member_is_refused() {
        let tmp = TempDir::new().unwrap();
        let mut ws_deps = crate::core::manifest::DeclOrderMap::new();
        ws_deps.insert(
            "vendored".to_string(),
            DependencySpec::detailed(DetailedDependencySpec {
                path: Some(PathBuf::from("elsewhere")),
                ..Default::default()
            }),
        );

        let mut members = HashMap::new();
        members.insert(InternedString::new("vendored"), tmp.path().to_path_buf());

        let err = validate_workspace_deps_do_not_name_members(&ws_deps, &members)
            .unwrap_err()
            .to_string();
        assert!(err.contains("names a workspace member"), "{err}");
        assert!(err.contains("vendored"), "and names the entry: {err}");

        // A key that names no member is fine.
        let mut other = HashMap::new();
        other.insert(InternedString::new("app"), tmp.path().to_path_buf());
        validate_workspace_deps_do_not_name_members(&ws_deps, &other)
            .expect("only a key that collides with a member is refused");
    }

    /// A member's own `optional` is taken at face value, and the rest of the
    /// workspace entry still inherits around it.
    #[test]
    fn a_member_can_mark_an_inherited_dependency_optional() {
        let tmp = TempDir::new().unwrap();
        let members = HashMap::new();

        let mut ws_deps = crate::core::manifest::DeclOrderMap::new();
        ws_deps.insert(
            "optdep".to_string(),
            DependencySpec::detailed(DetailedDependencySpec {
                version: Some("1.0".to_string()),
                features: Some(vec!["base".to_string()]),
                ..Default::default()
            }),
        );

        let spec = DependencySpec::detailed(DetailedDependencySpec {
            workspace: Some(true),
            optional: Some(true),
            ..Default::default()
        });

        let dep = resolve_dependency(
            "optdep",
            &spec,
            &DepContext::new(
                tmp.path(),
                Some(WorkspaceDeps::new(&ws_deps, tmp.path())),
                Some(&members),
                DEFAULT_REGISTRY_URL,
            ),
        )
        .unwrap();
        assert!(dep.is_optional());
        assert_eq!(
            dep.version_req().to_string(),
            "^1.0",
            "the version still inherits"
        );
        assert_eq!(
            dep.features(),
            ["base".to_string()],
            "and so do the features"
        );
    }
}
