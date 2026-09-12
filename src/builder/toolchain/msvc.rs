//! MSVC toolchain implementation.

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

use crate::core::target::Language;

use super::{
    ArchiveInput, CommandSpec, CompileInput, CxxOptions, LinkInput, OptLevel, ProfileOptions,
    Sanitizer, Toolchain, ToolchainPlatform,
};

/// MSVC toolchain (Windows).
#[derive(Debug, Clone)]
pub struct MsvcToolchain {
    /// Path to cl.exe (compiler)
    pub cl: PathBuf,
    /// Path to lib.exe (librarian)
    pub lib: PathBuf,
    /// Path to link.exe (linker)
    pub link: PathBuf,
}

impl MsvcToolchain {
    /// Create a new MSVC toolchain.
    pub fn new(cl: PathBuf, lib: PathBuf, link: PathBuf) -> Self {
        MsvcToolchain { cl, lib, link }
    }
}

impl Toolchain for MsvcToolchain {
    fn platform(&self) -> ToolchainPlatform {
        ToolchainPlatform::Msvc
    }

    fn compiler_path(&self) -> &Path {
        &self.cl
    }

    fn cxx_compiler_path(&self) -> &Path {
        // MSVC uses the same cl.exe for both C and C++
        &self.cl
    }

    fn compile_command(
        &self,
        input: &CompileInput,
        lang: Language,
        cxx_opts: Option<&CxxOptions>,
    ) -> CommandSpec {
        let mut cmd = CommandSpec::new(&self.cl);

        // Quiet logo, compile only
        cmd = cmd.arg("/nologo");
        cmd = cmd.arg("/c");

        // C++ specific flags
        if lang == Language::Cxx {
            // Force C++ compilation
            cmd = cmd.arg("/TP");

            if let Some(opts) = cxx_opts {
                // C++ standard
                if let Some(std) = opts.std {
                    cmd = cmd.arg(format!("/std:{}", std.as_msvc_flag_value()));
                }

                // Exceptions
                if opts.exceptions {
                    cmd = cmd.arg("/EHsc");
                } else {
                    cmd = cmd.arg("/EHs-c-");
                }

                // RTTI
                if !opts.rtti {
                    cmd = cmd.arg("/GR-");
                }

                // MSVC runtime (/MD or /MT)
                let runtime_flag = if opts.is_debug {
                    opts.msvc_runtime.as_debug_flag()
                } else {
                    opts.msvc_runtime.as_flag()
                };
                cmd = cmd.arg(runtime_flag);
            }
        }

        // Include directories
        for dir in &input.include_dirs {
            cmd = cmd.arg(format!("/I{}", dir.display()));
        }

        // Defines
        for (name, value) in &input.defines {
            match value {
                Some(v) => cmd = cmd.arg(format!("/D{}={}", name, v)),
                None => cmd = cmd.arg(format!("/D{}", name)),
            }
        }

        // Custom flags
        cmd = cmd.args(input.cflags.iter().cloned());

        // Input
        cmd = cmd.arg(input.source.display().to_string());

        // Output
        cmd = cmd.arg(format!("/Fo{}", input.output.display()));

        cmd
    }

    /// The mapping this whole change exists for.
    ///
    /// # Optimisation
    ///
    /// MSVC's set is smaller than GCC's and is not a prefix of it:
    ///
    /// | manifest | MSVC   | why |
    /// |----------|--------|-----|
    /// | `0`      | `/Od`  | disables optimisation, the documented equivalent |
    /// | `1`      | `/O1`  | "minimum size code" |
    /// | `2`      | `/O2`  | "maximum speed" |
    /// | `3`      | `/O2`  | MSVC has nothing above `/O2`; `/Ox` is documented as a *strict subset* of it, and `/Og` is deprecated |
    /// | `s`      | `/O1`  | `/O1` *is* the size preset; bare `/Os` is only a size-vs-speed preference within an enabled optimisation level |
    /// | `z`      | `/O1`  | no separate "smaller still" level exists |
    /// | `g`      | error  | `-Og` is "optimise but stay debuggable"; MSVC has no such mode, and picking either `/Od` or `/O2` would silently deliver the opposite of half of it |
    /// | `fast`   | error  | `-Ofast` enables standards-violating maths. `/fp:fast` is *not* the same set, and quietly turning it on is not a decision Harbour should make for you |
    ///
    /// `3 -> /O2`, `s -> /O1` and `z -> /O1` are lossy but directionally
    /// right, so they map. `g` and `fast` have no counterpart in either
    /// direction, so they are rejected with the portable alternative named.
    /// The alternative -- mapping them to something arbitrary -- is how you
    /// get a manifest that says `fast` and a binary that isn't.
    ///
    /// # Debug information
    ///
    /// `/Z7`, not `/Zi`, and the choice matters for correctness rather than
    /// taste. `/Zi` writes a separate PDB, and the compiler names it
    /// `<project>.pdb` -- or `VC<x>.pdb` for a file compiled outside a
    /// project, which is exactly what Harbour does. Harbour compiles in
    /// parallel, so every `cl` in a target would be writing one shared
    /// `VC143.pdb`. `/Z7` puts the CodeView information in the `.obj`
    /// instead: no side file, nothing shared, and nothing that can go stale
    /// relative to the object the fingerprint cache is tracking.
    ///
    /// MSVC has no level gradation (`debug = "1"` and `"2"` are the same
    /// command line), and `/Z7` alone produces no PDB at all -- the linker
    /// needs [`/DEBUG`](Self::profile_link_flags) to build one for the image.
    ///
    /// # Sanitizers
    ///
    /// `address` becomes `/fsanitize=address` (VS 2019 16.9 and later).
    /// MSVC implements no user-mode equivalent of `thread`, `memory`,
    /// `undefined` or `leak`, so those are an error. Before this, they were
    /// passed as `-fsanitize=thread`, which `cl` reported as `D9002:
    /// ignoring unknown option` and then ignored -- a build that claimed to
    /// be sanitized and was not.
    fn profile_compile_flags(&self, opts: &ProfileOptions) -> Result<Vec<String>> {
        let mut flags = Vec::new();

        if let Some(level) = opts.opt_level {
            flags.push(
                match level {
                    OptLevel::None => "/Od",
                    OptLevel::Basic => "/O1",
                    OptLevel::Standard => "/O2",
                    OptLevel::Aggressive => "/O2",
                    OptLevel::Size => "/O1",
                    OptLevel::SizeAggressive => "/O1",
                    OptLevel::Debug | OptLevel::Fast => bail!(
                        "`[profile] opt_level = \"{}\"` has no MSVC equivalent\n\
                         help: MSVC offers /Od, /O1 and /O2 only. Use \"0\", \"1\", \"2\" \
                         or \"3\", or put the exact flag you want in `[profile] cflags` \
                         under a `compiler = \"msvc\"` condition",
                        level.as_manifest_value()
                    ),
                }
                .to_string(),
            );
        }

        // One flag for either level: MSVC's debug information is on or off.
        if opts.debug.enabled() {
            flags.push("/Z7".to_string());
        }

        for sanitizer in &opts.sanitizers {
            match sanitizer {
                Sanitizer::Address => flags.push("/fsanitize=address".to_string()),
                Sanitizer::Thread | Sanitizer::Memory | Sanitizer::Undefined | Sanitizer::Leak => {
                    bail!(
                        "`[profile] sanitizers` includes `{}`, which MSVC does not implement\n\
                         help: MSVC supports `address` only. Remove it, or build \
                         the sanitized profile with a GCC or clang toolchain",
                        sanitizer.as_str()
                    )
                }
            }
        }

        // The compile half of LTO. MSVC spells the two halves differently
        // (`/GL` to compile, `/LTCG` to link) where the Unix compilers reuse
        // `-flto`.
        if opts.lto {
            flags.push("/GL".to_string());
        }

        Ok(flags)
    }

    /// The link half of the same intent.
    ///
    /// Three differences from GCC/clang, all of them MSVC being MSVC:
    ///
    /// - **`/DEBUG` is required.** `/Z7` leaves the debug information in the
    ///   objects and the linker emits no PDB without being asked, so a
    ///   `debug`-enabled Windows build would produce a binary you cannot set
    ///   a breakpoint in. `/DEBUG` is what turns `/Z7`'s CodeView records
    ///   into a `.pdb` beside the image.
    /// - **Sanitizers contribute nothing here.** `/fsanitize=address` marks
    ///   the objects with the ASan library they need, and `/INFERASANLIBS`
    ///   (on by default) makes the linker resolve them. Passing
    ///   `/fsanitize=address` to `link.exe` would only earn an unknown-option
    ///   warning.
    /// - **ASan forces `/INCREMENTAL:NO`.** Incremental linking is documented
    ///   as unsupported with ASan, and `/DEBUG` turns incremental linking
    ///   *on* by default -- so the combination Harbour would otherwise
    ///   produce for a sanitized debug build is precisely the unsupported
    ///   one.
    fn profile_link_flags(&self, opts: &ProfileOptions) -> Result<Vec<String>> {
        let mut flags = Vec::new();

        if opts.lto {
            flags.push("/LTCG".to_string());
        }

        if opts.debug.enabled() {
            flags.push("/DEBUG".to_string());
        }

        // Reject here too, rather than letting the link line quietly agree to
        // something the compile line refused: these two methods are called
        // from different places, and a caller that only ever asks for link
        // flags must get the same answer.
        for sanitizer in &opts.sanitizers {
            match sanitizer {
                Sanitizer::Address => {
                    if !flags.iter().any(|f| f == "/INCREMENTAL:NO") {
                        flags.push("/INCREMENTAL:NO".to_string());
                    }
                }
                other => bail!(
                    "`[profile] sanitizers` includes `{}`, which MSVC does not implement\n\
                     help: MSVC supports `address` only. Remove it, or build \
                     the sanitized profile with a GCC or clang toolchain",
                    other.as_str()
                ),
            }
        }

        Ok(flags)
    }

    fn archive_command(&self, input: &ArchiveInput) -> CommandSpec {
        let mut cmd = CommandSpec::new(&self.lib);

        cmd = cmd.arg("/nologo");
        cmd = cmd.arg(format!("/OUT:{}", input.output.display()));

        // Object files
        for obj in &input.objects {
            cmd = cmd.arg(obj.display().to_string());
        }

        cmd
    }

    fn link_shared_command(
        &self,
        input: &LinkInput,
        _driver: Language,
        _cxx_opts: Option<&CxxOptions>,
    ) -> CommandSpec {
        // MSVC uses link.exe for both C and C++ linking
        let mut cmd = CommandSpec::new(&self.link);

        cmd = cmd.arg("/nologo");
        cmd = cmd.arg("/DLL");
        cmd = cmd.arg(format!("/OUT:{}", input.output.display()));

        // Object files
        for obj in &input.objects {
            cmd = cmd.arg(obj.display().to_string());
        }

        // Library search paths
        for dir in &input.lib_dirs {
            cmd = cmd.arg(format!("/LIBPATH:{}", dir.display()));
        }

        // Libraries
        for lib in &input.libs {
            cmd = cmd.arg(format!("{}.lib", lib));
        }

        // Custom flags
        cmd = cmd.args(input.ldflags.iter().cloned());

        cmd
    }

    fn link_exe_command(
        &self,
        input: &LinkInput,
        _driver: Language,
        _cxx_opts: Option<&CxxOptions>,
    ) -> CommandSpec {
        // MSVC uses link.exe for both C and C++ linking
        let mut cmd = CommandSpec::new(&self.link);

        cmd = cmd.arg("/nologo");
        cmd = cmd.arg(format!("/OUT:{}", input.output.display()));

        // Object files
        for obj in &input.objects {
            cmd = cmd.arg(obj.display().to_string());
        }

        // Library search paths
        for dir in &input.lib_dirs {
            cmd = cmd.arg(format!("/LIBPATH:{}", dir.display()));
        }

        // Libraries
        for lib in &input.libs {
            cmd = cmd.arg(format!("{}.lib", lib));
        }

        // Custom flags
        cmd = cmd.args(input.ldflags.iter().cloned());

        cmd
    }

    fn object_extension(&self) -> &str {
        "obj"
    }

    fn static_lib_extension(&self) -> &str {
        "lib"
    }

    fn shared_lib_extension(&self) -> &str {
        "dll"
    }

    fn exe_extension(&self) -> &str {
        "exe"
    }

    fn static_lib_prefix(&self) -> &str {
        ""
    }

    fn shared_lib_prefix(&self) -> &str {
        ""
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::toolchain::{DebugInfo, OptLevel, ProfileOptions, Sanitizer};

    fn msvc() -> MsvcToolchain {
        MsvcToolchain::new(
            PathBuf::from("cl.exe"),
            PathBuf::from("lib.exe"),
            PathBuf::from("link.exe"),
        )
    }

    fn cflags(opts: &ProfileOptions) -> Vec<String> {
        msvc().profile_compile_flags(opts).unwrap()
    }

    fn opt(level: OptLevel) -> ProfileOptions {
        ProfileOptions {
            opt_level: Some(level),
            ..Default::default()
        }
    }

    /// The whole table, so a future edit to one arm cannot quietly change
    /// another. Every one of these used to be emitted in GCC syntax and
    /// ignored by `cl` with `D9002`.
    #[test]
    fn every_optimisation_level_gets_an_msvc_spelling() {
        assert_eq!(cflags(&opt(OptLevel::None)), ["/Od"]);
        assert_eq!(cflags(&opt(OptLevel::Basic)), ["/O1"]);
        assert_eq!(cflags(&opt(OptLevel::Standard)), ["/O2"]);
        // No `/O3` exists; `/Ox` is documented as a strict subset of `/O2`.
        assert_eq!(cflags(&opt(OptLevel::Aggressive)), ["/O2"]);
        // `/O1` *is* the size preset. Bare `/Os` only expresses a preference
        // within an already-enabled level.
        assert_eq!(cflags(&opt(OptLevel::Size)), ["/O1"]);
        assert_eq!(cflags(&opt(OptLevel::SizeAggressive)), ["/O1"]);
    }

    /// The two with no counterpart in either direction. Mapping them to
    /// `/Od` or `/O2` would deliver the opposite of half of what was asked
    /// for, and emitting nothing is the bug being fixed.
    #[test]
    fn the_two_unmappable_levels_are_an_error_not_a_guess() {
        for level in [OptLevel::Debug, OptLevel::Fast] {
            let err = msvc()
                .profile_compile_flags(&opt(level))
                .unwrap_err()
                .to_string();
            assert!(err.contains("no MSVC equivalent"), "{err}");
            assert!(
                err.contains(level.as_manifest_value()),
                "the error must name the value that was rejected: {err}"
            );
        }
    }

    /// `/Z7`, not `/Zi`: the compiler names a `/Zi` PDB `VC<x>.pdb` for a
    /// file compiled outside a project, so Harbour's parallel `cl`
    /// invocations would all be writing one shared file.
    #[test]
    fn debug_info_is_embedded_in_the_object_and_the_linker_is_told_to_keep_it() {
        for level in [DebugInfo::Limited, DebugInfo::Full] {
            let opts = ProfileOptions {
                debug: level,
                ..Default::default()
            };
            assert_eq!(cflags(&opts), ["/Z7"], "{level:?}");
            assert!(
                !cflags(&opts).iter().any(|f| f.starts_with("/Zi")),
                "a shared-PDB /Zi must never be emitted: {:?}",
                cflags(&opts)
            );
            // Without this the CodeView records in the objects never become
            // a PDB, and the image cannot be debugged.
            assert_eq!(msvc().profile_link_flags(&opts).unwrap(), ["/DEBUG"]);
        }
    }

    #[test]
    fn no_debug_info_means_no_flags_at_all() {
        let opts = ProfileOptions::default();
        assert!(cflags(&opts).is_empty());
        assert!(msvc().profile_link_flags(&opts).unwrap().is_empty());
    }

    /// ASan is the one sanitizer MSVC implements, and the linker resolves its
    /// runtime from the objects (`/INFERASANLIBS`, on by default), so
    /// `/fsanitize=address` belongs on the compile line only. Incremental
    /// linking is documented as unsupported with ASan, and `/DEBUG` turns
    /// incremental linking on by default -- so the sanitized debug build is
    /// exactly the combination that needs `/INCREMENTAL:NO`.
    #[test]
    fn address_sanitizer_is_a_compile_flag_and_forces_a_non_incremental_link() {
        let opts = ProfileOptions {
            debug: DebugInfo::Full,
            sanitizers: vec![Sanitizer::Address],
            ..Default::default()
        };
        assert_eq!(cflags(&opts), ["/Z7", "/fsanitize=address"]);

        let ldflags = msvc().profile_link_flags(&opts).unwrap();
        assert_eq!(ldflags, ["/DEBUG", "/INCREMENTAL:NO"]);
        assert!(
            !ldflags.iter().any(|f| f.contains("sanitize")),
            "`/fsanitize=address` is not a linker option: {ldflags:?}"
        );
    }

    /// The other four do not exist on MSVC. They used to be passed as
    /// `-fsanitize=thread`, which `cl` answered with `D9002: ignoring unknown
    /// option` -- a build that claimed to be sanitized and was not.
    #[test]
    fn the_sanitizers_msvc_lacks_are_an_error_on_both_command_lines() {
        for sanitizer in [
            Sanitizer::Thread,
            Sanitizer::Memory,
            Sanitizer::Undefined,
            Sanitizer::Leak,
        ] {
            let opts = ProfileOptions {
                sanitizers: vec![sanitizer],
                ..Default::default()
            };
            for err in [
                msvc().profile_compile_flags(&opts).unwrap_err(),
                msvc().profile_link_flags(&opts).unwrap_err(),
            ] {
                let err = err.to_string();
                assert!(err.contains(sanitizer.as_str()), "{err}");
                assert!(err.contains("MSVC does not implement"), "{err}");
            }
        }
    }

    /// Nothing in the MSVC spelling may be GCC-shaped. This is the assertion
    /// that would have failed on `main` for every flag at once.
    #[test]
    fn nothing_msvc_emits_is_in_gcc_syntax() {
        let opts = ProfileOptions {
            opt_level: Some(OptLevel::Aggressive),
            debug: DebugInfo::Full,
            sanitizers: vec![Sanitizer::Address],
            lto: true,
        };
        let all: Vec<String> = cflags(&opts)
            .into_iter()
            .chain(msvc().profile_link_flags(&opts).unwrap())
            .collect();
        assert!(
            all.iter().all(|f| f.starts_with('/')),
            "MSVC takes `/`-prefixed options: {all:?}"
        );
        assert_eq!(
            all,
            [
                "/O2",
                "/Z7",
                "/fsanitize=address",
                "/GL",
                "/LTCG",
                "/DEBUG",
                "/INCREMENTAL:NO"
            ]
        );
    }
}
