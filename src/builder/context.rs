//! Build context - compiler, target, and profile configuration.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;

use crate::builder::toolchain::{
    detect_toolchain, resolve_target, CommandSpec, CompileInput, CxxOptions, ProfileOptions,
    Toolchain, ToolchainPlatform,
};
use crate::builder::util::parse_define_flags;
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

    /// Fold vcpkg's include and library directories into a resolved
    /// surface, if vcpkg is configured. A no-op otherwise.
    ///
    /// One implementation, called by both the build plan and
    /// `harbour flags`, for the same reason there is now only one surface
    /// fold: a second copy of "and then vcpkg's directories go here" is a
    /// second answer to "what does the compiler receive".
    ///
    /// Appended, never sorted. `-I` and `-L` are first-match-wins, so a
    /// system-wide fallback belongs at the end of the search path; sorting
    /// would interleave vcpkg's copy of a header with a package's own,
    /// decided by nothing more than how the two paths collate.
    pub fn merge_vcpkg_dirs(
        &self,
        compile: &mut crate::builder::surface_resolver::EffectiveCompileSurface,
        link: &mut crate::builder::surface_resolver::EffectiveLinkSurface,
    ) {
        let Some(vcpkg) = self.vcpkg.as_ref() else {
            return;
        };
        compile
            .include_dirs
            .extend(vcpkg.include_dirs.iter().cloned());
        link.lib_dirs.extend(vcpkg.lib_dirs.iter().cloned());
        compile.dedup_for_build();
        link.dedup_for_build();
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

    /// Build the compile command for a planned compile step.
    ///
    /// **The** place a compile command is constructed. There used to be two:
    /// `NativeBuilder::compile` for the real build, and
    /// `BuildPlan::emit_compile_commands` for `compile_commands.json`. They
    /// drifted -- the compile database passed `cxx_opts: None`, and since
    /// `-std=`, `-fno-exceptions`, `-fno-rtti` and `-stdlib=` are all emitted
    /// inside a `if let Some(opts) = cxx_opts` in the toolchain backends,
    /// clangd read every C++ file as exceptions-enabled C++ at the default
    /// standard while the compiler was given `-std=c++17 -fno-exceptions
    /// -fno-rtti`. Wrong diagnostics, wrong completions, phantom errors.
    ///
    /// Both callers now go through here, and neither is passed a `CxxOptions`
    /// it could get wrong: the value comes from [`Self::cxx_options`], which
    /// derives it from the resolved C++ constraints. The two consumers can no
    /// longer disagree because there is nothing left for them to disagree
    /// about.
    pub fn compile_spec(&self, step: &crate::builder::plan::CompileStep) -> Result<CommandSpec> {
        let mut cflags = self.profile_cflags()?;
        cflags.extend(step.cflags.iter().cloned());

        let input = CompileInput {
            source: step.source.clone(),
            output: step.output.clone(),
            include_dirs: step.include_dirs.clone(),
            defines: parse_define_flags(&step.defines),
            cflags,
        };

        Ok(self
            .toolchain()
            .compile_command(&input, step.lang, self.cxx_options().as_ref()))
    }

    /// The profile's contribution to every compile command.
    ///
    /// Three layers, in this order: the target's flags, then the profile's
    /// *intent* as spelled by the active toolchain, then the profile's
    /// verbatim `cflags`.
    ///
    /// The middle layer is the point. This method used to write the flags
    /// itself -- `-O{level}`, `-g`, `-g3`, `-fsanitize={name}` -- with no
    /// toolchain branch at all, and `cl.exe` answered every one of them with
    /// `D9002: ignoring unknown option` and carried on. The build went green,
    /// every Windows release was built unoptimised, no Windows build had ever
    /// carried debug information, and `sanitizers` did nothing there. Now the
    /// intent is parsed once into [`ProfileOptions`] and each backend spells
    /// it: [`GccToolchain`] and [`MsvcToolchain`] are the only two places a
    /// `-O` or a `/O` is written.
    ///
    /// Fallible because some intent has no spelling on some toolchains --
    /// `opt_level = "g"` and the thread/memory/undefined/leak sanitizers on
    /// MSVC. Refusing to build is the only honest answer there; emitting
    /// nothing is the bug being fixed.
    ///
    /// [`GccToolchain`]: crate::builder::toolchain::GccToolchain
    /// [`MsvcToolchain`]: crate::builder::toolchain::MsvcToolchain
    pub fn profile_cflags(&self) -> Result<Vec<String>> {
        // Target flags come first so a profile or manifest flag can override
        // them, and because both the plan and the native builder start from
        // this method -- putting them here means every compile command gets
        // them without each call site having to remember.
        let mut flags = self.target_cflags.clone();

        flags.extend(
            self.toolchain()
                .profile_compile_flags(&self.profile_options()?)?,
        );

        // Custom flags, verbatim. Whoever wrote these named a compiler.
        flags.extend(self.profile.cflags.iter().cloned());

        Ok(flags)
    }

    /// The profile's contribution to every link command.
    ///
    /// Same three layers as [`Self::profile_cflags`], and the same reason for
    /// asking the toolchain: LTO needs a flag on both command lines with two
    /// different spellings on MSVC, sanitizers need one at link time on
    /// GCC/clang and none on MSVC, and MSVC debug information needs `/DEBUG`
    /// here or `/Z7`'s records never become a PDB.
    pub fn profile_ldflags(&self) -> Result<Vec<String>> {
        // Target link flags first, for the same reason as profile_cflags:
        // native.rs starts from this method, so putting them here means every
        // link command gets them without each call site remembering.
        let mut flags = self.target_ldflags.clone();

        flags.extend(
            self.toolchain()
                .profile_link_flags(&self.profile_options()?)?,
        );

        // Custom flags
        flags.extend(self.profile.ldflags.iter().cloned());

        Ok(flags)
    }

    /// The active profile's settings, parsed into intent.
    ///
    /// Derived on demand from `self.profile` rather than stored alongside it:
    /// a cached copy is a second answer to "what did the profile ask for",
    /// and a `BuildContext` whose `profile` and `profile_options` disagree is
    /// exactly the failure mode this change is undoing.
    pub fn profile_options(&self) -> Result<ProfileOptions> {
        ProfileOptions::from_profile(&self.profile)
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
    use crate::builder::toolchain::{GccToolchain, MsvcToolchain};

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

        let flags = ctx.profile_cflags().unwrap();
        assert!(flags.contains(&"-O2".to_string()));
        assert!(flags.contains(&"-g".to_string()));
        assert!(flags.contains(&"-fsanitize=address".to_string()));
        // Target flags are absent for a host build, by construction.
        assert!(!flags.iter().any(|f| f.starts_with("-mcpu")));
    }

    /// Build a context for `platform` with only `lto` set on the profile.
    fn lto_ctx(platform: ToolchainPlatform) -> BuildContext {
        let profile = Profile {
            lto: Some(true),
            ..Default::default()
        };

        let toolchain: Arc<dyn Toolchain> = match platform {
            ToolchainPlatform::Msvc => Arc::new(MsvcToolchain::new(
                PathBuf::from("cl.exe"),
                PathBuf::from("lib.exe"),
                PathBuf::from("link.exe"),
            )),
            other => Arc::new(GccToolchain::new(
                PathBuf::from("cc"),
                PathBuf::from("c++"),
                PathBuf::from("ar"),
                other,
            )),
        };

        BuildContext {
            toolchain,
            target: TargetTriple::host(),
            compiler: CompilerIdentity::new("gcc", "13.0"),
            platform: TargetPlatform::host(),
            profile,
            profile_name: "release".to_string(),
            output_dir: PathBuf::from("target"),
            deps_dir: PathBuf::from("target/deps"),
            workspace_root: PathBuf::from("."),
            cpp_constraints: None,
            vcpkg: None,
            target_cflags: Vec::new(),
            target_ldflags: Vec::new(),
        }
    }

    /// The regression this exists for: `lto = true` used to reach the link
    /// line only, so the compiler never emitted IR and LTO never happened.
    /// Both command lines have to carry it.
    #[test]
    fn test_lto_reaches_compile_and_link() {
        for platform in [
            ToolchainPlatform::Gcc,
            ToolchainPlatform::Clang,
            ToolchainPlatform::AppleClang,
        ] {
            let ctx = lto_ctx(platform);
            assert!(
                ctx.profile_cflags().unwrap().contains(&"-flto".to_string()),
                "no -flto on the compile line for {:?}",
                platform
            );
            assert!(
                ctx.profile_ldflags()
                    .unwrap()
                    .contains(&"-flto".to_string()),
                "no -flto on the link line for {:?}",
                platform
            );
        }
    }

    /// MSVC spells the two halves differently. Not reachable on a non-Windows
    /// machine as a real build, so this asserts on the generated argv only.
    #[test]
    fn test_lto_msvc_spelling() {
        let ctx = lto_ctx(ToolchainPlatform::Msvc);
        assert!(ctx.profile_cflags().unwrap().contains(&"/GL".to_string()));
        assert!(ctx
            .profile_ldflags()
            .unwrap()
            .contains(&"/LTCG".to_string()));
        assert!(!ctx.profile_cflags().unwrap().contains(&"-flto".to_string()));
        assert!(!ctx
            .profile_ldflags()
            .unwrap()
            .contains(&"-flto".to_string()));
    }

    /// `lto` unset or false must not put anything on either line.
    #[test]
    fn test_lto_off_emits_nothing() {
        for lto in [None, Some(false)] {
            let mut ctx = lto_ctx(ToolchainPlatform::Gcc);
            ctx.profile.lto = lto;
            assert!(!ctx
                .profile_cflags()
                .unwrap()
                .iter()
                .any(|f| f.contains("lto")));
            assert!(!ctx
                .profile_ldflags()
                .unwrap()
                .iter()
                .any(|f| f.contains("lto")));
        }
    }

    /// Build the MSVC compile command a manifest would really produce, on
    /// whatever host is running the tests.
    ///
    /// `compile_spec` takes the toolchain from the context, so pointing a
    /// context at `MsvcToolchain` exercises the entire decision chain --
    /// manifest -> `CppConstraints::compute` -> `cxx_options` -> backend
    /// argv -- without Windows or `cl.exe`. Only what `cl.exe` subsequently
    /// *does* with those flags is out of reach here.
    fn msvc_argv_for(manifest_body: &str) -> Vec<String> {
        msvc_argv_with_profile(manifest_body, "none")
    }

    /// As [`msvc_argv_for`], but with the named profile in play.
    ///
    /// The profile is read off the loaded manifest rather than passed in, so
    /// the flags under test are the ones `harbour build` would really
    /// resolve -- defaults included, which is where `opt_level = "3"` and
    /// `debug = "2"` come from.
    fn msvc_argv_with_profile(manifest_body: &str, profile_name: &str) -> Vec<String> {
        use crate::builder::plan::CompileStep;
        use crate::builder::toolchain::MsvcToolchain;
        use crate::core::target::Language;

        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("p");
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("Harbour.toml"),
            format!(
                "[package]\nname = \"p\"\nversion = \"1.0.0\"\n\n{manifest_body}\n\
                 [targets.p]\nkind = \"exe\"\nlang = \"c++\"\ncpp_std = \"17\"\n\
                 sources = [\"src/m.cpp\"]\n"
            ),
        )
        .unwrap();
        std::fs::write(dir.join("src/m.cpp"), "int main(){}\n").unwrap();

        let manifest = crate::core::Manifest::load(&dir.join("Harbour.toml")).unwrap();
        let profile = match profile_name {
            "debug" => manifest.debug_profile(),
            "release" => manifest.release_profile(),
            // No profile at all, for the tests that are only interested in
            // the C++ language options.
            _ => Profile::default(),
        };
        let build_config = manifest.build.clone();
        let source = crate::core::SourceId::for_path(&dir).unwrap();
        let pkg_id = crate::core::PackageId::new("p", "1.0.0".parse().unwrap(), source);
        let package = crate::core::Package::with_source_id(manifest, dir.clone(), source).unwrap();

        let mut resolve = crate::resolver::Resolve::new();
        resolve.add_package(pkg_id, crate::core::Summary::new(pkg_id, vec![], None));
        let mut packages = std::collections::HashMap::new();
        packages.insert(pkg_id, package);

        let constraints =
            CppConstraints::compute(&resolve, &packages, &build_config, None).unwrap();

        let ctx = BuildContext {
            toolchain: Arc::new(MsvcToolchain::new(
                PathBuf::from("cl"),
                PathBuf::from("lib"),
                PathBuf::from("link"),
            )),
            target: TargetTriple::host(),
            compiler: CompilerIdentity::new("msvc", "19.0"),
            platform: TargetPlatform::host(),
            profile,
            profile_name: profile_name.to_string(),
            output_dir: PathBuf::from("target"),
            deps_dir: PathBuf::from("target/deps"),
            workspace_root: dir.clone(),
            cpp_constraints: Some(constraints),
            vcpkg: None,
            target_cflags: Vec::new(),
            target_ldflags: Vec::new(),
        };

        let step = CompileStep {
            source: dir.join("src/m.cpp"),
            output: dir.join("m.obj"),
            include_dirs: vec![],
            defines: vec![],
            cflags: vec![],
            lang: Language::Cxx,
            package: "p".to_string(),
            target: "p".to_string(),
        };
        ctx.compile_spec(&step).unwrap().args
    }

    /// The defect in <https://github.com/aryamurray/harbour/issues/100>, at
    /// the level it was actually reported: not "does the backend spell `/O2`"
    /// but "does the argv `cl.exe` is handed contain it".
    ///
    /// The whole chain runs -- manifest, `Manifest::release_profile`,
    /// `ProfileOptions`, `MsvcToolchain`, `compile_spec` -- and this is the
    /// argv `NativeBuilder::compile` and `compile_commands.json` both use. On
    /// `main` it contained `-O3`, which `cl` answered with `D9002: ignoring
    /// unknown option '-O3'`, so every Windows release build was unoptimised.
    #[test]
    fn a_windows_release_build_is_actually_optimised() {
        let args = msvc_argv_with_profile("", "release");

        assert!(args.contains(&"/O2".to_string()), "{args:?}");
        assert!(
            !args.iter().any(|a| a.starts_with("-O")),
            "a GCC `-O` on a `cl` command line is the bug: {args:?}"
        );
        // `debug = "0"` in the release profile: no debug information asked
        // for, so none emitted.
        assert!(!args.contains(&"/Z7".to_string()), "{args:?}");
    }

    /// And the debug profile's half of the same thing. `/Od` is a real
    /// instruction to `cl`, not a no-op: without it `cl`'s own default
    /// applies, and `-O0` never disabled anything.
    #[test]
    fn a_windows_debug_build_carries_debug_info_and_no_optimisation() {
        let args = msvc_argv_with_profile("", "debug");

        assert!(args.contains(&"/Od".to_string()), "{args:?}");
        assert!(args.contains(&"/Z7".to_string()), "{args:?}");
        assert!(
            !args.iter().any(|a| a == "-g" || a == "-g3" || a == "-O0"),
            "no GCC-syntax profile flag may reach `cl`: {args:?}"
        );
    }

    /// A manifest with no `[build]` table must reach the MSVC backend with
    /// exceptions and RTTI *enabled*.
    ///
    /// This is the Windows half of the defect that made `[build] exceptions`
    /// and `rtti` default to `false`: the serde per-field defaults only fire
    /// for a key missing from a table that is present, so a manifest never
    /// mentioning `[build]` fell to `BuildConfig::default()`. That is fixed
    /// at the manifest layer, but the fix is only worth anything if the value
    /// survives to the compiler on *every* backend, and MSVC spells the flag
    /// `/EHsc` rather than omitting `-fno-exceptions`. Asserting on the
    /// backend argv is what makes that checkable from a Mac.
    #[test]
    fn msvc_gets_exceptions_and_rtti_from_a_manifest_with_no_build_table() {
        let args = msvc_argv_for("");
        assert!(
            args.contains(&"/EHsc".to_string()),
            "MSVC enables exceptions only when handed `/EHsc`; its absence \
             means exceptions really are off: {args:?}"
        );
        assert!(
            !args.iter().any(|a| a.starts_with("/EHs-")),
            "the exceptions-disabling form must not appear: {args:?}"
        );
        assert!(
            !args.contains(&"/GR-".to_string()),
            "`/GR-` disables RTTI and must not appear when `rtti` defaults \
             true: {args:?}"
        );
    }

    /// And the other direction, so the assertion above is not vacuous.
    ///
    /// The negative spellings are read off the backend rather than guessed:
    /// MSVC disables exceptions with `/EHs-c-`, not `/EHsc-`.
    #[test]
    fn msvc_disables_exceptions_and_rtti_when_the_manifest_asks() {
        let args = msvc_argv_for("[build]\ncpp_std = \"17\"\nexceptions = false\nrtti = false\n");
        assert!(args.contains(&"/std:c++17".to_string()), "{args:?}");
        assert!(args.contains(&"/EHs-c-".to_string()), "{args:?}");
        assert!(args.contains(&"/GR-".to_string()), "{args:?}");
        assert!(!args.contains(&"/EHsc".to_string()), "{args:?}");
    }
}
