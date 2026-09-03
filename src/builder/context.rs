//! Build context - compiler, target, and profile configuration.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;

use crate::builder::toolchain::{
    detect_toolchain, resolve_target, CxxOptions, Toolchain, ToolchainPlatform,
};
use crate::core::abi::CompilerIdentity;
use crate::core::manifest::Profile;
use crate::core::surface::TargetPlatform;
use crate::core::target::TargetTriple;
use crate::core::Workspace;
use crate::resolver::CppConstraints;
use crate::util::config::VcpkgConfig;
use crate::util::process::ProcessBuilder;
use crate::util::VcpkgIntegration;

/// Build context containing compiler and target information.
#[derive(Clone)]
pub struct BuildContext {
    /// Toolchain implementation
    pub toolchain: Arc<dyn Toolchain>,

    /// Target triple
    pub target: TargetTriple,

    /// Compiler identity
    pub compiler: CompilerIdentity,

    /// Target platform for surface condition evaluation
    pub platform: TargetPlatform,

    /// Build profile
    pub profile: Profile,

    /// Profile name
    pub profile_name: String,

    /// Output directory
    pub output_dir: PathBuf,

    /// Dependencies output directory
    pub deps_dir: PathBuf,

    /// Workspace root
    pub workspace_root: PathBuf,

    /// C++ constraints for the build graph
    pub cpp_constraints: Option<CppConstraints>,

    /// Vcpkg integration, if configured
    pub vcpkg: Option<VcpkgIntegration>,

    /// Compiler flags required by the target itself, from its [`TargetSpec`].
    ///
    /// Empty for host builds. For a cross target these are not optional
    /// niceties: invoking `arm-none-eabi-gcc` without `-mcpu`/`-mthumb`/
    /// `-mfloat-abi` compiles for a default core, and a `-march`/`-mabi`
    /// mismatch on RISC-V compiles cleanly and fails at link.
    ///
    /// [`TargetSpec`]: crate::builder::toolchain::TargetSpec
    pub target_cflags: Vec<String>,

    /// Link flags required by the target itself.
    ///
    /// Empty for host builds. Some targets need a flag on both the compile
    /// and the link step -- Apple's `-arch` is one -- and applying it to only
    /// one produces an artifact compiled for one architecture and linked for
    /// another, which fails confusingly at best and silently at worst.
    pub target_ldflags: Vec<String>,
}

impl fmt::Debug for BuildContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BuildContext")
            .field("toolchain", &self.toolchain.platform())
            .field("target", &self.target)
            .field("compiler", &self.compiler)
            .field("platform", &self.platform)
            .field("profile", &self.profile)
            .field("profile_name", &self.profile_name)
            .field("output_dir", &self.output_dir)
            .field("deps_dir", &self.deps_dir)
            .field("workspace_root", &self.workspace_root)
            .field("cpp_constraints", &self.cpp_constraints)
            .field("vcpkg", &self.vcpkg)
            .finish()
    }
}

impl BuildContext {
    /// Create a new build context from a workspace.
    ///
    /// `target` is the triple to build for; `None` means the host.
    pub fn new(ws: &Workspace, profile_name: &str, target: Option<&TargetTriple>) -> Result<Self> {
        // Resolve the effective target once, so toolchain selection, the ABI
        // identity and the output directory cannot disagree about it.
        let target = resolve_target(target);

        let toolchain: Arc<dyn Toolchain> = Arc::from(detect_toolchain(Some(&target))?);

        let compiler = detect_compiler_identity(toolchain.as_ref())?;

        // Surface conditions -- which defines, include dirs and flags apply --
        // are evaluated against the build target, not the host. Reading the
        // host here meant a cross build applied the host's surface.
        let platform = TargetPlatform::for_target(&target).with_compiler(&compiler.family);

        // Get profile
        let profile = if profile_name == "release" {
            ws.manifest().release_profile()
        } else {
            ws.manifest().debug_profile()
        };

        // Flags the target requires. Deliberately empty for host builds, so
        // this cannot change existing host behaviour.
        let (target_cflags, target_ldflags) = if target.is_host() {
            (Vec::new(), Vec::new())
        } else {
            let spec = crate::builder::toolchain::TargetSpec::for_triple(&target);
            if spec.flags_uncertain && !spec.uncertainty_note.is_empty() {
                tracing::warn!(
                    "flags for {} are not fully determined: {}",
                    target.as_str(),
                    spec.uncertainty_note
                );
            }
            (spec.cflags(), spec.ldflags())
        };

        // Cross builds get their own output tree. Without this, host and
        // target artifacts share a path and silently contaminate each other --
        // and unlike a wrong cache key, path separation needs no fingerprint
        // machinery to be correct. Keyed on `canonical()` so two spellings of
        // one target resolve to one directory.
        let (output_dir, deps_dir) = if target.is_host() {
            (ws.output_dir(), ws.deps_dir())
        } else {
            let base = ws.target_dir().join(target.canonical()).join(ws.profile());
            let deps = base.join("deps");
            (base, deps)
        };

        Ok(BuildContext {
            toolchain,
            target,
            compiler,
            platform,
            profile,
            profile_name: profile_name.to_string(),
            output_dir,
            deps_dir,
            workspace_root: ws.root().to_path_buf(),
            cpp_constraints: None,
            vcpkg: None,
            target_cflags,
            target_ldflags,
        })
    }

    /// Create a new build context with vcpkg integration.
    pub fn new_with_vcpkg(
        ws: &Workspace,
        profile_name: &str,
        vcpkg: &VcpkgConfig,
        target: Option<&TargetTriple>,
    ) -> Result<Self> {
        let mut ctx = Self::new(ws, profile_name, target)?;
        ctx.vcpkg = VcpkgIntegration::from_config(vcpkg, &ctx.target, ctx.is_release());
        Ok(ctx)
    }

    /// Set C++ constraints for this build context.
    pub fn with_cpp_constraints(mut self, constraints: CppConstraints) -> Self {
        self.cpp_constraints = Some(constraints);
        self
    }

    /// Get vcpkg integration details, if configured.
    pub fn vcpkg(&self) -> Option<&VcpkgIntegration> {
        self.vcpkg.as_ref()
    }

    /// Get C++ options from constraints for compilation/linking.
    ///
    /// Returns None if no C++ is involved in this build.
    pub fn cxx_options(&self) -> Option<CxxOptions> {
        let constraints = self.cpp_constraints.as_ref()?;

        if !constraints.has_cpp {
            return None;
        }

        Some(CxxOptions {
            std: constraints.effective_std,
            exceptions: constraints.effective_exceptions,
            rtti: constraints.effective_rtti,
            runtime: constraints.cpp_runtime,
            msvc_runtime: constraints.msvc_runtime_effective,
            is_debug: !self.is_release(),
        })
    }

    /// Get compiler flags from profile.
    pub fn profile_cflags(&self) -> Vec<String> {
        // Target flags come first so a profile or manifest flag can override
        // them, and because both the plan and the native builder start from
        // this method -- putting them here means every compile command gets
        // them without each call site having to remember.
        let mut flags = self.target_cflags.clone();

        // Optimization level
        if let Some(ref opt) = self.profile.opt_level {
            flags.push(format!("-O{}", opt));
        }

        // Debug info
        if let Some(ref debug) = self.profile.debug {
            if debug != "0" {
                flags.push("-g".to_string());
                if debug == "full" || debug == "2" {
                    flags.push("-g3".to_string());
                }
            }
        }

        // Sanitizers
        for sanitizer in &self.profile.sanitizers {
            flags.push(format!("-fsanitize={}", sanitizer));
        }

        // Custom flags
        flags.extend(self.profile.cflags.iter().cloned());

        flags
    }

    /// Get linker flags from profile.
    pub fn profile_ldflags(&self) -> Vec<String> {
        // Target link flags first, for the same reason as profile_cflags:
        // native.rs starts from this method, so putting them here means every
        // link command gets them without each call site remembering.
        let mut flags = self.target_ldflags.clone();

        // LTO
        if self.profile.lto == Some(true) {
            flags.push("-flto".to_string());
        }

        // Sanitizers (need to be passed to linker too)
        for sanitizer in &self.profile.sanitizers {
            flags.push(format!("-fsanitize={}", sanitizer));
        }

        // Custom flags
        flags.extend(self.profile.ldflags.iter().cloned());

        flags
    }

    /// Check if this is a release build.
    pub fn is_release(&self) -> bool {
        self.profile_name == "release"
    }

    /// Get the OS name.
    ///
    /// `self.target` is always `TargetTriple::host()` today (see `new`), and
    /// the host always has a real OS, so `unwrap_or("")` never loses
    /// information in practice. If `BuildContext` ever carries a bare-metal
    /// cross target, this should become `Option<&str>` at the call sites.
    pub fn os(&self) -> &str {
        self.target.os().unwrap_or("")
    }

    /// Get the active toolchain.
    pub fn toolchain(&self) -> &dyn Toolchain {
        self.toolchain.as_ref()
    }
}

/// Detect the compiler identity from the compiler path.
fn detect_compiler_identity(toolchain: &dyn Toolchain) -> Result<CompilerIdentity> {
    let compiler_path = toolchain.compiler_path();
    let family = compiler_family(toolchain.platform());

    let version =
        get_compiler_version(compiler_path, family).unwrap_or_else(|| "unknown".to_string());

    Ok(CompilerIdentity::new(family, &version))
}

fn compiler_family(platform: ToolchainPlatform) -> &'static str {
    match platform {
        ToolchainPlatform::Gcc => "gcc",
        ToolchainPlatform::Clang => "clang",
        ToolchainPlatform::AppleClang => "apple-clang",
        ToolchainPlatform::Msvc => "msvc",
    }
}

/// Get the compiler version.
fn get_compiler_version(cc: &Path, family: &str) -> Option<String> {
    let output = if family == "msvc" {
        ProcessBuilder::new(cc).exec().ok()?
    } else {
        ProcessBuilder::new(cc).arg("--version").exec().ok()?
    };

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Parse version from output
    // This is a simplified version parser
    for line in stdout.lines() {
        // Look for version numbers like "14.0.0" or "13.2.1"
        for word in line.split_whitespace() {
            if word.chars().next()?.is_ascii_digit() {
                let parts: Vec<&str> = word.split('.').collect();
                if parts.len() >= 2 {
                    return Some(format!("{}.{}", parts[0], parts[1]));
                }
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::toolchain::GccToolchain;

    #[test]
    fn test_profile_cflags() {
        let profile = Profile {
            opt_level: Some("2".to_string()),
            debug: Some("1".to_string()),
            sanitizers: vec!["address".to_string()],
            ..Default::default()
        };

        let toolchain = Arc::new(GccToolchain::new(
            PathBuf::from("gcc"),
            PathBuf::from("g++"),
            PathBuf::from("ar"),
            ToolchainPlatform::Gcc,
        ));

        let ctx = BuildContext {
            toolchain,
            target: TargetTriple::host(),
            compiler: CompilerIdentity::new("gcc", "13.0"),
            platform: TargetPlatform::host(),
            profile,
            profile_name: "debug".to_string(),
            output_dir: PathBuf::from("target"),
            deps_dir: PathBuf::from("target/deps"),
            workspace_root: PathBuf::from("."),
            cpp_constraints: None,
            vcpkg: None,
            target_cflags: Vec::new(),
            target_ldflags: Vec::new(),
        };

        let flags = ctx.profile_cflags();
        assert!(flags.contains(&"-O2".to_string()));
        assert!(flags.contains(&"-g".to_string()));
        assert!(flags.contains(&"-fsanitize=address".to_string()));
        // Target flags are absent for a host build, by construction.
        assert!(!flags.iter().any(|f| f.starts_with("-mcpu")));
    }
}
