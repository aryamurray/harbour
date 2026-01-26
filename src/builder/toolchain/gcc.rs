//! GCC/Clang toolchain implementation.

use std::path::{Path, PathBuf};

use crate::core::target::Language;

use super::{ArchiveInput, CommandSpec, CompileInput, CxxOptions, LinkInput, Toolchain, ToolchainPlatform};

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
}

impl GccToolchain {
    /// Create a new GCC-style toolchain.
    pub fn new(cc: PathBuf, cxx: PathBuf, ar: PathBuf, family: ToolchainPlatform) -> Self {
        GccToolchain {
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
            Language::C => &self.cc,
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
            Language::C => &self.cc,
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
            Language::C => &self.cc,
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
        if cfg!(target_os = "macos") {
            "dylib"
        } else {
            "so"
        }
    }

    fn exe_extension(&self) -> &str {
        ""
    }

    fn static_lib_prefix(&self) -> &str {
        "lib"
    }

    fn shared_lib_prefix(&self) -> &str {
        "lib"
    }
}
