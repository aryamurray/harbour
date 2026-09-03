//! Implementation of `harbour build`.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{bail, Result};

use crate::builder::shim::{
    BackendAvailability, BackendId, BackendRegistry, LinkagePreference, TargetTriple,
};
use crate::builder::{BuildContext, BuildPlan, NativeBuilder};
use crate::core::target::CppStandard;
use crate::core::workspace::WorkspaceMember;
use crate::core::{Package, Workspace};
use crate::ops::resolve::{resolve_workspace_with_opts, ResolveOptions};
use crate::resolver::{CppConstraints, Resolve};
use crate::sources::SourceCache;
use crate::util::config::VcpkgConfig;

/// Validate that all requested targets exist in the selected packages.
///
/// This prevents silent no-ops when the user specifies a nonexistent target.
fn validate_target_filter(packages: &[&Package], targets: &[String]) -> Result<()> {
    // Collect all valid targets from selected packages
    let valid_targets: Vec<_> = packages
        .iter()
        .flat_map(|p| p.targets().iter().map(|t| t.name.to_string()))
        .collect();

    for requested in targets {
        if !valid_targets.iter().any(|t| t == requested) {
            bail!(
                "unknown target `{}`\n\
                 available targets: {}\n\
                 hint: use `harbour tree` to see all targets",
                requested,
                if valid_targets.is_empty() {
                    "(none)".to_string()
                } else {
                    valid_targets.join(", ")
                }
            );
        }
    }

    Ok(())
}

/// Options for the build command.
#[derive(Debug, Clone, Default)]
pub struct BuildOptions {
    /// Build in release mode
    pub release: bool,

    /// Specific packages to build (empty = default members)
    pub packages: Vec<String>,

    /// Specific targets to build (empty = all)
    pub targets: Vec<String>,

    /// Emit compile_commands.json
    pub emit_compile_commands: bool,

    /// Emit build plan as JSON
    pub emit_plan: bool,

    /// Number of parallel jobs
    pub jobs: Option<usize>,

    /// Verbose output
    pub verbose: bool,

    /// Explicit C++ standard from CLI (--std flag)
    pub cpp_std: Option<CppStandard>,

    /// Explicit backend selection (native, cmake, meson, custom)
    pub backend: Option<BackendId>,

    /// Library linkage preference
    pub linkage: LinkagePreference,

    /// Build for FFI consumption
    pub ffi: bool,

    /// Target triple for cross-compilation
    pub target_triple: Option<TargetTriple>,

    /// Require lockfile to be up-to-date (error if resolution would change it)
    pub locked: bool,

    /// Vcpkg integration settings
    pub vcpkg: VcpkgConfig,
}

/// Select workspace members based on the filter.
///
/// If no packages are specified, returns the default members.
/// Errors if a specified package is not found in the workspace.
pub fn select_members<'a>(
    ws: &'a Workspace,
    filter: &[String],
) -> Result<Vec<&'a WorkspaceMember>> {
    if filter.is_empty() {
        return Ok(ws.default_members());
    }

    let mut members = Vec::new();
    let member_names: Vec<&str> = ws.member_names();

    for name in filter {
        if let Some(member) = ws.member(name) {
            members.push(member);
        } else {
            bail!(
                "package `{}` not found in workspace\n\
                 available packages: {}",
                name,
                if member_names.is_empty() {
                    "(none)".to_string()
                } else {
                    member_names.join(", ")
                }
            );
        }
    }

    Ok(members)
}

/// Select packages to build based on the filter.
///
/// If no packages are specified, returns the default members' packages.
/// Errors if a specified package is not found in the workspace.
pub fn select_packages<'a>(ws: &'a Workspace, filter: &[String]) -> Result<Vec<&'a Package>> {
    Ok(select_members(ws, filter)?
        .into_iter()
        .map(|m| &m.package)
        .collect())
}

/// Build result.
#[derive(Debug)]
pub struct BuildResult {
    /// Built artifacts
    pub artifacts: Vec<Artifact>,

    /// Build plan (if requested)
    pub plan: Option<BuildPlan>,

    /// Number of source files actually compiled (0 if `plan` was requested
    /// and the build returned early)
    pub compiled: usize,

    /// Number of source files skipped because they were already up to date
    pub skipped: usize,
}

/// A built artifact.
#[derive(Debug)]
pub struct Artifact {
    /// Artifact path
    pub path: PathBuf,

    /// Target name
    pub target: String,
}

/// Build the workspace.
pub fn build(
    ws: &Workspace,
    source_cache: &mut SourceCache,
    opts: &BuildOptions,
) -> Result<BuildResult> {
    // Create backend registry and get the requested backend
    let registry = BackendRegistry::new();
    let backend_id = opts.backend.unwrap_or(BackendId::Native);
    let backend = registry
        .get(backend_id)
        .ok_or_else(|| anyhow::anyhow!("unknown backend: {:?}", backend_id))?;

    // Check backend availability
    match backend.availability()? {
        BackendAvailability::Available { version } => {
            tracing::debug!("Using {} backend v{}", backend_id, version);
        }
        BackendAvailability::AlwaysAvailable => {
            tracing::debug!("Using {} backend (built-in)", backend_id);
        }
        BackendAvailability::NotInstalled { tool, install_hint } => {
            bail!(
                "{} backend requires {} which is not installed.\n\
                 hint: {}",
                backend_id,
                tool,
                install_hint
            );
        }
        BackendAvailability::VersionTooOld { found, required } => {
            bail!(
                "{} backend requires version {}, but found {}.\n\
                 hint: upgrade {} to meet version requirements",
                backend_id,
                required,
                found,
                backend_id
            );
        }
    }

    // The capability checks below read `opts` directly. A BuildIntent used to
    // be constructed here as well, but nothing consumed it -- BackendValidator,
    // its only production reader, is never called from this path -- so it was
    // scaffolding that made the requested target look like it flowed somewhere
    // when it was discarded a few lines later.
    let caps = backend.capabilities();

    // Check linkage support
    match &opts.linkage {
        LinkagePreference::Static if !caps.linkage.static_linking => {
            bail!(
                "backend `{}` does not support static linking.\n\
                 hint: use --linkage=shared or choose a different backend",
                backend_id
            );
        }
        LinkagePreference::Shared if !caps.linkage.shared_linking => {
            bail!(
                "backend `{}` does not support shared linking.\n\
                 hint: use --linkage=static or choose a different backend",
                backend_id
            );
        }
        _ => {}
    }

    // Check FFI requirements
    if opts.ffi {
        if !caps.linkage.shared_linking {
            bail!(
                "FFI mode requires shared library support, but backend `{}` only supports static linking.\n\
                 hint: use a different backend that supports shared libraries",
                backend_id
            );
        }
        if !caps.linkage.runtime_bundle {
            tracing::warn!(
                "Backend `{}` does not support runtime bundling. \
                 FFI bundle may require manual dependency collection.",
                backend_id
            );
        }
    }

    // Check cross-compilation support
    if opts.target_triple.is_some() && !caps.platform.cross_compile {
        bail!(
            "backend `{}` does not support cross-compilation.\n\
                 hint: use cmake or meson backend for cross-compilation",
            backend_id
        );
    }

    // Log validation success
    if opts.ffi {
        tracing::info!("FFI mode enabled (shared libraries + runtime bundling)");
    }
    if let Some(ref triple) = opts.target_triple {
        tracing::info!("Cross-compiling for {}", triple);
    }

    // Select packages to build
    let selected_packages = select_packages(ws, &opts.packages)?;

    if selected_packages.is_empty() {
        bail!("no packages to build");
    }

    // Log selected packages
    if selected_packages.len() > 1 || !opts.packages.is_empty() {
        let names: Vec<_> = selected_packages
            .iter()
            .map(|p| p.name().as_str())
            .collect();
        tracing::info!("Building packages: {}", names.join(", "));
    }

    // Validate target filter if specified
    let target_filter = if !opts.targets.is_empty() {
        validate_target_filter(&selected_packages, &opts.targets)?;
        Some(opts.targets.as_slice())
    } else {
        None
    };

    // Resolve dependencies (uses lockfile if available)
    let resolve_opts = ResolveOptions {
        locked: opts.locked,
    };
    let resolve = resolve_workspace_with_opts(ws, source_cache, &resolve_opts)?;

    // Ensure output directory exists
    ws.ensure_output_dir()?;

    // Create build context
    let profile = if opts.release { "release" } else { "debug" };
    let mut build_ctx =
        BuildContext::new_with_vcpkg(ws, profile, &opts.vcpkg, opts.target_triple.as_ref())?;

    if let Some(vcpkg) = build_ctx.vcpkg() {
        tracing::info!(
            "Using vcpkg {} (triplet {})",
            vcpkg.root.display(),
            vcpkg.triplet
        );
    }

    // Compute C++ constraints from the resolved packages
    let packages = collect_packages(&resolve, source_cache)?;
    let cpp_constraints =
        CppConstraints::compute(&resolve, &packages, &ws.manifest().build, opts.cpp_std)?;

    // Log C++ constraints if any C++ is involved
    if cpp_constraints.has_cpp {
        if let Some(std) = cpp_constraints.effective_std {
            tracing::info!("C++ standard: {}", std);
        }
        if !cpp_constraints.effective_exceptions {
            tracing::info!("C++ exceptions: disabled");
        }
        if !cpp_constraints.effective_rtti {
            tracing::info!("C++ RTTI: disabled");
        }
    }

    // Set C++ constraints on build context
    build_ctx = build_ctx.with_cpp_constraints(cpp_constraints.clone());

    // Create build plan with target filter
    let plan = BuildPlan::new(&build_ctx, &resolve, source_cache, target_filter)?;

    // If only emitting plan, return early
    if opts.emit_plan {
        let plan_json = serde_json::to_string_pretty(&plan)?;
        println!("{}", plan_json);

        return Ok(BuildResult {
            artifacts: vec![],
            plan: Some(plan),
            compiled: 0,
            skipped: 0,
        });
    }

    // Emit compile_commands.json if requested (enabled by default for IDE support)
    if opts.emit_compile_commands {
        // Put in .harbour/ directory
        let harbour_dir = ws.root().join(".harbour");
        std::fs::create_dir_all(&harbour_dir).ok();
        let cc_path = harbour_dir.join("compile_commands.json");
        plan.emit_compile_commands(&build_ctx, &cc_path)?;

        // Also create/update symlink in project root for IDE discovery
        // (clangd, VSCode C/C++, etc. look for compile_commands.json in project root)
        let root_cc_path = ws.root().join("compile_commands.json");
        create_compile_commands_link(&cc_path, &root_cc_path);

        tracing::info!("Wrote {}", cc_path.display());
    }

    // Execute build with C++ options if needed
    let cxx_opts = build_ctx.cxx_options();
    let builder = if let Some(opts) = cxx_opts {
        NativeBuilder::with_cxx_options(&build_ctx, opts)
    } else {
        NativeBuilder::new(&build_ctx)
    };
    let outcome = builder.execute(&plan, opts.jobs)?;

    Ok(BuildResult {
        artifacts: outcome.artifacts,
        plan: None,
        compiled: outcome.compiled,
        skipped: outcome.skipped,
    })
}

/// Create a symlink or copy of compile_commands.json in the project root.
///
/// This helps IDEs like clangd and VSCode C/C++ extension find the file,
/// as they typically look for it in the project root.
fn create_compile_commands_link(source: &std::path::Path, dest: &std::path::Path) {
    // Remove existing file/link if present
    if dest.exists() || dest.is_symlink() {
        let _ = std::fs::remove_file(dest);
    }

    // Try symlink first (works on Unix, requires admin/dev mode on Windows)
    #[cfg(unix)]
    {
        if std::os::unix::fs::symlink(source, dest).is_ok() {
            return;
        }
    }

    #[cfg(windows)]
    {
        // On Windows, try symbolic link (requires elevated privileges or dev mode)
        if std::os::windows::fs::symlink_file(source, dest).is_ok() {
            return;
        }
    }

    // Fall back to copying the file
    let _ = std::fs::copy(source, dest);
}

/// Collect packages from the resolve for C++ constraint computation.
fn collect_packages(
    resolve: &Resolve,
    source_cache: &mut SourceCache,
) -> Result<HashMap<crate::core::PackageId, crate::core::Package>> {
    let mut packages = HashMap::new();

    for pkg_id in resolve.topological_order() {
        if let Ok(package) = source_cache.load_package(pkg_id) {
            packages.insert(pkg_id, package);
        }
    }

    Ok(packages)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::GlobalContext;
    use tempfile::TempDir;

    fn create_test_project(dir: &std::path::Path) {
        std::fs::write(
            dir.join("Harbour.toml"),
            r#"
[package]
name = "test"
version = "1.0.0"

[targets.test]
kind = "exe"
sources = ["src/**/*.c"]

[targets.test.surface.compile.private]
cflags = ["-Wall"]
"#,
        )
        .unwrap();

        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("src/main.c"),
            r#"
#include <stdio.h>

int main(void) {
    printf("Hello from Harbour!\n");
    return 0;
}
"#,
        )
        .unwrap();
    }

    #[test]
    #[ignore] // Requires C compiler
    fn test_build() {
        let tmp = TempDir::new().unwrap();
        create_test_project(tmp.path());

        let ctx = GlobalContext::with_cwd(tmp.path().to_path_buf()).unwrap();
        let ws = Workspace::new(&tmp.path().join("Harbour.toml"), &ctx).unwrap();

        let mut cache = SourceCache::new(tmp.path().join("cache"));
        let opts = BuildOptions::default();

        let result = build(&ws, &mut cache, &opts).unwrap();
        assert!(!result.artifacts.is_empty());
    }

    #[test]
    #[ignore] // Requires C compiler
    fn test_incremental_build_skips_unchanged_and_recompiles_on_touch() {
        let tmp = TempDir::new().unwrap();
        create_test_project(tmp.path());

        let ctx = GlobalContext::with_cwd(tmp.path().to_path_buf()).unwrap();
        let ws = Workspace::new(&tmp.path().join("Harbour.toml"), &ctx).unwrap();
        let mut cache = SourceCache::new(tmp.path().join("cache"));
        let opts = BuildOptions::default();

        // First build: nothing is cached yet, so everything must compile.
        let result1 = build(&ws, &mut cache, &opts).unwrap();
        assert!(
            result1.compiled > 0,
            "first build should compile at least one file"
        );
        assert_eq!(result1.skipped, 0, "first build has nothing to skip");

        // Second build, source unchanged: nothing should be recompiled.
        let result2 = build(&ws, &mut cache, &opts).unwrap();
        assert_eq!(
            result2.compiled, 0,
            "unchanged source must not be recompiled"
        );
        assert_eq!(
            result2.skipped, result1.compiled,
            "every previously-compiled file should now be reported as skipped"
        );

        // Touch (content-change) the source file and rebuild: it must recompile.
        std::fs::write(
            tmp.path().join("src/main.c"),
            r#"
#include <stdio.h>

int main(void) {
    printf("Hello from Harbour, updated!\n");
    return 0;
}
"#,
        )
        .unwrap();

        let result3 = build(&ws, &mut cache, &opts).unwrap();
        assert!(result3.compiled > 0, "touched source must be recompiled");
    }

    #[test]
    #[ignore] // Requires C compiler
    fn test_incremental_build_profile_switch_does_not_reuse_artifacts() {
        let tmp = TempDir::new().unwrap();
        create_test_project(tmp.path());

        let ctx = GlobalContext::with_cwd(tmp.path().to_path_buf()).unwrap();
        let mut cache = SourceCache::new(tmp.path().join("cache"));

        // `BuildOptions::release` only picks which profile the compile flags
        // come from; the workspace's own notion of "current profile" (which
        // drives `output_dir`, and therefore where the fingerprint cache
        // lives) is set separately via `Workspace::with_profile`, exactly as
        // the `harbour build` CLI command does.
        let debug_opts = BuildOptions::default();
        let release_opts = BuildOptions {
            release: true,
            ..Default::default()
        };

        let manifest_path = tmp.path().join("Harbour.toml");
        let debug_ws = || {
            Workspace::new(&manifest_path, &ctx)
                .unwrap()
                .with_profile("debug")
        };
        let release_ws = Workspace::new(&manifest_path, &ctx)
            .unwrap()
            .with_profile("release");

        let debug_result = build(&debug_ws(), &mut cache, &debug_opts).unwrap();
        assert!(debug_result.compiled > 0);

        // Switching to release must not reuse the debug build's fingerprints
        // or artifacts, even though the source is unchanged.
        let release_result = build(&release_ws, &mut cache, &release_opts).unwrap();
        assert!(
            release_result.compiled > 0,
            "release build must not reuse debug artifacts"
        );

        // Switching back to debug must still find the earlier debug
        // fingerprints intact (separate cache per profile).
        let debug_again = build(&debug_ws(), &mut cache, &debug_opts).unwrap();
        assert_eq!(
            debug_again.compiled, 0,
            "debug artifacts from the first debug build are still valid"
        );
    }

    #[test]
    fn test_validate_target_filter_valid() {
        let tmp = TempDir::new().unwrap();
        create_test_project(tmp.path());

        let ctx = GlobalContext::with_cwd(tmp.path().to_path_buf()).unwrap();
        let ws = Workspace::new(&tmp.path().join("Harbour.toml"), &ctx).unwrap();

        let packages = select_packages(&ws, &[]).unwrap();

        // "test" is a valid target in create_test_project
        let result = validate_target_filter(&packages, &["test".to_string()]);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_target_filter_invalid() {
        let tmp = TempDir::new().unwrap();
        create_test_project(tmp.path());

        let ctx = GlobalContext::with_cwd(tmp.path().to_path_buf()).unwrap();
        let ws = Workspace::new(&tmp.path().join("Harbour.toml"), &ctx).unwrap();

        let packages = select_packages(&ws, &[]).unwrap();

        // "nonexistent" is not a valid target
        let result = validate_target_filter(&packages, &["nonexistent".to_string()]);
        assert!(result.is_err());

        let err = result.unwrap_err().to_string();
        assert!(err.contains("unknown target"));
        assert!(err.contains("nonexistent"));
        assert!(err.contains("available targets"));
    }

    #[test]
    fn test_select_packages_not_found() {
        let tmp = TempDir::new().unwrap();
        create_test_project(tmp.path());

        let ctx = GlobalContext::with_cwd(tmp.path().to_path_buf()).unwrap();
        let ws = Workspace::new(&tmp.path().join("Harbour.toml"), &ctx).unwrap();

        // Try to select a non-existent package
        let result = select_packages(&ws, &["nonexistent".to_string()]);
        assert!(result.is_err());

        let err = result.unwrap_err().to_string();
        assert!(err.contains("not found in workspace"));
        assert!(err.contains("available packages"));
    }
}
