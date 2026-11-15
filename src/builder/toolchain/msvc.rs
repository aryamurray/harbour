//! MSVC toolchain implementation.

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

use crate::core::target::{Language, WarningLevel, WarningOptions};

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

/// System libraries that exist on Unix and have no MSVC counterpart: libm,
/// libdl, libpthread, librt and libc are all provided by the C runtime (or, for
/// `dl`/`rt`, not needed) there. A manifest says `libs = ["m"]` because that is
/// how every Unix project asks for the math library, and rendering it as `m.lib`
/// names a file that exists in no MSVC installation: `link.exe` fails with
/// LNK1181, and a native probe that used it would answer "no" for a function
/// that is right there (the spike found both; cJSON's link failed on a Windows
/// runner for exactly this reason).
const UNIX_ONLY_LIBS: [&str; 5] = ["m", "dl", "pthread", "rt", "c"];

/// How one `libs` entry is spelled on a `link.exe`/`lib.exe` command line, or
/// `None` when it names something MSVC has no library for. Anything else is
/// passed through: `ws2_32` becomes `ws2_32.lib`, and a name that already ends
/// in `.lib` is not suffixed twice.
fn msvc_library_arg(lib: &str) -> Option<String> {
    if UNIX_ONLY_LIBS.contains(&lib) {
        tracing::debug!("not linking `{lib}` under MSVC: the C runtime provides it");
        return None;
    }
    if lib.to_ascii_lowercase().ends_with(".lib") {
        Some(lib.to_string())
    } else {
        Some(format!("{lib}.lib"))
    }
}

/// `cl`'s stdout from a successful compile of `source`, less the one line that
/// is not a diagnostic.
///
/// `cl` writes its file-level diagnostics (`main.c(3): warning C4189: ...`) to
/// stdout, not stderr. It also starts every compile by echoing the name of the
/// file it was given, without its directory, on a line of its own; `/nologo`
/// removes the banner but not that echo. Relaying stdout whole would therefore
/// print `main.c` for every file of a build that has nothing to say.
///
/// The rule is exact, so that it cannot take a diagnostic with it: the
/// **first** line is dropped if, with its line ending removed, it **equals**
/// the source file's name. Nothing else is touched. Not "any line that looks
/// like a file name", and not "that line wherever it appears": a
/// `#pragma message("main.c")` further down is the user's own output and stays.
///
/// Harbour hands `cl` one source per command, so the multi-file chatter
/// (`Compiling...`, `Generating Code...`) does not occur and is not looked for.
fn cl_stdout_without_echo<'a>(source: &Path, stdout: &'a str) -> &'a str {
    let Some(name) = source.file_name().and_then(|n| n.to_str()) else {
        return stdout;
    };
    let (first, rest) = match stdout.split_once('\n') {
        Some((first, rest)) => (first, rest),
        None => (stdout, ""),
    };
    if first.trim_end_matches('\r') == name {
        rest
    } else {
        stdout
    }
}

/// Whether a line of `link.exe` / `lib.exe` stdout is a progress report rather
/// than a diagnostic.
///
/// Both tools write warnings (`LINK : warning LNK4098: ...`,
/// `a.obj : warning LNK4221: ...`) to stdout, and a few status lines with them
/// that `/nologo` does not remove:
///
/// - `   Creating library foo.lib and object foo.exp`, whenever an image
///   exports something and so gets an import library;
/// - `Generating code` and `Finished generating code`, under `/LTCG`.
///
/// Only these are recognised, by their whole shape, and everything else is
/// kept: an unknown line is more likely something the user should read than
/// something to hide.
fn is_link_progress_line(line: &str) -> bool {
    let line = line.trim();
    line == "Generating code"
        || line == "Finished generating code"
        || (line.starts_with("Creating library ") && line.contains(" and object "))
}

/// `link.exe` / `lib.exe` stdout with the progress lines removed and every
/// other line as it was, line ending included.
fn link_stdout_without_progress(stdout: &str) -> String {
    stdout
        .split_inclusive('\n')
        .filter(|line| !is_link_progress_line(line))
        .collect()
}

/// stderr, then what is left of stdout, the order `failure_text` uses for a
/// failed tool. `cl` puts its command-line warnings (`D9002`) on stderr and
/// they are about the command as a whole, so they read first.
fn join_streams(stderr: &[u8], stdout: &str) -> String {
    let stderr = String::from_utf8_lossy(stderr);
    match (stderr.trim().is_empty(), stdout.trim().is_empty()) {
        (_, true) => stderr.into_owned(),
        (true, false) => stdout.to_string(),
        (false, false) => format!("{}\n{}", stderr.trim_end(), stdout),
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
            }
        }

        // The C runtime, for C and C++ alike, when the build supplies options. It
        // used to be emitted only inside the C++ block above, so a C file was
        // compiled with `cl`'s own default, `/MT` (static), whatever `[build]
        // msvc_runtime` said and whatever the C++ files beside it were given: a
        // build with both languages then failed to link on a `RuntimeLibrary`
        // mismatch, and a C library that looks its functions up in the CRT's DLL
        // (LuaJIT's `ffi.C`) found no such module.
        //
        // No options means a probe, which keeps `cl`'s default on purpose: a
        // `check_symbols` probe links with no library named and relies on the
        // `/DEFAULTLIB`s the object carries, and under `/MD` a function the UCRT
        // headers define inline (`printf`, `snprintf`) has no out-of-line symbol to
        // find, so the probe would answer `no` for something every program has.
        if matches!(lang, Language::C | Language::Cxx) {
            if let Some(opts) = cxx_opts {
                cmd = cmd.arg(if opts.is_debug {
                    opts.msvc_runtime.as_debug_flag()
                } else {
                    opts.msvc_runtime.as_flag()
                });
            }
        }

        // C standard, where `cl` has one. It only has `/std:c11` and
        // `/std:c17`; there is no `/std:c89`, no `/std:c99` and no GNU
        // dialect, so `CStandardSpec::as_msvc_flag_value` returns `None`
        // for those and nothing is emitted. Emitting a GCC-shaped
        // `-std=c99` instead would be an unknown option to `cl`, and
        // emitting nothing *quietly* is the failure mode this whole change
        // is about -- so `BuildPlan` warns, once per target, naming the
        // standard and what `cl` will actually use.
        if let (Language::C, Some(c_std)) = (lang, input.c_std) {
            if let Some(value) = c_std.as_msvc_flag_value() {
                cmd = cmd.arg(format!("/std:{}", value));
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
                         or \"3\". For an MSVC flag this schema cannot name, put it \
                         in a target's `surface.when` block guarded by \
                         `compiler = \"msvc\"`",
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

    /// `all` is `/W4`, which is what `cl`'s own documentation recommends for
    /// new code. `/Wall` is not "all" in the GCC sense: it enables every
    /// off-by-default warning, including ones the Windows SDK headers trip,
    /// and no project keeps it clean.
    ///
    /// `pedantic` is also `/W4`. `-Wpedantic` asks for the diagnostics strict
    /// ISO conformance requires, and `cl` has no warnings-only switch for
    /// that: `/permissive-` and `/Za` change what the compiler *accepts*
    /// rather than what it reports, and `/Za` cannot compile `<windows.h>`.
    /// The level is not an error here because "as strict as this compiler
    /// goes" is still what was asked for.
    ///
    /// `none` is `/w`, and `warnings_as_errors` is `/WX`.
    fn warning_flags(&self, opts: &WarningOptions) -> Vec<String> {
        let mut flags = Vec::new();

        match opts.level {
            WarningLevel::None => flags.push("/w".to_string()),
            WarningLevel::Default => {}
            WarningLevel::All | WarningLevel::Pedantic => flags.push("/W4".to_string()),
        }

        if opts.as_errors {
            flags.push("/WX".to_string());
        }

        flags
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
        for lib in input.libs.iter().filter_map(|l| msvc_library_arg(l)) {
            cmd = cmd.arg(lib);
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
        for lib in input.libs.iter().filter_map(|l| msvc_library_arg(l)) {
            cmd = cmd.arg(lib);
        }

        // Custom flags
        cmd = cmd.args(input.ldflags.iter().cloned());

        cmd
    }

    fn compile_diagnostics(&self, source: &Path, stdout: &[u8], stderr: &[u8]) -> String {
        let stdout = String::from_utf8_lossy(stdout);
        join_streams(stderr, cl_stdout_without_echo(source, &stdout))
    }

    fn link_diagnostics(&self, stdout: &[u8], stderr: &[u8]) -> String {
        let stdout = String::from_utf8_lossy(stdout);
        join_streams(stderr, &link_stdout_without_progress(&stdout))
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
    #[test]
    fn unix_only_libraries_are_not_linked_under_msvc() {
        for name in ["m", "dl", "pthread", "rt", "c"] {
            assert_eq!(msvc_library_arg(name), None, "{name} has no MSVC library");
        }
    }

    #[test]
    fn real_libraries_keep_their_lib_suffix_and_are_not_suffixed_twice() {
        assert_eq!(msvc_library_arg("ws2_32").as_deref(), Some("ws2_32.lib"));
        assert_eq!(msvc_library_arg("version").as_deref(), Some("version.lib"));
        assert_eq!(
            msvc_library_arg("bcrypt.lib").as_deref(),
            Some("bcrypt.lib")
        );
        assert_eq!(
            msvc_library_arg("User32.LIB").as_deref(),
            Some("User32.LIB")
        );
    }

    #[test]
    fn the_link_command_drops_unix_only_libraries_and_keeps_the_rest() {
        let input = LinkInput {
            objects: vec![PathBuf::from("a.obj")],
            output: PathBuf::from("a.exe"),
            lib_dirs: vec![],
            libs: vec!["m".into(), "ws2_32".into(), "pthread".into()],
            ldflags: vec![],
            frameworks: vec![],
        };
        for spec in [
            msvc().link_exe_command(&input, Language::C, None),
            msvc().link_shared_command(&input, Language::C, None),
        ] {
            let args: Vec<String> = spec.args.iter().map(|a| a.to_string()).collect();
            assert!(args.contains(&"ws2_32.lib".to_string()), "{args:?}");
            assert!(
                !args.iter().any(|a| a == "m.lib" || a == "pthread.lib"),
                "Unix-only libraries must not become nonexistent .lib files: {args:?}"
            );
        }
    }

    use super::*;
    use crate::builder::toolchain::{DebugInfo, OptLevel, ProfileOptions, Sanitizer};
    use crate::core::target::{CStandard, CStandardSpec};

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

    /// The MSVC spelling of every `warnings` level, pinned in full.
    ///
    /// `pedantic` is `/W4` and nothing more: `cl` has no warnings-only
    /// counterpart to `-Wpedantic`, and `/permissive-` or `/Za` would change
    /// what compiles. `/Wall` must never appear -- it is not GCC's `-Wall`.
    #[test]
    fn warning_levels_are_spelled_in_msvc_syntax() {
        let flags = |level, as_errors| msvc().warning_flags(&WarningOptions { level, as_errors });

        assert_eq!(flags(WarningLevel::None, false), ["/w"]);
        assert!(flags(WarningLevel::Default, false).is_empty());
        assert_eq!(flags(WarningLevel::All, false), ["/W4"]);
        assert_eq!(flags(WarningLevel::Pedantic, false), ["/W4"]);
        assert_eq!(flags(WarningLevel::All, true), ["/W4", "/WX"]);
        assert_eq!(flags(WarningLevel::Default, true), ["/WX"]);
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

    fn compile_args(c_std: Option<CStandardSpec>, lang: Language) -> Vec<String> {
        msvc()
            .compile_command(
                &CompileInput {
                    source: PathBuf::from("a.c"),
                    output: PathBuf::from("a.obj"),
                    include_dirs: vec![],
                    defines: vec![],
                    cflags: vec![],
                    c_std,
                },
                lang,
                None,
            )
            .args
    }

    /// The two standards `cl` actually has a switch for.
    ///
    /// Command construction only -- no `cl.exe` is run, so this can be
    /// pinned from any host. What `cl` *does* with `/std:c11` is checked by
    /// the `cfg(target_env = "msvc")` integration test on the
    /// `windows-latest` job, because guessing MSVC behaviour from a GCC
    /// mental model has produced two wrong expectations in this codebase
    /// already.
    #[test]
    fn c11_and_c17_become_msvc_std_switches() {
        assert!(
            compile_args(Some(CStandardSpec::iso(CStandard::C11)), Language::C)
                .contains(&"/std:c11".to_string())
        );
        assert!(
            compile_args(Some(CStandardSpec::iso(CStandard::C17)), Language::C)
                .contains(&"/std:c17".to_string())
        );
    }

    /// C89, C99 and C23 have no `/std:` switch on `cl`. Emitting nothing is
    /// deliberate: the GCC spelling would be an unknown option -- `cl`
    /// answers those with `D9002` and compiles anyway, which is the defect
    /// the profile-flag mapping above exists to fix -- and `/std:clatest` is
    /// not the same promise. `BuildPlan` warns, so the author is told rather
    /// than left to find out from behaviour.
    #[test]
    fn a_standard_cl_cannot_express_emits_no_flag_at_all() {
        for std in [CStandard::C89, CStandard::C99, CStandard::C23] {
            let args = compile_args(Some(CStandardSpec::iso(std)), Language::C);
            assert!(
                !args
                    .iter()
                    .any(|a| a.contains("/std:") || a.contains("-std")),
                "{std} has no `cl` switch, so nothing may be emitted for it: {args:?}"
            );
        }
    }

    /// The GNU dialect degrades to the ISO standard of the same version
    /// rather than being dropped entirely -- `gnu11` on `cl` is still
    /// closer to C11 than to `cl`'s default.
    #[test]
    fn the_gnu_dialect_degrades_to_the_iso_standard() {
        assert!(
            compile_args(Some(CStandardSpec::gnu(CStandard::C11)), Language::C)
                .contains(&"/std:c11".to_string())
        );
    }

    /// Assembly and C++ must not pick it up, for the same reasons as on
    /// gcc: `/std:` here describes the C dialect, and a C++ translation
    /// unit takes its standard from `CxxOptions`.
    #[test]
    fn only_c_gets_the_c_standard() {
        for lang in [Language::Asm, Language::Cxx] {
            let args = compile_args(Some(CStandardSpec::iso(CStandard::C11)), lang);
            assert!(
                !args.iter().any(|a| a == "/std:c11"),
                "{lang:?} must not be given the C standard: {args:?}"
            );
        }
    }

    // --- What a successful `cl`, `link` or `lib` said -----------------------
    //
    // The fixtures are `cl`'s and `link`'s documented output shapes with the
    // CRLF line endings the tools write. No MSVC is needed to run these; the
    // integration tests in `tests/cli_integration` check the same thing
    // against the real tools on a Windows runner.

    fn compile_said(source: &str, stdout: &str, stderr: &str) -> String {
        msvc().compile_diagnostics(Path::new(source), stdout.as_bytes(), stderr.as_bytes())
    }

    fn link_said(stdout: &str, stderr: &str) -> String {
        msvc().link_diagnostics(stdout.as_bytes(), stderr.as_bytes())
    }

    /// A clean compile: `cl` echoes the file name and nothing else.
    #[test]
    fn a_clean_compile_has_nothing_to_report() {
        assert_eq!(compile_said("C:/app/src/main.c", "main.c\r\n", ""), "");
        assert_eq!(compile_said("src/main.c", "main.c\n", ""), "");
        assert_eq!(compile_said("src/main.c", "main.c", ""), "");
        assert_eq!(compile_said("src/main.c", "", ""), "");
    }

    /// The warnings follow the echo on stdout and are relayed untouched.
    #[test]
    fn warnings_on_stdout_are_kept_in_order_and_verbatim() {
        let warnings = "C:\\app\\src\\main.c(2): warning C4100: 'ignored': unreferenced formal parameter\r\n\
                        C:\\app\\src\\main.c(4): warning C4189: 'never_read': local variable is initialized but not referenced\r\n";
        assert_eq!(
            compile_said("src/main.c", &format!("main.c\r\n{warnings}"), ""),
            warnings
        );
    }

    /// A diagnostic with continuation lines (a note, an indented template
    /// context) keeps every one of them.
    #[test]
    fn multi_line_diagnostics_keep_their_continuation_lines() {
        let warnings = "C:\\app\\src\\widget.cpp(12): warning C4244: 'argument': conversion from 'double' to 'int', possible loss of data\r\n\
                        C:\\app\\src\\widget.cpp(12): note: see reference to function template instantiation 'void take<double>(T)' being compiled\r\n\
                        \x20       with\r\n\
                        \x20       [\r\n\
                        \x20           T=double\r\n\
                        \x20       ]\r\n\
                        C:\\app\\include\\widget.h(3): note: see declaration of 'take'\r\n";
        assert_eq!(
            compile_said(
                "C:/app/src/widget.cpp",
                &format!("widget.cpp\r\n{warnings}"),
                ""
            ),
            warnings
        );
    }

    /// `cl` reports an option it does not know on stderr (`D9002`), before it
    /// compiles anything. It is shown first, then the file's own warnings.
    #[test]
    fn command_line_warnings_on_stderr_come_first() {
        let d9002 = "cl : Command line warning D9002 : ignoring unknown option '-O3'\r\n";
        let c4189 =
            "main.c(4): warning C4189: 'x': local variable is initialized but not referenced\r\n";

        assert_eq!(compile_said("main.c", "main.c\r\n", d9002), d9002);
        assert_eq!(
            compile_said("main.c", &format!("main.c\r\n{c4189}"), d9002),
            format!("{}\n{c4189}", d9002.trim_end())
        );
    }

    /// Only the first line, and only when it is the file name and nothing
    /// more. Each of these is a line that a looser rule would have eaten.
    #[test]
    fn nothing_but_the_echo_itself_is_removed() {
        // A diagnostic that starts with the file name is not the echo.
        let diag = "main.c(1): warning C4005: 'X': macro redefinition\r\n";
        assert_eq!(compile_said("main.c", diag, ""), diag);

        // Another file's name on the first line is not this compile's echo.
        assert_eq!(compile_said("main.c", "other.c\r\n", ""), "other.c\r\n");

        // The name later in the output is the program's author speaking
        // (`#pragma message("main.c")`), not `cl`.
        assert_eq!(
            compile_said("main.c", "main.c\r\nnote\r\nmain.c\r\n", ""),
            "note\r\nmain.c\r\n"
        );

        // No echo at all (a future `cl`, a wrapper): everything is kept.
        assert_eq!(
            compile_said("main.c", "note\r\nmain.c\r\n", ""),
            "note\r\nmain.c\r\n"
        );

        // The name with something after it is not the echo either.
        assert_eq!(compile_said("main.c", "main.c \r\n", ""), "main.c \r\n");
        assert_eq!(compile_said("main.c", "main.cpp\r\n", ""), "main.cpp\r\n");
    }

    /// A clean link says nothing, and the status lines `/nologo` leaves in
    /// are not diagnostics.
    #[test]
    fn link_progress_lines_are_not_reported() {
        assert_eq!(link_said("", ""), "");
        assert_eq!(
            link_said(
                "   Creating library target\\debug\\shared.lib and object target\\debug\\shared.exp\r\n",
                ""
            ),
            ""
        );
        assert_eq!(
            link_said("Generating code\r\nFinished generating code\r\n", ""),
            ""
        );
    }

    /// Linker and librarian warnings are on stdout and are kept verbatim,
    /// around any progress lines.
    #[test]
    fn link_warnings_on_stdout_are_kept() {
        let lnk4044 = "LINK : warning LNK4044: unrecognized option '/NOSUCHOPTION'; ignored\r\n";
        let lnk4099 = "dep.lib(dep.obj) : warning LNK4099: PDB 'dep.pdb' was not found with 'dep.lib(dep.obj)' or at 'C:\\app\\dep.pdb'; linking object as if no debug info\r\n";
        assert_eq!(
            link_said(
                &format!(
                    "{lnk4044}   Creating library a.lib and object a.exp\r\n{lnk4099}Generating code\r\nFinished generating code\r\n"
                ),
                ""
            ),
            format!("{lnk4044}{lnk4099}")
        );

        let lnk4221 = "empty.obj : warning LNK4221: This object file does not define any previously undefined public symbols, so it will not be used by any link operation that consumes this library\r\n";
        assert_eq!(link_said(lnk4221, ""), lnk4221);
    }

    /// A line that merely mentions a progress phrase is not a progress line.
    #[test]
    fn only_whole_progress_lines_are_recognised() {
        for line in [
            "LINK : warning LNK4098: defaultlib 'LIBCMT' conflicts with use of other libs; use /NODEFAULTLIB:library\r\n",
            "a.obj : warning LNK4006: Creating library symbol already defined in b.obj; second definition ignored\r\n",
            "Generating code failed\r\n",
        ] {
            assert_eq!(link_said(line, ""), line);
        }
    }

    /// The GNU toolchains are untouched: stderr, whole, and stdout ignored.
    #[test]
    fn gcc_reports_stderr_and_ignores_stdout() {
        use crate::builder::toolchain::gcc::GccToolchain;
        let gcc = GccToolchain::new(
            "cc".into(),
            "c++".into(),
            "ar".into(),
            ToolchainPlatform::Gcc,
        );
        let warning = "src/main.c:4:9: warning: unused variable 'x' [-Wunused-variable]\n";
        assert_eq!(
            gcc.compile_diagnostics(Path::new("src/main.c"), b"main.c\n", warning.as_bytes()),
            warning
        );
        assert_eq!(
            gcc.link_diagnostics(b"chatter\n", b"ld: warning\n"),
            "ld: warning\n"
        );
    }

    /// `EnvWrapper` is what a detected MSVC is actually handed around as; if
    /// it fell back to the trait's default the filter would never run.
    #[test]
    fn the_env_wrapper_asks_the_wrapped_toolchain() {
        let wrapped = crate::builder::toolchain::EnvWrapper::new(msvc(), Vec::new());
        let c4189 =
            "main.c(4): warning C4189: 'x': local variable is initialized but not referenced\r\n";
        assert_eq!(
            wrapped.compile_diagnostics(
                Path::new("main.c"),
                format!("main.c\r\n{c4189}").as_bytes(),
                b""
            ),
            c4189
        );
        assert_eq!(wrapped.link_diagnostics(b"Generating code\r\n", b""), "");
    }
}
