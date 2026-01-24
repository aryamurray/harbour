//! C++ constraint computation and enforcement.
//!
//! This module handles:
//! - Standard unification (effective_std = max of all requirements)
//! - Runtime enforcement (graph-wide single value for MSVC)
//! - Exceptions/RTTI (workspace-wide setting from [build] config)

use std::collections::HashMap;

use anyhow::{bail, Result};

use crate::core::manifest::{BuildConfig, CppRuntime, MsvcRuntime};
use crate::core::target::CppStandard;
use crate::core::Package;
use crate::core::PackageId;
use crate::resolver::Resolve;

/// Computed C++ constraints for the build graph.
///
/// These constraints are computed once at the start of a build and apply
/// to the entire dependency graph.
#[derive(Debug, Clone)]
pub struct CppConstraints {
    /// Effective C++ standard (max of all requirements)
    pub effective_std: Option<CppStandard>,

    /// Minimum required C++ standard (from dependencies)
    pub min_required_std: Option<CppStandard>,

    /// C++ runtime library (non-MSVC platforms)
    pub cpp_runtime: Option<CppRuntime>,

    /// MSVC runtime (graph-wide single value from [build] config)
    pub msvc_runtime_effective: MsvcRuntime,

    /// Whether exceptions are enabled (from [build].exceptions, default true)
    pub effective_exceptions: bool,

    /// Whether RTTI is enabled (from [build].rtti, default true)
    pub effective_rtti: bool,

    /// Whether any target in the graph requires C++
    pub has_cpp: bool,
}

impl Default for CppConstraints {
    fn default() -> Self {
        CppConstraints {
            effective_std: None,
            min_required_std: None,
            cpp_runtime: None,
            msvc_runtime_effective: MsvcRuntime::default(),
            effective_exceptions: true,
            effective_rtti: true,
            has_cpp: false,
        }
    }
}

impl CppConstraints {
    /// Compute C++ constraints for the entire build graph.
    ///
    /// # Algorithm
    ///
    /// 1. Collect min_required_std from all:
    ///    - target.cpp_std across graph
    ///    - surface.compile.requires_cpp across graph
    ///
    /// 2. Determine requested_std:
    ///    - CLI --std if present
    ///    - ELSE workspace [build].cpp_std if present
    ///    - ELSE None
    ///
    /// 3. Compute effective_std:
    ///    - If requested_std exists AND >= min_required_std: use requested_std
    ///    - If requested_std exists AND < min_required_std: ERROR
    ///    - If no requested_std: use min_required_std
    ///
    /// 4. Compute has_cpp = any target has lang=c++ OR cpp_std OR requires_cpp
    ///
    /// 5. Compute effective_exceptions/rtti = build_config.exceptions/rtti
    ///    (In v0, we just use the workspace setting; OR-merge is for future)
    ///
    /// 6. Set msvc_runtime_effective from build_config (or default)
    pub fn compute(
        resolve: &Resolve,
        packages: &HashMap<PackageId, Package>,
        build_config: &BuildConfig,
        cli_std: Option<CppStandard>,
    ) -> Result<Self> {
        let mut min_required_std: Option<CppStandard> = None;
        let mut has_cpp = false;

        // Collect requirements from all packages/targets
        for (pkg_id, _summary) in resolve.packages() {
            let Some(package) = packages.get(pkg_id) else {
                continue;
            };

            for target in package.targets() {
                // Check if target is C++
                if target.requires_cpp() {
                    has_cpp = true;
                }

                // Update min_required from target.cpp_std
                if let Some(std) = target.cpp_std {
                    min_required_std = Some(max_std(min_required_std, std));
                    has_cpp = true;
                }

                // Update min_required from surface.compile.requires_cpp
                if let Some(std) = target.surface.compile.requires_cpp {
                    min_required_std = Some(max_std(min_required_std, std));
                    has_cpp = true;
                }
            }
        }

        // Determine requested_std (CLI takes precedence over build config)
        let requested_std = cli_std.or(build_config.cpp_std);

        // Compute effective_std with validation
        let effective_std = match (requested_std, min_required_std) {
            (Some(requested), Some(required)) if requested < required => {
                // Find which package requires the higher standard for error message
                let mut requiring_pkg = "a dependency";
                for (pkg_id, _summary) in resolve.packages() {
                    if let Some(package) = packages.get(pkg_id) {
                        for target in package.targets() {
                            let target_requires = target.cpp_std.unwrap_or(CppStandard::Cpp11);
                            let surface_requires = target
                                .surface
                                .compile
                                .requires_cpp
                                .unwrap_or(CppStandard::Cpp11);
                            let max_target_req = std::cmp::max(target_requires, surface_requires);
                            if max_target_req == required {
                                requiring_pkg = target.name.as_str();
                                break;
                            }
                        }
                    }
                }

                bail!(
                    "`{}` requires {} but explicit standard is {}\n\
                     hint: pass --std={} or higher, or remove explicit [build].cpp_std",
                    requiring_pkg,
                    required,
                    requested,
                    match required {
                        CppStandard::Cpp11 => "11",
                        CppStandard::Cpp14 => "14",
                        CppStandard::Cpp17 => "17",
                        CppStandard::Cpp20 => "20",
                        CppStandard::Cpp23 => "23",
                    }
                );
            }
            (Some(requested), _) => Some(requested),
            (None, min) => min,
        };

        // If effective_std is set, we have C++
        if effective_std.is_some() {
            has_cpp = true;
        }

        Ok(CppConstraints {
            effective_std,
            min_required_std,
            cpp_runtime: build_config.cpp_runtime,
            msvc_runtime_effective: build_config.msvc_runtime.unwrap_or_default(),
            effective_exceptions: build_config.exceptions,
            effective_rtti: build_config.rtti,
            has_cpp,
        })
    }
}

/// Return the maximum of two C++ standards.
fn max_std(a: Option<CppStandard>, b: CppStandard) -> CppStandard {
    match a {
        Some(existing) => std::cmp::max(existing, b),
        None => b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::target::{Language, Target, TargetKind};
    use crate::core::{Manifest, PackageId, SourceId, Summary};
    use std::path::PathBuf;
    use tempfile::TempDir;

    #[test]
    fn test_max_std() {
        assert_eq!(max_std(None, CppStandard::Cpp17), CppStandard::Cpp17);
        assert_eq!(
            max_std(Some(CppStandard::Cpp14), CppStandard::Cpp20),
            CppStandard::Cpp20
        );
        assert_eq!(
            max_std(Some(CppStandard::Cpp20), CppStandard::Cpp14),
            CppStandard::Cpp20
        );
    }

    #[test]
    fn test_cpp_standard_ordering() {
        assert!(CppStandard::Cpp11 < CppStandard::Cpp14);
        assert!(CppStandard::Cpp14 < CppStandard::Cpp17);
        assert!(CppStandard::Cpp17 < CppStandard::Cpp20);
        assert!(CppStandard::Cpp20 < CppStandard::Cpp23);
    }

    /// Helper to create a test package with a specific C++ standard requirement
    fn create_test_package(
        name: &str,
        version: &str,
        source: SourceId,
        cpp_std: Option<CppStandard>,
    ) -> (PackageId, Package, Summary) {
        let pkg_id = PackageId::new(name, version.parse().unwrap(), source);

        // Create a target with the specified cpp_std
        let mut target = Target::new(name, TargetKind::StaticLib);
        target.lang = Language::Cxx;
        target.cpp_std = cpp_std;

        // Create a minimal manifest
        let manifest = Manifest {
            package: crate::core::manifest::PackageMetadata {
                name: name.to_string(),
                version: version.to_string(),
                description: None,
                license: None,
                authors: vec![],
                repository: None,
                homepage: None,
                documentation: None,
                keywords: vec![],
                categories: vec![],
            },
            dependencies: HashMap::new(),
            targets: vec![target],
            profiles: HashMap::new(),
            build: BuildConfig::default(),
            manifest_dir: PathBuf::new(),
        };

        let package = Package::with_source_id(manifest, PathBuf::new(), source).unwrap();
        let summary = Summary::new(pkg_id, vec![], None);

        (pkg_id, package, summary)
    }

    #[test]
    fn test_std_unification_higher_request() {
        // Test that when a dependency requires C++20 and root requests C++23,
        // effective_std is C++23
        let tmp = TempDir::new().unwrap();
        let source = SourceId::for_path(tmp.path()).unwrap();

        // Create a dependency package requiring C++20
        let (dep_id, dep_pkg, dep_summary) =
            create_test_package("dep", "1.0.0", source, Some(CppStandard::Cpp20));

        // Create root package (no specific requirement)
        let (root_id, root_pkg, root_summary) =
            create_test_package("root", "1.0.0", source, None);

        // Build the resolve
        let mut resolve = Resolve::new();
        resolve.add_package(root_id, root_summary);
        resolve.add_package(dep_id, dep_summary);
        resolve.add_edge(root_id, dep_id);

        // Build the packages map
        let mut packages = HashMap::new();
        packages.insert(root_id, root_pkg);
        packages.insert(dep_id, dep_pkg);

        // Build config with C++23 request
        let build_config = BuildConfig {
            cpp_std: Some(CppStandard::Cpp23),
            ..Default::default()
        };

        // Compute constraints
        let constraints = CppConstraints::compute(&resolve, &packages, &build_config, None)
            .expect("compute should succeed");

        // effective_std should be C++23 (the higher requested standard)
        assert_eq!(constraints.effective_std, Some(CppStandard::Cpp23));
        // min_required_std should be C++20 (from the dependency)
        assert_eq!(constraints.min_required_std, Some(CppStandard::Cpp20));
        assert!(constraints.has_cpp);
    }

    #[test]
    fn test_std_conflict_error() {
        // Test that when root explicitly sets C++17 but a dependency requires C++20,
        // compute() returns an error with a helpful message
        let tmp = TempDir::new().unwrap();
        let source = SourceId::for_path(tmp.path()).unwrap();

        // Create a dependency package requiring C++20
        let (dep_id, dep_pkg, dep_summary) =
            create_test_package("mylib", "1.0.0", source, Some(CppStandard::Cpp20));

        // Create root package (no specific requirement)
        let (root_id, root_pkg, root_summary) =
            create_test_package("root", "1.0.0", source, None);

        // Build the resolve
        let mut resolve = Resolve::new();
        resolve.add_package(root_id, root_summary);
        resolve.add_package(dep_id, dep_summary);
        resolve.add_edge(root_id, dep_id);

        // Build the packages map
        let mut packages = HashMap::new();
        packages.insert(root_id, root_pkg);
        packages.insert(dep_id, dep_pkg);

        // Build config with C++17 request (lower than required)
        let build_config = BuildConfig {
            cpp_std: Some(CppStandard::Cpp17),
            ..Default::default()
        };

        // Compute constraints - should fail
        let result = CppConstraints::compute(&resolve, &packages, &build_config, None);

        assert!(result.is_err(), "Expected an error due to standard conflict");

        let error_msg = result.unwrap_err().to_string();
        // Error should mention the requiring package
        assert!(
            error_msg.contains("mylib"),
            "Error should mention the requiring package name. Got: {}",
            error_msg
        );
        // Error should mention C++20 (the required standard)
        assert!(
            error_msg.contains("C++20"),
            "Error should mention the required standard. Got: {}",
            error_msg
        );
        // Error should mention C++17 (the explicitly set standard)
        assert!(
            error_msg.contains("C++17"),
            "Error should mention the explicit standard. Got: {}",
            error_msg
        );
        // Error should provide a hint
        assert!(
            error_msg.contains("hint"),
            "Error should provide a hint. Got: {}",
            error_msg
        );
    }
}
