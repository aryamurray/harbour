//! Surface propagation algorithm.
//!
//! This module computes the effective compile and link surfaces for a target
//! by propagating public surfaces from dependencies.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::PathBuf;

use anyhow::Result;

use crate::core::surface::{
    CompileRequirements, Define, LibRef, LinkRequirements, ResolvedSurface, Surface,
    TargetPlatform,
};
use crate::core::target::Visibility;
use crate::core::{Package, PackageId, Target};
use crate::resolver::Resolve;
use crate::sources::SourceCache;

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
    /// 1. Start with target's private compile surface
    /// 2. Add target's public compile surface
    /// 3. For each dependency (transitively):
    ///    - Add dependency's public compile surface
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

        // Resolve the target's surface
        let resolved = target.surface.resolve(self.platform);

        // Add private (only for this target's sources)
        self.add_compile_requirements(&mut effective, &resolved.compile_private, package.root());

        // Add public
        self.add_compile_requirements(&mut effective, &resolved.compile_public, package.root());

        // Add transitive public surfaces from dependencies
        let transitive_deps = self.resolve.transitive_deps(pkg_id);
        for dep_id in transitive_deps {
            if let Some(dep_package) = self.packages.get(&dep_id) {
                if let Some(dep_target) = dep_package.default_target() {
                    let dep_resolved = dep_target.surface.resolve(self.platform);
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

    /// Compute the effective link surface for a target.
    ///
    /// Algorithm:
    /// 1. Start with target's private link surface
    /// 2. Add target's public link surface
    /// 3. For each dependency (in topological order):
    ///    - Add the built library
    ///    - Add dependency's public link surface
    pub fn resolve_link_surface(
        &self,
        pkg_id: PackageId,
        target: &Target,
        deps_dir: &std::path::Path,
    ) -> Result<EffectiveLinkSurface> {
        let mut effective = EffectiveLinkSurface::default();

        // Get package
        let package = self
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

            if let Some(dep_package) = self.packages.get(&dep_id) {
                if let Some(dep_target) = dep_package.default_target() {
                    // Add the built library
                    if dep_target.kind.is_linkable() {
                        let lib_dir = deps_dir
                            .join(format!("{}-{}", dep_id.name(), dep_id.version()))
                            .join("lib");

                        let lib_file = lib_dir.join(dep_target.output_filename(self.platform.os.as_str()));

                        if lib_file.exists() || true {
                            // Include even if not built yet
                            effective.dep_libs.push(lib_file);
                            effective.lib_dirs.push(lib_dir);
                        }
                    }

                    // Add public link surface
                    let dep_resolved = dep_target.surface.resolve(self.platform);
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
        let package = self
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
        };

        let flags = surface.to_flags();
        assert!(flags.contains(&"-L/usr/lib".to_string()));
        assert!(flags.contains(&"-lpthread".to_string()));
        assert!(flags.contains(&"-lm".to_string()));
        assert!(flags.contains(&"-framework".to_string()));
        assert!(flags.contains(&"Security".to_string()));
    }
}
