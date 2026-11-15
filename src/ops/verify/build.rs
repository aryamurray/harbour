//! The throwaway consumer project, and the build that goes through it.
//!
//! `verify` does not build a package: it builds a *consumer* of the package,
//! the way a user's `harbour add <pkg> && harbour build` does, and asks what
//! came out. The package is fetched, patched, resolved and compiled by the
//! same code a consumer reaches -- default target, sibling closure, prebuild
//! generators, host tools, probes, public include dirs -- because none of it is
//! reimplemented here. The earlier design had its own build driver that wrote a
//! manifest of its own over the package's, and so verified a different library
//! (or none) from the one anybody could install.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use toml::Value;

use super::types::{VerifyLinkage, VerifyOptions};
use crate::builder::linkage::LinkagePreference;
use crate::builder::util::Artifact;
use crate::core::target::{Target, TargetTriple};
use crate::core::workspace::MANIFEST_NAME;
use crate::core::{Package, Workspace};
use crate::ops::harbour_build::{build, BuildOptions, BuildResult};
use crate::resolver::encode::Lockfile;
use crate::sources::SourceCache;
use crate::util::config::VcpkgConfig;
use crate::util::context::GlobalContext;

/// Name of the consumer package.
const CONSUMER_PACKAGE: &str = "harbour-verify-consumer";

/// Name of the consumer's executable target: the harness.
pub(crate) const HARNESS_TARGET: &str = "harbour-verify-harness";

/// Directory, under the consumer, that holds the harness's sources. Replaced
/// wholesale each time the harness changes.
const HARNESS_DIR: &str = "harness";

/// One source file of the harness.
pub(crate) struct HarnessSource {
    /// File name under the harness directory.
    pub file: String,
    pub content: String,
}

/// The consumer project on disk.
pub(crate) struct Consumer {
    dir: PathBuf,
    /// Removes the directory on drop when it is ours to remove.
    _temp: Option<tempfile::TempDir>,
}

impl Consumer {
    /// A fresh consumer directory: `--output` if given (and kept, so the
    /// project can be inspected afterwards), else a temporary one.
    ///
    /// Never reused across runs by default. A warm build directory would turn
    /// the compile counts into "0 compiled", and a stale object file is
    /// exactly what a merge gate must not trust; the *source* cache is what
    /// stays warm.
    pub fn create(options: &VerifyOptions) -> Result<Self> {
        match &options.output_dir {
            Some(dir) => {
                std::fs::create_dir_all(dir)
                    .with_context(|| format!("failed to create {}", dir.display()))?;
                Ok(Consumer {
                    dir: crate::util::fs::normalize_path(dir),
                    _temp: None,
                })
            }
            None => {
                let temp = tempfile::Builder::new()
                    .prefix("harbour-verify-")
                    .tempdir()
                    .context("failed to create a directory for the verification project")?;
                Ok(Consumer {
                    dir: crate::util::fs::normalize_path(temp.path()),
                    _temp: Some(temp),
                })
            }
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// A second project inside this one, for the stage that has to resolve
    /// before the package's language is known. A source cache keeps each path
    /// project's parsed manifest, so a project that is rewritten has to be a
    /// different one (or a different cache); this is the cheaper of the two.
    pub fn nested(&self, name: &str) -> Consumer {
        Consumer {
            dir: self.dir.join(name),
            _temp: None,
        }
    }

    pub fn manifest_path(&self) -> PathBuf {
        self.dir.join(MANIFEST_NAME)
    }

    /// Write the manifest, once.
    ///
    /// The dependency is the plain `name = "requirement"` form, so the package
    /// is looked up in the consumer's default registry like any other. The
    /// harness is named by glob, so changing the harness changes files and not
    /// the manifest: a source cache holds the consumer's parsed manifest, and
    /// one that changed under it would be served stale.
    pub fn write_manifest(&self, package: &str, requirement: &str, cxx: bool) -> Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let mut package_table = toml::Table::new();
        package_table.insert("name".into(), Value::String(CONSUMER_PACKAGE.into()));
        package_table.insert("version".into(), Value::String("0.0.0".into()));

        let mut dependencies = toml::Table::new();
        dependencies.insert(package.into(), Value::String(requirement.into()));

        let mut target = toml::Table::new();
        target.insert("kind".into(), Value::String("exe".into()));
        target.insert(
            "lang".into(),
            Value::String(if cxx { "c++" } else { "c" }.into()),
        );
        target.insert(
            "sources".into(),
            Value::Array(vec![Value::String(format!(
                "{HARNESS_DIR}/*.{}",
                if cxx { "cpp" } else { "c" }
            ))]),
        );
        let mut targets = toml::Table::new();
        targets.insert(HARNESS_TARGET.into(), Value::Table(target));

        let mut doc = toml::Table::new();
        doc.insert("package".into(), Value::Table(package_table));
        doc.insert("dependencies".into(), Value::Table(dependencies));
        doc.insert("targets".into(), Value::Table(targets));

        std::fs::write(
            self.manifest_path(),
            toml::to_string_pretty(&doc)
                .context("failed to serialize the verification manifest")?,
        )
        .context("failed to write the verification manifest")
    }

    /// Replace the harness's sources with `sources`.
    pub fn write_harness(&self, sources: &[HarnessSource]) -> Result<()> {
        let harness_dir = self.dir.join(HARNESS_DIR);
        if harness_dir.exists() {
            std::fs::remove_dir_all(&harness_dir)
                .with_context(|| format!("failed to clear {}", harness_dir.display()))?;
        }
        std::fs::create_dir_all(&harness_dir)?;
        for source in sources {
            std::fs::write(harness_dir.join(&source.file), &source.content)
                .with_context(|| format!("failed to write harness source {}", source.file))?;
        }
        Ok(())
    }

    /// The consumer as a workspace, built in the `release` profile (what a
    /// package is shipped as).
    pub fn workspace(&self, ctx: &GlobalContext) -> Result<Workspace> {
        Ok(Workspace::new(&self.manifest_path(), ctx)?.with_profile("release"))
    }
}

/// Build the consumer: `harbour build`, through [`build`].
pub(crate) fn build_consumer(
    ws: &Workspace,
    source_cache: &mut SourceCache,
    options: &VerifyOptions,
    ctx: &GlobalContext,
) -> Result<BuildResult> {
    // `Both` has no meaning for one consumer link; it used to warn and build
    // static, and still does.
    let linkage = match options.linkage {
        VerifyLinkage::Auto | VerifyLinkage::Static => LinkagePreference::static_(),
        VerifyLinkage::Shared => LinkagePreference::shared(),
        VerifyLinkage::Both => {
            tracing::warn!(
                "VerifyLinkage::Both currently only tests static linkage. \
                 Run separately with --linkage=shared to test shared linkage."
            );
            LinkagePreference::static_()
        }
    };

    let opts = BuildOptions {
        packages: vec![],
        targets: vec![],
        emit_compile_commands: false,
        emit_plan: false,
        jobs: None,
        verbose: options.verbose || ctx.is_verbose(),
        cpp_std: None,
        linkage,
        ffi: false,
        target_triple: options.target_triple.as_deref().map(TargetTriple::parse),
        locked: false,
        vcpkg: VcpkgConfig::default(),
        strict_sources: options.strict_sources,
        dep_warnings: false,
    };

    build(ws, source_cache, &opts)
}

/// The libraries a consumer links when it asks for the package and names
/// nothing: its default target, and the sibling targets that one reaches
/// through `local = true` edges.
///
/// The resolver's own `dep_members` is the authority; this is the same rule
/// stated on the manifest alone, so verification can say *which* libraries
/// should have been built without a build to ask.
pub(crate) fn default_library_closure(package: &Package) -> Result<Vec<&Target>> {
    let Some(default) = package.default_target() else {
        bail!("package `{}` declares no targets", package.name());
    };

    let all = package.targets();
    let mut closure: Vec<&Target> = vec![default];
    let mut next = 0;
    while next < closure.len() {
        let current = closure[next];
        next += 1;
        for (name, _) in current.local_deps() {
            if let Some(sibling) = all.iter().find(|t| t.name.as_str() == name.as_str()) {
                if !closure.iter().any(|t| t.name == sibling.name) {
                    closure.push(sibling);
                }
            }
        }
    }

    if !closure
        .iter()
        .any(|t| t.kind.is_linkable() || t.kind.is_header_only())
    {
        bail!(
            "package `{}`'s default target `{}` is an executable, so a consumer has \
             nothing to link\n\
             hint: mark the library with `[package] default_target = \"...\"`; its targets \
             are: {}",
            package.name(),
            default.name,
            all.iter()
                .map(|t| format!("{} ({:?})", t.name, t.kind))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(closure)
}

/// The built archives or shared libraries of `closure`, in the build's output.
///
/// A linkable target in the closure with no artifact is a hard failure, and
/// the message lists every target the build did produce, because "nothing came
/// out" and "something else came out" are different bugs.
pub(crate) fn library_artifacts<'a>(
    closure: &[&Target],
    built: &'a [Artifact],
) -> Result<Vec<&'a Artifact>> {
    let mut found = Vec::new();
    let mut missing = Vec::new();
    for target in closure.iter().filter(|t| t.kind.is_linkable()) {
        match built.iter().find(|a| a.target == target.name.as_str()) {
            Some(artifact) => found.push(artifact),
            None => missing.push(target.name.to_string()),
        }
    }

    if !missing.is_empty() {
        bail!(
            "no library artifact was produced for target(s) {}\n\
             targets that were built: {}\n\
             hint: a library target with no sources compiles nothing; check its `sources` \
             patterns against the fetched tree",
            missing.join(", "),
            if built.is_empty() {
                "(none)".to_string()
            } else {
                built
                    .iter()
                    .map(|a| format!("{} ({})", a.target, a.path.display()))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        );
    }
    Ok(found)
}

/// The translation units the lockfile records for `package`, which is what the
/// plan compiled for it. Empty if it recorded none.
pub(crate) fn translation_units(ws: &Workspace, package: &str) -> Vec<String> {
    let Ok(lock) = Lockfile::load(&ws.lockfile_path()) else {
        return Vec::new();
    };
    let mut files: Vec<String> = lock
        .packages
        .iter()
        .filter(|p| p.name == package)
        .flat_map(|p| p.sources.values().flatten().cloned())
        .collect();
    files.sort();
    files.dedup();
    files
}

/// The include directories the harness compiles with: the consumer target's
/// effective compile surface, from the same fold the build plan uses.
///
/// This is what lets the default harness name a header the way a consumer
/// would write it (`openssl/sha.h`, not a path into the cache).
pub(crate) fn consumer_include_dirs(
    ws: &Workspace,
    source_cache: &mut SourceCache,
    options: &VerifyOptions,
) -> Result<Vec<PathBuf>> {
    use crate::builder::surface_resolver::SurfaceResolver;
    use crate::builder::BuildContext;

    let target_triple = options.target_triple.as_deref().map(TargetTriple::parse);
    let resolve = crate::ops::resolve::resolve_workspace(ws, source_cache)?;
    let build_ctx = BuildContext::new_with_vcpkg(
        ws,
        ws.profile(),
        &VcpkgConfig::default(),
        target_triple.as_ref(),
    )?;
    let mut surface = SurfaceResolver::new(&resolve, &build_ctx.platform)
        .with_feature_selection(build_ctx.feature_selection.clone());
    surface.load_packages(source_cache)?;

    let root = ws.root_package();
    let target = root
        .target(HARNESS_TARGET)
        .context("the verification project lost its harness target")?;
    Ok(surface
        .resolve_compile_surface(root.package_id(), target)?
        .include_dirs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn built(target: &str) -> Artifact {
        Artifact {
            path: PathBuf::from(format!("deps/lib{target}.a")),
            target: target.to_string(),
        }
    }

    #[test]
    fn a_missing_library_artifact_lists_what_was_built() {
        let a = Target::staticlib("a");
        let b = Target::staticlib("b");
        let closure = [&a, &b];

        let both = [built("a"), built("b")];
        let ok = library_artifacts(&closure, &both).unwrap();
        assert_eq!(ok.len(), 2);

        let partial = [built("a"), built("harness")];
        let err = library_artifacts(&closure, &partial)
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("target(s) b"), "{err}");
        assert!(
            err.contains("a (") && err.contains("harness ("),
            "the targets that were built are listed: {err}"
        );

        let none = library_artifacts(&closure, &[]).err().unwrap().to_string();
        assert!(none.contains("(none)"), "{none}");
    }

    #[test]
    fn a_header_only_target_needs_no_artifact() {
        let mut header_only = Target::staticlib("hdr");
        header_only.kind = crate::core::target::TargetKind::HeaderOnly;
        let found = library_artifacts(&[&header_only], &[]).unwrap();
        assert!(found.is_empty());
    }
}
