//! GCC/Clang toolchain implementation.

use std::path::{Path, PathBuf};

use crate::core::target::{Language, TargetTriple};

use super::{
    ArchiveInput, CommandSpec, CompileInput, CxxOptions, LinkInput, Toolchain, ToolchainPlatform,
};

/// GCC/Clang toolchain (Unix-like systems).
#[derive(Debug, Clone)]
pub struct GccToolchain {
    /// Path to the C compiler
    pub cc: PathBuf,
    /// Path to the C++ compiler
    pub cxx: PathBuf,
    /// Path to the archiver
    pub ar: PathBuf,
    /// Compiler family (gcc, clang, apple-clang)
    pub family: ToolchainPlatform,
    /// The target this toolchain builds *for*.
    ///
    /// Artifact naming is a property of the target, not of the machine running
    /// Harbour: cross-building for macOS from Linux must still produce
    /// `.dylib`. Defaults to the host so that host builds are unaffected.
    pub target: TargetTriple,
}

impl GccToolchain {
    /// Create a new GCC-style toolchain.
    pub fn new(cc: PathBuf, cxx: PathBuf, ar: PathBuf, family: ToolchainPlatform) -> Self {
        GccToolchain {
            target: TargetTriple::host(),
            cc,
            cxx,
            ar,
            family,
        }
    }

    /// Infer C++ compiler path from C compiler path.
    ///
    /// Handles common patterns:
    /// - gcc, x86_64-linux-gnu-gcc -> g++, x86_64-linux-gnu-g++
    /// - clang -> clang++
    /// - cc, /usr/bin/cc -> c++, /usr/bin/c++
    pub fn infer_cxx(cc: &Path) -> PathBuf {
        let cc_str = cc.to_string_lossy();

        // gcc or *-gcc -> g++ or *-g++
        if cc_str.ends_with("gcc") {
            return PathBuf::from(format!("{}++", &cc_str[..cc_str.len() - 2]));
        }

        // clang -> clang++
        if cc_str.ends_with("clang") {
            return PathBuf::from(format!("{}++", cc_str));
        }

        // Only match "cc" when it's a complete basename (not "mycc")
        // Check for: "cc", "/cc", or "-cc" at the end
        let is_standalone_cc = cc_str == "cc"
            || cc_str.ends_with("/cc")
            || cc_str.ends_with("\\cc")
            || cc_str.ends_with("-cc");

        if is_standalone_cc {
            // Replace the final "cc" with "c++"
            return PathBuf::from(format!("{}++", &cc_str[..cc_str.len() - 1]));
        }

        // Fallback: append ++ (handles edge cases like "tcc" -> "tcc++")
        PathBuf::from(format!("{}++", cc_str))
    }
}

impl GccToolchain {
    /// Set the target this toolchain builds for.
    pub fn with_target(mut self, target: TargetTriple) -> Self {
        self.target = target;
        self
    }
}

impl Toolchain for GccToolchain {
    fn platform(&self) -> ToolchainPlatform {
        self.family
    }

    fn compiler_path(&self) -> &Path {
        &self.cc
    }

    fn cxx_compiler_path(&self) -> &Path {
        &self.cxx
    }

    fn compile_command(
        &self,
        input: &CompileInput,
        lang: Language,
        cxx_opts: Option<&CxxOptions>,
    ) -> CommandSpec {
        // Select compiler based on language
        let compiler = match lang {
            // Assembly goes to the C driver, which dispatches to the
            // assembler by extension (`.S` preprocessed, `.s` not).
            Language::C | Language::Asm => &self.cc,
            Language::Cxx => &self.cxx,
        };

        let mut cmd = CommandSpec::new(compiler);

        // Compile only
        cmd = cmd.arg("-c");

        // C++ specific flags
        if lang == Language::Cxx {
            if let Some(opts) = cxx_opts {
                // C++ standard
                if let Some(std) = opts.std {
                    cmd = cmd.arg(format!("-std={}", std.as_flag_value()));
                }

                // Exceptions
                if !opts.exceptions {
                    cmd = cmd.arg("-fno-exceptions");
                }

                // RTTI
                if !opts.rtti {
                    cmd = cmd.arg("-fno-rtti");
                }

                // C++ runtime library (clang only, and only on non-Apple platforms)
                if self.family == ToolchainPlatform::Clang {
                    if let Some(runtime) = opts.runtime {
                        cmd = cmd.arg(runtime.as_flag());
                    }
                }
            }
        }

        // Include directories
        for dir in &input.include_dirs {
            cmd = cmd.arg(format!("-I{}", dir.display()));
        }

        // Defines
        for (name, value) in &input.defines {
            match value {
                Some(v) => cmd = cmd.arg(format!("-D{}={}", name, v)),
                None => cmd = cmd.arg(format!("-D{}", name)),
            }
        }

        // Custom flags
        cmd = cmd.args(input.cflags.iter().cloned());

        // Input and output
        cmd = cmd.arg(input.source.display().to_string());
        cmd = cmd.arg("-o");
        cmd = cmd.arg(input.output.display().to_string());

        cmd
    }

    fn archive_command(&self, input: &ArchiveInput) -> CommandSpec {
        let mut cmd = CommandSpec::new(&self.ar);

        // Create archive with symbol index, replace files
        cmd = cmd.arg("rcs");
        cmd = cmd.arg(input.output.display().to_string());

        // Object files
        for obj in &input.objects {
            cmd = cmd.arg(obj.display().to_string());
        }

        cmd
    }

    fn link_shared_command(
        &self,
        input: &LinkInput,
        driver: Language,
        cxx_opts: Option<&CxxOptions>,
    ) -> CommandSpec {
        // Select linker driver based on language
        let linker = match driver {
            // A pure-assembly target still links with the C driver.
            Language::C | Language::Asm => &self.cc,
            Language::Cxx => &self.cxx,
        };

        let mut cmd = CommandSpec::new(linker);

        // Shared library flag
        cmd = cmd.arg("-shared");

        // C++ runtime library (for linking)
        if driver == Language::Cxx {
            if let Some(opts) = cxx_opts {
                if self.family == ToolchainPlatform::Clang {
                    if let Some(runtime) = opts.runtime {
                        cmd = cmd.arg(runtime.as_flag());
                    }
                }
            }
        }

        // Output
        cmd = cmd.arg("-o");
        cmd = cmd.arg(input.output.display().to_string());

        // Object files
        for obj in &input.objects {
            cmd = cmd.arg(obj.display().to_string());
        }

        // Library search paths
        for dir in &input.lib_dirs {
            cmd = cmd.arg(format!("-L{}", dir.display()));
        }

        // Libraries
        for lib in &input.libs {
            cmd = cmd.arg(format!("-l{}", lib));
        }

        // macOS frameworks. Two separate argv entries: `-framework` takes
        // its name as the following argument, so a single "-framework Foo"
        // string would reach the driver as one unparsable arg.
        for framework in &input.frameworks {
            cmd = cmd.arg("-framework");
            cmd = cmd.arg(framework);
        }

        // Custom flags
        cmd = cmd.args(input.ldflags.iter().cloned());

        cmd
    }

    fn link_exe_command(
        &self,
        input: &LinkInput,
        driver: Language,
        cxx_opts: Option<&CxxOptions>,
    ) -> CommandSpec {
        // Select linker driver based on language
        let linker = match driver {
            // A pure-assembly target still links with the C driver.
            Language::C | Language::Asm => &self.cc,
            Language::Cxx => &self.cxx,
        };

        let mut cmd = CommandSpec::new(linker);

        // C++ runtime library (for linking)
        if driver == Language::Cxx {
            if let Some(opts) = cxx_opts {
                if self.family == ToolchainPlatform::Clang {
                    if let Some(runtime) = opts.runtime {
                        cmd = cmd.arg(runtime.as_flag());
                    }
                }
            }
        }

        // Output
        cmd = cmd.arg("-o");
        cmd = cmd.arg(input.output.display().to_string());

        // Object files
        for obj in &input.objects {
            cmd = cmd.arg(obj.display().to_string());
        }

        // Library search paths
        for dir in &input.lib_dirs {
            cmd = cmd.arg(format!("-L{}", dir.display()));
        }

        // Libraries
        for lib in &input.libs {
            cmd = cmd.arg(format!("-l{}", lib));
        }

        // macOS frameworks. Two separate argv entries: `-framework` takes
        // its name as the following argument, so a single "-framework Foo"
        // string would reach the driver as one unparsable arg.
        for framework in &input.frameworks {
            cmd = cmd.arg("-framework");
            cmd = cmd.arg(framework);
        }

        // Custom flags
        cmd = cmd.args(input.ldflags.iter().cloned());

        cmd
    }

    fn object_extension(&self) -> &str {
        "o"
    }

    fn static_lib_extension(&self) -> &str {
        "a"
    }

    fn shared_lib_extension(&self) -> &str {
        // Derived from the target, not `cfg!(target_os)`. The previous
        // host-based version emitted `.so` when cross-building for macOS.
        self.target.shared_lib_extension()
    }

    fn exe_extension(&self) -> &str {
        self.target.exe_extension()
    }

    fn static_lib_prefix(&self) -> &str {
        "lib"
    }

    fn shared_lib_prefix(&self) -> &str {
        "lib"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::target::CppStandard;

    fn toolchain() -> GccToolchain {
        GccToolchain::new(
            PathBuf::from("clang"),
            PathBuf::from("clang++"),
            PathBuf::from("ar"),
            ToolchainPlatform::Clang,
        )
    }

    /// Assembly must go to the C driver -- which dispatches to the
    /// assembler by extension -- and must not receive C++ standard flags,
    /// while include dirs still apply because `.S` runs through the
    /// preprocessor.
    #[test]
    fn assembly_compiles_with_the_c_driver() {
        let tc = toolchain();
        let input = CompileInput {
            source: PathBuf::from("aesv8-armx.S"),
            output: PathBuf::from("aesv8-armx.o"),
            include_dirs: vec![PathBuf::from("include")],
            defines: vec![],
            cflags: vec![],
        };
        let opts = CxxOptions {
            std: Some(CppStandard::Cpp17),
            ..Default::default()
        };

        let spec = tc.compile_command(&input, Language::Asm, Some(&opts));

        assert_eq!(spec.program, PathBuf::from("clang"));
        assert!(
            !spec.args.iter().any(|a| a.starts_with("-std=")),
            "assembly must not get a C++ standard flag: {:?}",
            spec.args
        );
        assert!(
            spec.args.iter().any(|a| a == "-Iinclude"),
            "include dirs still apply to preprocessed assembly: {:?}",
            spec.args
        );
    }

    /// A freestanding image's flags travel as ordinary `cflags`/`ldflags`
    /// (see `Target::freestanding_cflags` / `link_control_flags`), so what
    /// has to be pinned is that both lists reach the *exact* argv the driver
    /// sees. `frameworks` was declared, propagated and reported for months
    /// while `LinkStep`/`LinkInput` had no field for it; asserting the whole
    /// argv is the only check that would have caught that.
    #[test]
    fn a_freestanding_image_gets_its_flags_on_the_compile_and_link_argv() {
        let tc = toolchain();

        let compile = tc.compile_command(
            &CompileInput {
                source: PathBuf::from("/pkg/src/start.S"),
                output: PathBuf::from("/out/start.o"),
                include_dirs: vec![],
                defines: vec![],
                cflags: vec!["-ffreestanding".to_string()],
            },
            Language::Asm,
            None,
        );
        assert_eq!(
            compile.args,
            vec![
                "-c",
                "-ffreestanding",
                "/pkg/src/start.S",
                "-o",
                "/out/start.o",
            ]
        );

        let link = tc.link_exe_command(
            &LinkInput {
                objects: vec![PathBuf::from("/out/start.o")],
                output: PathBuf::from("/out/payload.elf"),
                lib_dirs: vec![],
                libs: vec!["gcc".to_string()],
                // Sorted, as the effective link surface delivers them.
                ldflags: vec![
                    "-Wl,--entry=_start".to_string(),
                    "-Wl,-T,/pkg/boot/layout.ld".to_string(),
                    "-nostdlib".to_string(),
                ],
                frameworks: vec![],
            },
            Language::C,
            None,
        );
        assert_eq!(
            link.args,
            vec![
                "-o",
                "/out/payload.elf",
                "/out/start.o",
                "-lgcc",
                "-Wl,--entry=_start",
                "-Wl,-T,/pkg/boot/layout.ld",
                "-nostdlib",
            ],
            "-nostdlib, -T and --entry must all reach the linker driver"
        );
    }

    fn input_with_frameworks() -> LinkInput {
        LinkInput {
            objects: vec![PathBuf::from("main.o")],
            output: PathBuf::from("app"),
            lib_dirs: vec![],
            libs: vec![],
            ldflags: vec![],
            frameworks: vec![
                "SystemConfiguration".to_string(),
                "CoreFoundation".to_string(),
            ],
        }
    }

    /// `frameworks` was parsed, resolved, deduped and reported by `harbour
    /// flags`, but `LinkStep`/`LinkInput` had no field for it, so it never
    /// reached the linker: a static libcurl failed with undefined
    /// `_CFRelease`/`_SCDynamicStoreCopyProxies` while `harbour flags`
    /// cheerfully listed the frameworks that would have supplied them.
    #[test]
    fn link_commands_pass_frameworks_to_the_driver() {
        let tc = toolchain();
        let input = input_with_frameworks();

        for spec in [
            tc.link_exe_command(&input, Language::C, None),
            tc.link_shared_command(&input, Language::C, None),
        ] {
            let args = &spec.args;
            let at = args
                .iter()
                .position(|a| a == "SystemConfiguration")
                .expect("framework name must be passed");
            assert_eq!(
                args[at - 1],
                "-framework",
                "each framework name must be preceded by its own `-framework` \
                 argument, not folded into one string"
            );
            assert!(args.iter().any(|a| a == "CoreFoundation"));
        }
    }
}
