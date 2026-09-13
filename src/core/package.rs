//! Package - full package with manifest and targets.
//!
//! A Package combines the manifest with resolved source locations.

use std::path::{Path, PathBuf};

use anyhow::Result;
use semver::Version;

use crate::core::dependency::{resolve_dependency, DepContext, WorkspaceDeps};
use crate::core::workspace::{find_manifest, MANIFEST_NAME};
use crate::core::{Manifest, PackageId, SourceId, Summary, Target};
use crate::util::InternedString;

/// A complete package with its manifest and location.
#[derive(Debug, Clone)]
pub struct Package {
    /// The package ID
    package_id: PackageId,

    /// The parsed manifest
    manifest: Manifest,

    /// Root directory of the package
    root: PathBuf,
}

impl Package {
    /// Create a new package from a manifest and root directory.
    ///
    /// Returns an error if the manifest is a virtual workspace (no [package] section).
    pub fn new(manifest: Manifest, root: PathBuf) -> Result<Self> {
        if manifest.package.is_none() {
            anyhow::bail!(
                "cannot create Package from virtual workspace manifest at {}",
                root.display()
            );
        }
        let version = manifest.version()?;
        let source_id = SourceId::for_path(&root)?;
        let package_id = PackageId::new(manifest.name(), version, source_id);

        Ok(Package {
            package_id,
            manifest,
            root,
        })
    }

    /// Create a package with a specific source ID (for git/registry sources).
    ///
    /// Returns an error if the manifest is a virtual workspace (no [package] section).
    pub fn with_source_id(manifest: Manifest, root: PathBuf, source_id: SourceId) -> Result<Self> {
        if manifest.package.is_none() {
            anyhow::bail!(
                "cannot create Package from virtual workspace manifest at {}",
                root.display()
            );
        }
        let version = manifest.version()?;
        let package_id = PackageId::new(manifest.name(), version, source_id);

        Ok(Package {
            package_id,
            manifest,
            root,
        })
    }

    /// Load a package from a manifest file.
    pub fn load(manifest_path: &Path) -> Result<Self> {
        let manifest = Manifest::load(manifest_path)?;
        let root = manifest_path
            .parent()
            .unwrap_or(Path::new("."))
            .to_path_buf();
        Self::new(manifest, root)
    }

    /// Get the package ID.
    pub fn package_id(&self) -> PackageId {
        self.package_id
    }

    /// Get the package name.
    pub fn name(&self) -> InternedString {
        self.package_id.name()
    }

    /// Get the package version.
    pub fn version(&self) -> &Version {
        self.package_id.version()
    }

    /// Get the source ID.
    pub fn source_id(&self) -> SourceId {
        self.package_id.source_id()
    }

    /// Get the manifest.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Get the package root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Get the manifest file path.
    pub fn manifest_path(&self) -> PathBuf {
        find_manifest(&self.root).unwrap_or_else(|_| self.root.join(MANIFEST_NAME))
    }

    /// Get all targets.
    pub fn targets(&self) -> &[Target] {
        &self.manifest.targets
    }

    /// Get a target by name.
    pub fn target(&self, name: &str) -> Option<&Target> {
        self.manifest.target(name)
    }

    /// The target a dependent gets when it does not name one:
    /// `[package] default_target` if set, else the first declared library,
    /// else the first declared target.
    pub fn default_target(&self) -> Option<&Target> {
        self.manifest.default_target()
    }

    /// Whether this package's author named its default target explicitly.
    pub fn has_explicit_default_target(&self) -> bool {
        self.manifest.explicit_default_target_name().is_some()
    }

    /// Create a summary for this package.
    ///
    /// `default_registry` is the registry that a dependency naming none
    /// (`foo = "1.0"`) resolves against. It is threaded in from the caller's
    /// `GlobalContext` rather than read from a global, so a single process
    /// can hold two contexts with different configured registries.
    ///
    /// # Why this looks up its own workspace
    ///
    /// This used to call the context-free `DependencySpec::to_dependency`,
    /// which is the whole of #133's second bug: `{ workspace = true }`
    /// reached a function that had never heard of
    /// `[workspace.dependencies]` and died with "must specify `path`,
    /// `git`, `registry`, `vcpkg`, or `version`" -- naming keys the author
    /// had deliberately not written. Meanwhile `resolve_workspace`'s own
    /// pass over the identical manifest inherited it correctly, so one
    /// field had two readers that disagreed.
    ///
    /// The fix is not a second workspace-aware parameter threaded through
    /// every `Package` constructor. `resolve_dependency` is now the only
    /// route, and the workspace question is answered from the package's
    /// location by `governing_workspace` -- which is also what makes a
    /// *fetched* package work: a git dependency that is a member of a
    /// workspace inside its own repository inherits from that repository's
    /// root, which no amount of threading from Harbour's workspace could
    /// have supplied.
    pub fn summary(&self, default_registry: &str) -> Result<Summary> {
        let governing = crate::core::workspace::governing_workspace(&self.root, &self.manifest)?;
        let workspace_deps = governing.as_ref().and_then(|ws| {
            ws.manifest
                .workspace
                .as_ref()
                .map(|cfg| WorkspaceDeps::new(&cfg.dependencies, &ws.root))
        });
        let members = governing.as_ref().map(|ws| &ws.members);

        let ctx = DepContext::new(&self.root, workspace_deps, members, default_registry);

        let deps = self
            .manifest
            .dependencies
            .iter()
            .map(|(name, spec)| resolve_dependency(name, spec, &ctx))
            .collect::<Result<Vec<_>>>()?;

        Ok(Summary::new(self.package_id, deps, None))
    }

    /// Get the source directory (typically src/).
    pub fn src_dir(&self) -> PathBuf {
        self.root.join("src")
    }

    /// Get the include directory (typically include/).
    pub fn include_dir(&self) -> PathBuf {
        self.root.join("include")
    }
}

impl std::fmt::Display for Package {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.package_id)
    }
}

impl PartialEq for Package {
    fn eq(&self, other: &Self) -> bool {
        self.package_id == other.package_id
    }
}

impl Eq for Package {}

impl std::hash::Hash for Package {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.package_id.hash(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_manifest(dir: &Path) -> PathBuf {
        let manifest_path = dir.join("Harbor.toml");
        std::fs::write(
            &manifest_path,
            r#"
[package]
name = "testpkg"
version = "1.0.0"

[targets.testpkg]
kind = "staticlib"
sources = ["src/**/*.c"]
"#,
        )
        .unwrap();
        manifest_path
    }

    #[test]
    fn test_package_load() {
        let tmp = TempDir::new().unwrap();
        let manifest_path = create_test_manifest(tmp.path());

        let pkg = Package::load(&manifest_path).unwrap();
        assert_eq!(pkg.name().as_str(), "testpkg");
        assert_eq!(pkg.version(), &Version::new(1, 0, 0));
    }

    #[test]
    fn test_package_summary() {
        let tmp = TempDir::new().unwrap();
        let manifest_path = create_test_manifest(tmp.path());

        let pkg = Package::load(&manifest_path).unwrap();
        let summary = pkg
            .summary(crate::util::context::DEFAULT_REGISTRY_URL)
            .unwrap();

        assert_eq!(summary.name().as_str(), "testpkg");
        assert!(summary.dependencies().is_empty());
    }

    /// `Package::summary` was the second of the two routes from a
    /// `DependencySpec` to a `Dependency`, and the context-free one: a
    /// member inheriting `{ workspace = true }` died here with
    /// "dependency `vendored` must specify `path`, `git`, `registry`,
    /// `vcpkg`, or `version`" -- naming four keys the author had
    /// deliberately not written -- while the resolver's own pass over the
    /// identical manifest inherited it correctly (#133, bug 2).
    ///
    /// Asserting on the resolved *source path* rather than just "one
    /// dependency came back" is what distinguishes inheritance having
    /// happened from inheritance having been skipped: the path can only be
    /// `<workspace root>/vendored` if the workspace entry was both found
    /// and anchored correctly.
    #[test]
    fn summary_inherits_a_workspace_dependency_from_the_enclosing_workspace() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let app = root.join("app");
        let vendored = root.join("vendored");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::create_dir_all(vendored.join("src")).unwrap();

        std::fs::write(
            root.join("Harbour.toml"),
            "[workspace]\nmembers = [\"app\"]\n\n\
             [workspace.dependencies]\nvendored = { path = \"vendored\" }\n",
        )
        .unwrap();
        std::fs::write(
            app.join("Harbour.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
             [dependencies]\nvendored = { workspace = true }\n\n\
             [targets.app]\nkind = \"bin\"\nsources = [\"src/**/*.c\"]\n",
        )
        .unwrap();
        std::fs::write(
            vendored.join("Harbour.toml"),
            "[package]\nname = \"vendored\"\nversion = \"0.1.0\"\n\n\
             [targets.vendored]\nkind = \"staticlib\"\nsources = [\"src/**/*.c\"]\n",
        )
        .unwrap();

        let pkg = Package::load(&app.join("Harbour.toml")).unwrap();
        let summary = pkg
            .summary(crate::util::context::DEFAULT_REGISTRY_URL)
            .expect("a workspace-inherited dependency must resolve here too");

        let deps = summary.dependencies();
        assert_eq!(deps.len(), 1, "{deps:?}");
        assert_eq!(deps[0].name().as_str(), "vendored");
        assert!(deps[0].is_path(), "{:?}", deps[0]);
        assert_eq!(
            deps[0].source_id().path().expect("a path dependency"),
            root.join("vendored"),
            "the inherited path anchors at the workspace root"
        );
    }

    /// A package that is not claimed by any enclosing workspace stays
    /// standalone, so `{ workspace = true }` there is still an error rather
    /// than silently picking up an unrelated workspace's table.
    #[test]
    fn summary_does_not_inherit_from_a_workspace_that_does_not_claim_it() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let stray = root.join("stray");
        std::fs::create_dir_all(&stray).unwrap();
        std::fs::create_dir_all(root.join("vendored")).unwrap();

        // The workspace lists a different member; `stray` is not in it.
        std::fs::write(
            root.join("Harbour.toml"),
            "[workspace]\nmembers = [\"member\"]\n\n\
             [workspace.dependencies]\nvendored = { path = \"vendored\" }\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("member")).unwrap();
        std::fs::write(
            root.join("member").join("Harbour.toml"),
            "[package]\nname = \"member\"\nversion = \"0.1.0\"\n\n\
             [targets.member]\nkind = \"staticlib\"\nsources = [\"src/**/*.c\"]\n",
        )
        .unwrap();
        std::fs::write(
            stray.join("Harbour.toml"),
            "[package]\nname = \"stray\"\nversion = \"0.1.0\"\n\n\
             [dependencies]\nvendored = { workspace = true }\n\n\
             [targets.stray]\nkind = \"bin\"\nsources = [\"src/**/*.c\"]\n",
        )
        .unwrap();

        let pkg = Package::load(&stray.join("Harbour.toml")).unwrap();
        let err = pkg
            .summary(crate::util::context::DEFAULT_REGISTRY_URL)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("no workspace dependencies are defined"),
            "a package the workspace does not list must not inherit from \
             it: {err}"
        );
    }
}
