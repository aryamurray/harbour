//! MSVC toolchain implementation.

use std::path::{Path, PathBuf};

use crate::core::target::Language;

use super::{ArchiveInput, CommandSpec, CompileInput, CxxOptions, LinkInput, Toolchain, ToolchainPlatform};

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
