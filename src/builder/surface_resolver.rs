//! Surface propagation algorithm.
//!
//! This module computes the effective compile and link surfaces for a target
//! by propagating public surfaces from dependencies.

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;

use anyhow::Result;
use thiserror::Error;

use crate::core::surface::{
    CompileRequirements, Define, LibRef, LinkRequirements, TargetPlatform,
};
use crate::core::target::Visibility;
use crate::core::{Package, PackageId, Target};
use crate::resolver::Resolve;
use crate::sources::SourceCache;

/// Errors that can occur during surface resolution.
#[derive(Debug, Error)]
pub enum SurfaceResolveError {
    /// A dependency specified in target.deps was not found in the resolve graph.
    #[error("in target `{target_name}`: dependency `{dep_name}` not found\n\
             help: add `{dep_name}` to [dependencies]")]
    DependencyNotFound { target_name: String, dep_name: String },

    /// A dependency name is ambiguous (multiple packages with same name from different sources).
    #[error("in target `{target_name}`: dependency `{dep_name}` is ambiguous\n\
             candidates: {candidates:?}\n\
             help: disambiguate by source in target deps")]
    DependencyAmbiguous {
        target_name: String,
        dep_name: String,
        candidates: Vec<String>,
    },

    /// A target specified in target.deps was not found in the dependency package.
    #[error("in target `{target_name}`: target `{dep_target}` not found in `{dep_pkg}`\n\
             available: {available:?}")]
    TargetNotFound {
        target_name: String,
        dep_pkg: String,
        dep_target: String,
        available: Vec<String>,
    },
}

/// Indicates where a flag/setting originated from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provenance {
    /// The package that contributed this value.
    pub package_id: PackageId,
    /// Which surface section it came from.
    pub surface_kind: SurfaceKind,
}

impl fmt::Display for Provenance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {} ({})",
            self.package_id.name(),
            self.package_id.version(),
            self.surface_kind
        )
    }
}

/// Indicates which surface section a value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceKind {
    CompilePublic,
    CompilePrivate,
    LinkPublic,
    LinkPrivate,
}

impl fmt::Display for SurfaceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SurfaceKind::CompilePublic => write!(f, "surface.compile.public"),
            SurfaceKind::CompilePrivate => write!(f, "surface.compile.private"),
            SurfaceKind::LinkPublic => write!(f, "surface.link.public"),
            SurfaceKind::LinkPrivate => write!(f, "surface.link.private"),
        }
    }
}

/// A value paired with its provenance information.
#[derive(Debug, Clone)]
pub struct WithProvenance<T> {
    pub value: T,
    pub provenance: Provenance,
}

impl<T> WithProvenance<T> {
    pub fn new(value: T, package_id: PackageId, surface_kind: SurfaceKind) -> Self {
        WithProvenance {
            value,
            provenance: Provenance {
                package_id,
                surface_kind,
            },
        }
    }
}

/// Resolved compile environment with provenance tracking.
#[derive(Debug, Clone, Default)]
pub struct EffectiveCompileSurfaceWithProvenance {
    pub include_dirs: Vec<WithProvenance<PathBuf>>,
    pub defines: Vec<WithProvenance<Define>>,
    pub cflags: Vec<WithProvenance<String>>,
}

impl EffectiveCompileSurfaceWithProvenance {
    /// Convert to compiler flags (for actual compilation, without provenance).
    pub fn to_flags(&self) -> Vec<String> {
        let mut flags = Vec::new();

        for item in &self.include_dirs {
            flags.push(format!("-I{}", item.value.display()));
        }

        for item in &self.defines {
            flags.push(item.value.to_flag());
        }

        for item in &self.cflags {
            flags.push(item.value.clone());
        }

        flags
    }
}

/// Resolved link environment with provenance tracking.
#[derive(Debug, Clone, Default)]
pub struct EffectiveLinkSurfaceWithProvenance {
    pub libs: Vec<WithProvenance<LibRef>>,
    pub lib_dirs: Vec<WithProvenance<PathBuf>>,
    pub ldflags: Vec<WithProvenance<String>>,
    pub frameworks: Vec<WithProvenance<String>>,
    pub dep_libs: Vec<WithProvenance<PathBuf>>,
}

impl EffectiveLinkSurfaceWithProvenance {
    /// Convert to linker flags (for actual linking, without provenance).
    pub fn to_flags(&self) -> Vec<String> {
        let mut flags = Vec::new();

        // Library search paths
        for item in &self.lib_dirs {
            flags.push(format!("-L{}", item.value.display()));
        }

        // Built dependency libraries (full paths)
        for item in &self.dep_libs {
            flags.push(item.value.display().to_string());
        }

        // System libraries
        for item in &self.libs {
            flags.extend(item.value.to_flags());
        }

        // Frameworks
        for item in &self.frameworks {
            flags.push("-framework".to_string());
            flags.push(item.value.clone());
        }

        // Additional flags
        for item in &self.ldflags {
            flags.push(item.value.clone());
        }

        flags
    }
}

/// Resolved compile environment for a target.
#[derive(Debug, Clone, Default)]
pub struct EffectiveCompileSurface {
    /// Include directories
    pub include_dirs: Vec<PathBuf>,
    /// Preprocessor defines
    pub defines: Vec<Define>,
    /// Compiler flags
    pub cflags: Vec<String>,
}

/// Resolved link environment for a target.
#[derive(Debug, Clone, Default)]
pub struct EffectiveLinkSurface {
    /// Libraries to link
    pub libs: Vec<LibRef>,
    /// Library search paths
    pub lib_dirs: Vec<PathBuf>,
    /// Linker flags
    pub ldflags: Vec<String>,
    /// Frameworks (macOS)
    pub frameworks: Vec<String>,
    /// Built dependency libraries (paths to .a/.so files)
    pub dep_libs: Vec<PathBuf>,
    /// Link groups for controlling link order
    pub groups: Vec<crate::core::surface::LinkGroup>,
}

/// Resolves effective surfaces for targets.
pub struct SurfaceResolver<'a> {
    resolve: &'a Resolve,
    platform: &'a TargetPlatform,
    packages: HashMap<PackageId, Package>,
}

impl<'a> SurfaceResolver<'a> {
    /// Create a new surface resolver.
    pub fn new(resolve: &'a Resolve, platform: &'a TargetPlatform) -> Self {
        SurfaceResolver {
            resolve,
            platform,
            packages: HashMap::new(),
        }
    }

    /// Load packages for all resolved dependencies.
    pub fn load_packages(&mut self, source_cache: &mut SourceCache) -> Result<()> {
        for (pkg_id, _) in self.resolve.packages() {
            if !self.packages.contains_key(pkg_id) {
                let package = source_cache.load_package(*pkg_id)?;
                self.packages.insert(*pkg_id, package);
            }
        }
        Ok(())
    }

    /// Get a loaded package by ID.
    pub fn get_package(&self, pkg_id: PackageId) -> Option<&Package> {
        self.packages.get(&pkg_id)
    }

    /// Compute the effective compile surface for a target.
    ///
    /// Algorithm:
    /// 1. Validate that all deps in target.deps exist in the resolve graph
    /// 2. Start with target's private compile surface
    /// 3. Add target's public compile surface
    /// 4. For each dependency (transitively):
    ///    - Check target.deps for visibility override
    ///    - If public (or not overridden), add dependency's public compile surface
    pub fn resolve_compile_surface(
        &self,
        pkg_id: PackageId,
        target: &Target,
    ) -> Result<EffectiveCompileSurface> {
        let mut effective = EffectiveCompileSurface::default();

        // Get package
        let package = self
            .packages
            .get(&pkg_id)
            .ok_or_else(|| anyhow::anyhow!("package not loaded: {}", pkg_id))?;

        // Validate target.deps - ensure all referenced deps exist in resolve
        for (dep_name, _dep_spec) in &target.deps {
            match self.resolve.get_package_by_name_strict(*dep_name) {
                Ok(_) => { /* found, continue */ }
                Err(crate::resolver::ResolveError::PackageNotFound { .. }) => {
                    return Err(SurfaceResolveError::DependencyNotFound {
                        target_name: target.name.to_string(),
                        dep_name: dep_name.to_string(),
                    }
                    .into());
                }
                Err(crate::resolver::ResolveError::AmbiguousPackage { sources, .. }) => {
                    return Err(SurfaceResolveError::DependencyAmbiguous {
                        target_name: target.name.to_string(),
                        dep_name: dep_name.to_string(),
                        candidates: sources.split(", ").map(|s| s.to_string()).collect(),
                    }
                    .into());
                }
                Err(e) => {
                    return Err(e.into());
                }
            }
        }

        // Resolve the target's surface
        let resolved = target.surface.resolve(self.platform);

        // Add private (only for this target's sources)
        self.add_compile_requirements(&mut effective, &resolved.compile_private, package.root());

        // Add public
        self.add_compile_requirements(&mut effective, &resolved.compile_public, package.root());

        // Determine effective dependencies - use target.deps if specified
        let transitive_deps = self.resolve.transitive_deps(pkg_id);

        for dep_id in transitive_deps {
            // Check if target.deps specifies visibility for this dependency
            let visibility = self.get_compile_visibility(target, dep_id);

            // Only include if public visibility
            if visibility == Visibility::Private {
                continue;
            }

            if let Some(dep_package) = self.packages.get(&dep_id) {
                // Get the specific target if specified in target.deps
                let dep_target = self.get_dep_target(target, dep_id, dep_package)?;
                if let Some(dt) = dep_target {
                    let dep_resolved = dt.surface.resolve(self.platform);
                    self.add_compile_requirements(
                        &mut effective,
                        &dep_resolved.compile_public,
                        dep_package.root(),
                    );
                }
            }
        }

        // Deduplicate
        effective.include_dirs.sort();
        effective.include_dirs.dedup();
        effective.cflags.sort();
        effective.cflags.dedup();

        Ok(effective)
    }

    /// Get compile visibility for a dependency from target.deps.
    /// Returns Public if not specified (default).
    /// O(1) lookup using HashMap.
    fn get_compile_visibility(&self, target: &Target, dep_id: PackageId) -> Visibility {
        target
            .deps
            .get(&dep_id.name())
            .map(|spec| spec.compile)
            .unwrap_or(Visibility::Public)
    }

    /// Get link visibility for a dependency from target.deps.
    /// Returns Public if not specified (default).
    /// O(1) lookup using HashMap.
    fn get_link_visibility(&self, target: &Target, dep_id: PackageId) -> Visibility {
        target
            .deps
            .get(&dep_id.name())
            .map(|spec| spec.link)
            .unwrap_or(Visibility::Public)
    }

    /// Get the target to use from a dependency package.
    /// Respects target.deps[pkg].target if specified, otherwise uses default_target.
    /// O(1) lookup using HashMap.
    ///
    /// Returns an error if a specific target was requested but not found.
    fn get_dep_target<'b>(
        &self,
        target: &Target,
        dep_id: PackageId,
        dep_package: &'b Package,
    ) -> Result<Option<&'b Target>, SurfaceResolveError> {
        // Check if target.deps specifies a specific target (O(1) lookup)
        if let Some(dep_spec) = target.deps.get(&dep_id.name()) {
            if let Some(ref target_name) = dep_spec.target {
                return match dep_package.target(target_name) {
                    Some(t) => Ok(Some(t)),
                    None => Err(SurfaceResolveError::TargetNotFound {
                        target_name: target.name.to_string(),
                        dep_pkg: dep_id.name().to_string(),
                        dep_target: target_name.clone(),
                        available: dep_package
                            .targets()
                            .iter()
                            .map(|t| t.name.to_string())
                            .collect(),
                    }),
                };
            }
        }

        // Fall back to default target
        Ok(dep_package.default_target())
    }

    /// Compute the effective link surface for a target.
    ///
    /// Algorithm:
    /// 1. Start with target's private link surface
    /// 2. Add target's public link surface
    /// 3. For each dependency (in topological order):
    ///    - Check target.deps for visibility override
    ///    - If public (or not overridden), add the built library and public link surface
    pub fn resolve_link_surface(
        &self,
        pkg_id: PackageId,
        target: &Target,
        deps_dir: &std::path::Path,
    ) -> Result<EffectiveLinkSurface> {
        let mut effective = EffectiveLinkSurface::default();

        // Get package
        let _package = self
            .packages
            .get(&pkg_id)
            .ok_or_else(|| anyhow::anyhow!("package not loaded: {}", pkg_id))?;

        // Resolve the target's surface
        let resolved = target.surface.resolve(self.platform);

        // Add private
        self.add_link_requirements(&mut effective, &resolved.link_private);

        // Add public
        self.add_link_requirements(&mut effective, &resolved.link_public);

        // Add dependencies in topological order (dependencies before dependents)
        // This ensures correct link order
        let deps_order = self.resolve.topological_order();
        for dep_id in deps_order {
            if dep_id == pkg_id {
                continue;
            }

            // Check if this is a transitive dependency
            if !self.resolve.transitive_deps(pkg_id).contains(&dep_id) {
                continue;
            }

            // Check if target.deps specifies visibility for this dependency
            let visibility = self.get_link_visibility(target, dep_id);

            // Only include if public visibility
            if visibility == Visibility::Private {
                continue;
            }

            if let Some(dep_package) = self.packages.get(&dep_id) {
                // Get the specific target if specified in target.deps
                let dep_target = self.get_dep_target(target, dep_id, dep_package)?;
                if let Some(dt) = dep_target {
                    // Add the built library
                    if dt.kind.is_linkable() {
                        let lib_dir = deps_dir
                            .join(format!("{}-{}", dep_id.name(), dep_id.version()))
                            .join("lib");

                        let lib_file = lib_dir.join(dt.output_filename(self.platform.os.as_str()));

                        // Include even if not built yet
                        effective.dep_libs.push(lib_file);
                        effective.lib_dirs.push(lib_dir);
                    }

                    // Add public link surface
                    let dep_resolved = dt.surface.resolve(self.platform);
                    self.add_link_requirements(&mut effective, &dep_resolved.link_public);
                }
            }
        }

        // Deduplicate
        effective.lib_dirs.sort();
        effective.lib_dirs.dedup();
        effective.ldflags.sort();
        effective.ldflags.dedup();
        effective.frameworks.sort();
        effective.frameworks.dedup();

        Ok(effective)
    }

    fn add_compile_requirements(
        &self,
        effective: &mut EffectiveCompileSurface,
        reqs: &CompileRequirements,
        root: &std::path::Path,
    ) {
        // Make include dirs absolute
        for dir in &reqs.include_dirs {
            let abs_dir = if dir.is_absolute() {
                dir.clone()
            } else {
                root.join(dir)
            };
            effective.include_dirs.push(abs_dir);
        }

        effective.defines.extend(reqs.defines.iter().cloned());
        effective.cflags.extend(reqs.cflags.iter().cloned());
    }

    fn add_link_requirements(&self, effective: &mut EffectiveLinkSurface, reqs: &LinkRequirements) {
        effective.libs.extend(reqs.libs.iter().cloned());
        effective.ldflags.extend(reqs.ldflags.iter().cloned());
        effective.frameworks.extend(reqs.frameworks.iter().cloned());
        effective.groups.extend(reqs.groups.iter().cloned());
    }

    /// Compute the effective compile surface with provenance tracking.
    ///
    /// Same algorithm as `resolve_compile_surface`, but tracks where each
    /// flag came from for display purposes.
    pub fn resolve_compile_surface_with_provenance(
        &self,
        pkg_id: PackageId,
        target: &Target,
    ) -> Result<EffectiveCompileSurfaceWithProvenance> {
        let mut effective = EffectiveCompileSurfaceWithProvenance::default();

        // Get package
        let package = self
            .packages
            .get(&pkg_id)
            .ok_or_else(|| anyhow::anyhow!("package not loaded: {}", pkg_id))?;

        // Resolve the target's surface
        let resolved = target.surface.resolve(self.platform);

        // Add private (only for this target's sources)
        self.add_compile_requirements_with_provenance(
            &mut effective,
            &resolved.compile_private,
            package.root(),
            pkg_id,
            SurfaceKind::CompilePrivate,
        );

        // Add public
        self.add_compile_requirements_with_provenance(
            &mut effective,
            &resolved.compile_public,
            package.root(),
            pkg_id,
            SurfaceKind::CompilePublic,
        );

        // Add transitive public surfaces from dependencies
        let transitive_deps = self.resolve.transitive_deps(pkg_id);
        for dep_id in transitive_deps {
            if let Some(dep_package) = self.packages.get(&dep_id) {
                if let Some(dep_target) = dep_package.default_target() {
                    let dep_resolved = dep_target.surface.resolve(self.platform);
                    self.add_compile_requirements_with_provenance(
                        &mut effective,
                        &dep_resolved.compile_public,
                        dep_package.root(),
                        dep_id,
                        SurfaceKind::CompilePublic,
                    );
                }
            }
        }

        Ok(effective)
    }

    /// Compute the effective link surface with provenance tracking.
    ///
    /// Same algorithm as `resolve_link_surface`, but tracks where each
    /// flag came from for display purposes.
    pub fn resolve_link_surface_with_provenance(
        &self,
        pkg_id: PackageId,
        target: &Target,
        deps_dir: &std::path::Path,
    ) -> Result<EffectiveLinkSurfaceWithProvenance> {
        let mut effective = EffectiveLinkSurfaceWithProvenance::default();

        // Get package
        let _package = self
            .packages
            .get(&pkg_id)
            .ok_or_else(|| anyhow::anyhow!("package not loaded: {}", pkg_id))?;

        // Resolve the target's surface
        let resolved = target.surface.resolve(self.platform);

        // Add private
        self.add_link_requirements_with_provenance(
            &mut effective,
            &resolved.link_private,
            pkg_id,
            SurfaceKind::LinkPrivate,
        );

        // Add public
        self.add_link_requirements_with_provenance(
            &mut effective,
            &resolved.link_public,
            pkg_id,
            SurfaceKind::LinkPublic,
        );

        // Add dependencies in topological order (dependencies before dependents)
        // This ensures correct link order
        let deps_order = self.resolve.topological_order();
        for dep_id in deps_order {
            if dep_id == pkg_id {
                continue;
            }

            // Check if this is a transitive dependency
            if !self.resolve.transitive_deps(pkg_id).contains(&dep_id) {
                continue;
            }

            if let Some(dep_package) = self.packages.get(&dep_id) {
                if let Some(dep_target) = dep_package.default_target() {
                    // Add the built library
                    if dep_target.kind.is_linkable() {
                        let lib_dir = deps_dir
                            .join(format!("{}-{}", dep_id.name(), dep_id.version()))
                            .join("lib");

                        let lib_file =
                            lib_dir.join(dep_target.output_filename(self.platform.os.as_str()));

                        // Include even if not built yet
                        effective.dep_libs.push(WithProvenance::new(
                            lib_file,
                            dep_id,
                            SurfaceKind::LinkPublic,
                        ));
                        effective.lib_dirs.push(WithProvenance::new(
                            lib_dir,
                            dep_id,
                            SurfaceKind::LinkPublic,
                        ));
                    }

                    // Add public link surface
                    let dep_resolved = dep_target.surface.resolve(self.platform);
                    self.add_link_requirements_with_provenance(
                        &mut effective,
                        &dep_resolved.link_public,
                        dep_id,
                        SurfaceKind::LinkPublic,
                    );
                }
            }
        }

        Ok(effective)
    }

    fn add_compile_requirements_with_provenance(
        &self,
        effective: &mut EffectiveCompileSurfaceWithProvenance,
        reqs: &CompileRequirements,
        root: &std::path::Path,
        pkg_id: PackageId,
        surface_kind: SurfaceKind,
    ) {
        // Make include dirs absolute
        for dir in &reqs.include_dirs {
            let abs_dir = if dir.is_absolute() {
                dir.clone()
            } else {
                root.join(dir)
            };
            effective
                .include_dirs
                .push(WithProvenance::new(abs_dir, pkg_id, surface_kind));
        }

        for define in &reqs.defines {
            effective
                .defines
                .push(WithProvenance::new(define.clone(), pkg_id, surface_kind));
        }

        for cflag in &reqs.cflags {
            effective
                .cflags
                .push(WithProvenance::new(cflag.clone(), pkg_id, surface_kind));
        }
    }

    fn add_link_requirements_with_provenance(
        &self,
        effective: &mut EffectiveLinkSurfaceWithProvenance,
        reqs: &LinkRequirements,
        pkg_id: PackageId,
        surface_kind: SurfaceKind,
    ) {
        for lib in &reqs.libs {
            effective
                .libs
                .push(WithProvenance::new(lib.clone(), pkg_id, surface_kind));
        }

        for ldflag in &reqs.ldflags {
            effective
                .ldflags
                .push(WithProvenance::new(ldflag.clone(), pkg_id, surface_kind));
        }

        for framework in &reqs.frameworks {
            effective.frameworks.push(WithProvenance::new(
                framework.clone(),
                pkg_id,
                surface_kind,
            ));
        }
    }
}

impl EffectiveCompileSurface {
    /// Convert to compiler flags.
    pub fn to_flags(&self) -> Vec<String> {
        let mut flags = Vec::new();

        for dir in &self.include_dirs {
            flags.push(format!("-I{}", dir.display()));
        }

        for define in &self.defines {
            flags.push(define.to_flag());
        }

        flags.extend(self.cflags.iter().cloned());

        flags
    }
}

impl EffectiveLinkSurface {
    /// Convert to linker flags.
    pub fn to_flags(&self) -> Vec<String> {
        let mut flags = Vec::new();

        // Library search paths
        for dir in &self.lib_dirs {
            flags.push(format!("-L{}", dir.display()));
        }

        // Built dependency libraries (full paths)
        for lib in &self.dep_libs {
            flags.push(lib.display().to_string());
        }

        // System libraries
        for lib in &self.libs {
            flags.extend(lib.to_flags());
        }

        // Frameworks
        for fw in &self.frameworks {
            flags.push("-framework".to_string());
            flags.push(fw.clone());
        }

        // Additional flags
        flags.extend(self.ldflags.iter().cloned());

        flags
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_effective_compile_surface_to_flags() {
        let surface = EffectiveCompileSurface {
            include_dirs: vec![PathBuf::from("/usr/include"), PathBuf::from("./src")],
            defines: vec![Define::flag("DEBUG"), Define::key_value("VERSION", "1")],
            cflags: vec!["-Wall".to_string()],
        };

        let flags = surface.to_flags();
        assert!(flags.contains(&"-I/usr/include".to_string()));
        assert!(flags.contains(&"-I./src".to_string()));
        assert!(flags.contains(&"-DDEBUG".to_string()));
        assert!(flags.contains(&"-DVERSION=1".to_string()));
        assert!(flags.contains(&"-Wall".to_string()));
    }

    #[test]
    fn test_effective_link_surface_to_flags() {
        let surface = EffectiveLinkSurface {
            libs: vec![LibRef::system("pthread"), LibRef::system("m")],
            lib_dirs: vec![PathBuf::from("/usr/lib")],
            ldflags: vec!["-Wl,-rpath,/opt/lib".to_string()],
            frameworks: vec!["Security".to_string()],
            dep_libs: vec![PathBuf::from("target/deps/foo/libfoo.a")],
            groups: vec![],
        };

        let flags = surface.to_flags();
        assert!(flags.contains(&"-L/usr/lib".to_string()));
        assert!(flags.contains(&"-lpthread".to_string()));
        assert!(flags.contains(&"-lm".to_string()));
        assert!(flags.contains(&"-framework".to_string()));
        assert!(flags.contains(&"Security".to_string()));
    }
}
