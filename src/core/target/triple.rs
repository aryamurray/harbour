//! Target triple parsing.
//!
//! A target triple is conventionally `ARCH-VENDOR-OS-ENV`, but any middle
//! field may be absent and real-world triples are irregular: `avr` has one
//! component, `arm-none-eabi` has three with no OS at all, and
//! `x86_64-pc-windows-msvc` has four.
//!
//! Parsing therefore **recognizes components by set membership** rather than
//! by position, following `llvm::Triple`. Component 0 is always the
//! architecture; each remaining component is assigned to the first not-yet
//! filled slot among vendor -> os -> env whose known-value set contains it.
//!
//! Parsing is infallible. An unrecognized architecture, an unknown component,
//! or a malformed string still yields a usable `TargetTriple`, because the
//! toolchain lookup may well succeed anyway. Callers that want to warn can
//! check [`TargetTriple::fully_recognized`].

use std::fmt;

/// Known architecture values.
///
/// Sub-architecture suffixes (`thumbv7em`, `riscv32imac`, `thumbv8m.base`) are
/// part of the architecture token, not a separate field, so they are
/// enumerated rather than matched by family prefix.
const ARCHES: &[&str] = &[
    // x86
    "i386",
    "i486",
    "i586",
    "i686",
    "x86",
    "x86_64",
    "x86_64h",
    "amd64",
    // ARM A32
    "arm",
    "armeb",
    "armv4t",
    "armv5te",
    "armv6",
    "armv6k",
    "armv7",
    "armv7a",
    "armv7r",
    "armv7s",
    "armebv7r",
    "armv8",
    "arm64",
    "arm64e",
    "arm64_32",
    // ARM Thumb / T32
    "thumb",
    "thumbeb",
    "thumbv6m",
    "thumbv7m",
    "thumbv7em",
    "thumbv7neon",
    "thumbv8m.base",
    "thumbv8m.main",
    // AArch64
    "aarch64",
    "aarch64_be",
    "aarch64_32",
    // RISC-V
    "riscv32",
    "riscv32i",
    "riscv32imc",
    "riscv32imac",
    "riscv32imafc",
    "riscv32gc",
    "riscv64",
    "riscv64gc",
    "riscv64imac",
    // Embedded
    "xtensa",
    "avr",
    "msp430",
    // WebAssembly
    "wasm32",
    "wasm64",
    // MIPS
    "mips",
    "mipsel",
    "mips64",
    "mips64el",
    "mipsisa32r6",
    "mipsisa64r6",
    // PowerPC
    "powerpc",
    "powerpc64",
    "powerpc64le",
    "ppc",
    "ppc64",
    "ppc64le",
    // SPARC
    "sparc",
    "sparc64",
    "sparcv9",
    "sparcel",
    // SuperH and others
    "sh",
    "sh2",
    "sh4",
    "sh4eb",
    "m68k",
    "nios2",
    "or1k",
    "or1knd",
    "hexagon",
    "systemz",
    "s390x",
    "loongarch32",
    "loongarch64",
    "csky",
    "bpf",
    "bpfel",
    "bpfeb",
    "nvptx",
    "nvptx64",
    "ve",
    "lanai",
];

/// Known vendor values.
///
/// `unknown` is an explicit placeholder, not an absence. Espressif overloads
/// this slot with a chip family name (`esp32`, `esp32s3`) rather than a
/// company.
const VENDORS: &[&str] = &[
    "unknown",
    "pc",
    "apple",
    "none",
    "esp",
    "esp32",
    "esp32s2",
    "esp32s3",
    "nvidia",
    "ibm",
    "wrs",
    "suse",
    "sun",
    "sony",
    "scei",
    "uwp",
    "amd",
    "mti",
    "img",
    "csr",
    "myriad",
    "freescale",
    "fsl",
    "oe",
    "w64",
];

/// Known operating system values.
///
/// `freertos` is deliberately absent: FreeRTOS has no OS ABI of its own and
/// never appears as a triple component. A FreeRTOS application builds with an
/// ordinary bare-metal triple and links the kernel as a library.
///
/// `elf` is also absent -- see [`ENVS`] and the parser's override rule.
const OSES: &[&str] = &[
    "unknown",
    "linux",
    "darwin",
    "macos",
    "ios",
    "tvos",
    "watchos",
    "visionos",
    "windows",
    "freebsd",
    "netbsd",
    "openbsd",
    "dragonfly",
    "solaris",
    "illumos",
    "fuchsia",
    "hermit",
    "redox",
    "haiku",
    "hurd",
    "aix",
    "wasi",
    "emscripten",
    "cuda",
    "uefi",
    "none",
    // RTOS: a real OS, but not hosted -- see `is_embedded_rtos`.
    "rtems",
    "zephyr",
    "nuttx",
    "vxworks",
];

/// Known environment / ABI values.
///
/// `nano` is deliberately absent: newlib-nano is selected at link time via
/// `--specs=nano.specs` and is never a triple component.
const ENVS: &[&str] = &[
    "gnu",
    "gnueabi",
    "gnueabihf",
    "gnuabi64",
    "gnux32",
    "gnuilp32",
    "musl",
    "musleabi",
    "musleabihf",
    "msvc",
    "itanium",
    "eabi",
    "eabihf",
    "elf",
    "newlib",
    "uclibc",
    "uclibcgnueabi",
    "uclibcgnueabihf",
    "android",
    "androideabi",
    "coreclr",
    "sim",
    "macabi",
    "abi64",
    "mingw32",
];

/// Operating systems that are a real OS but still require a cross toolchain
/// and cannot use the host libc.
const RTOS_OSES: &[&str] = &["rtems", "zephyr", "nuttx", "vxworks"];

/// A parsed target triple.
///
/// The originating string is always preserved verbatim, so an unrecognized
/// triple round-trips losslessly through [`TargetTriple::as_str`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TargetTriple {
    raw: String,
    arch: String,
    vendor: Option<String>,
    os: Option<String>,
    env: Option<String>,
    extra: Vec<String>,
    fully_recognized: bool,
}

impl TargetTriple {
    /// Parse a triple. Never fails.
    pub fn parse(s: &str) -> Self {
        let raw = s.to_string();
        // Empty tokens (from a trailing or doubled hyphen) mean "field
        // absent", so they are dropped rather than filling a slot.
        let parts: Vec<&str> = s.split('-').filter(|p| !p.is_empty()).collect();

        if parts.is_empty() {
            return TargetTriple {
                raw,
                arch: String::new(),
                vendor: None,
                os: None,
                env: None,
                extra: Vec::new(),
                fully_recognized: false,
            };
        }

        // Component 0 is always the architecture, recognized or not.
        let arch = parts[0].to_string();
        let mut fully_recognized = ARCHES.contains(&parts[0]);

        let mut vendor: Option<String> = None;
        let mut os: Option<String> = None;
        let mut env: Option<String> = None;
        let mut extra: Vec<String> = Vec::new();

        for part in &parts[1..] {
            // `elf` describes an object format, never an operating system.
            // Binding it to `env` even when the os slot is open is what makes
            // `msp430-elf` and `xtensa-esp32-elf` classify as bare metal
            // instead of inventing an OS called "elf".
            if *part == "elf" {
                if env.is_none() {
                    env = Some((*part).to_string());
                } else {
                    extra.push((*part).to_string());
                }
                continue;
            }

            if vendor.is_none() && VENDORS.contains(part) {
                vendor = Some((*part).to_string());
            } else if os.is_none() && OSES.contains(part) {
                os = Some((*part).to_string());
            } else if env.is_none() && ENVS.contains(part) {
                env = Some((*part).to_string());
            } else {
                // Unrecognized: still fill the next open slot so nothing is
                // silently dropped, then overflow into `extra`.
                fully_recognized = false;
                if vendor.is_none() {
                    vendor = Some((*part).to_string());
                } else if os.is_none() {
                    os = Some((*part).to_string());
                } else if env.is_none() {
                    env = Some((*part).to_string());
                } else {
                    extra.push((*part).to_string());
                }
            }
        }

        TargetTriple {
            raw,
            arch,
            vendor,
            os,
            env,
            extra,
            fully_recognized,
        }
    }

    /// The host triple, derived from the compiling Rust target.
    ///
    /// This is the single source of truth for "the machine we are running on".
    pub fn host() -> Self {
        let arch = std::env::consts::ARCH;
        let os = std::env::consts::OS;
        let raw = match os {
            "linux" => format!("{arch}-unknown-linux-gnu"),
            "macos" => format!("{arch}-apple-darwin"),
            "windows" => format!("{arch}-pc-windows-msvc"),
            "freebsd" | "netbsd" | "openbsd" | "dragonfly" => {
                format!("{arch}-unknown-{os}")
            }
            other => format!("{arch}-unknown-{other}"),
        };
        Self::parse(&raw)
    }

    pub fn arch(&self) -> &str {
        &self.arch
    }

    pub fn vendor(&self) -> Option<&str> {
        self.vendor.as_deref()
    }

    pub fn os(&self) -> Option<&str> {
        self.os.as_deref()
    }

    pub fn env(&self) -> Option<&str> {
        self.env.as_deref()
    }

    /// Components that did not fit any slot.
    pub fn extra(&self) -> &[String] {
        &self.extra
    }

    /// False if any component was not recognized. Drives a diagnostic, never
    /// an error.
    pub fn fully_recognized(&self) -> bool {
        self.fully_recognized
    }

    /// The triple exactly as written. Lossless.
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// The normalized four-component form, for use as a cache key.
    ///
    /// Collapses spellings that denote the same target, so `arm-none-eabi` and
    /// `arm-unknown-none-eabi` canonicalize identically.
    pub fn canonical(&self) -> String {
        // A `none` vendor and an absent vendor both mean "no vendor".
        let vendor = match self.vendor.as_deref() {
            Some("none") | None => "unknown",
            Some(v) => v,
        };
        let os = self.os.as_deref().unwrap_or("none");
        match self.env.as_deref() {
            Some(env) => format!("{}-{}-{}-{}", self.arch, vendor, os, env),
            None => format!("{}-{}-{}", self.arch, vendor, os),
        }
    }

    /// Whether this target runs without a general-purpose OS providing a
    /// syscall ABI.
    ///
    /// Note that `"unknown"` is not `"none"`: `wasm32-unknown-unknown` is not
    /// bare metal.
    pub fn is_bare_metal(&self) -> bool {
        matches!(self.os.as_deref(), None | Some("none"))
    }

    /// Whether this target is hosted on an embedded RTOS.
    ///
    /// Distinct from [`is_bare_metal`](Self::is_bare_metal): an RTOS target has
    /// a real OS and is not freestanding, but still needs a cross toolchain and
    /// cannot use the host libc. Overloading one flag for both loses that.
    pub fn is_embedded_rtos(&self) -> bool {
        self.os.as_deref().is_some_and(|os| RTOS_OSES.contains(&os))
    }

    pub fn is_windows(&self) -> bool {
        self.os.as_deref() == Some("windows")
    }

    /// True for macOS, iOS, tvOS, watchOS and visionOS.
    pub fn is_apple(&self) -> bool {
        matches!(
            self.os.as_deref(),
            Some("darwin")
                | Some("macos")
                | Some("ios")
                | Some("tvos")
                | Some("watchos")
                | Some("visionos")
        )
    }

    pub fn is_msvc(&self) -> bool {
        self.env.as_deref() == Some("msvc")
    }

    pub fn env_is(&self, env: &str) -> bool {
        self.env.as_deref() == Some(env)
    }

    /// Whether this triple denotes the machine we are running on.
    ///
    /// Compares canonical forms, so it is not fooled by spelling differences,
    /// and unlike the previous implementations it does not substring-match or
    /// consult the host `cfg!`.
    pub fn is_host(&self) -> bool {
        self.canonical() == Self::host().canonical()
    }

    /// The shared library extension for this *target*.
    ///
    /// Deliberately a property of the triple rather than of the host, so that
    /// cross-building for macOS from Linux still yields `dylib`.
    pub fn shared_lib_extension(&self) -> &'static str {
        if self.is_windows() {
            "dll"
        } else if self.is_apple() {
            "dylib"
        } else {
            "so"
        }
    }

    /// The executable extension for this *target*.
    pub fn exe_extension(&self) -> &'static str {
        if self.is_windows() {
            "exe"
        } else {
            ""
        }
    }
}

impl fmt::Display for TargetTriple {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

impl From<&str> for TargetTriple {
    fn from(s: &str) -> Self {
        Self::parse(s)
    }
}

impl From<String> for TargetTriple {
    fn from(s: String) -> Self {
        Self::parse(&s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `raw, arch, vendor, os, env, is_bare_metal`
    ///
    /// Corpus of real-world triples. A wrong row here is worse than a missing
    /// one, so entries are drawn from LLVM/Rust/GCC conventions rather than
    /// invented.
    const CORPUS: &[(&str, &str, Option<&str>, Option<&str>, Option<&str>, bool)] = &[
        // Linux, glibc and musl
        (
            "x86_64-unknown-linux-gnu",
            "x86_64",
            Some("unknown"),
            Some("linux"),
            Some("gnu"),
            false,
        ),
        (
            "x86_64-unknown-linux-musl",
            "x86_64",
            Some("unknown"),
            Some("linux"),
            Some("musl"),
            false,
        ),
        (
            "aarch64-unknown-linux-gnu",
            "aarch64",
            Some("unknown"),
            Some("linux"),
            Some("gnu"),
            false,
        ),
        (
            "aarch64-unknown-linux-musl",
            "aarch64",
            Some("unknown"),
            Some("linux"),
            Some("musl"),
            false,
        ),
        (
            "i686-unknown-linux-gnu",
            "i686",
            Some("unknown"),
            Some("linux"),
            Some("gnu"),
            false,
        ),
        (
            "armv7-unknown-linux-gnueabihf",
            "armv7",
            Some("unknown"),
            Some("linux"),
            Some("gnueabihf"),
            false,
        ),
        (
            "armv7-unknown-linux-musleabihf",
            "armv7",
            Some("unknown"),
            Some("linux"),
            Some("musleabihf"),
            false,
        ),
        (
            "arm-unknown-linux-gnueabi",
            "arm",
            Some("unknown"),
            Some("linux"),
            Some("gnueabi"),
            false,
        ),
        (
            "mips-unknown-linux-gnu",
            "mips",
            Some("unknown"),
            Some("linux"),
            Some("gnu"),
            false,
        ),
        (
            "powerpc64le-unknown-linux-gnu",
            "powerpc64le",
            Some("unknown"),
            Some("linux"),
            Some("gnu"),
            false,
        ),
        (
            "sparc64-unknown-linux-gnu",
            "sparc64",
            Some("unknown"),
            Some("linux"),
            Some("gnu"),
            false,
        ),
        (
            "riscv64gc-unknown-linux-gnu",
            "riscv64gc",
            Some("unknown"),
            Some("linux"),
            Some("gnu"),
            false,
        ),
        (
            "riscv64gc-unknown-linux-musl",
            "riscv64gc",
            Some("unknown"),
            Some("linux"),
            Some("musl"),
            false,
        ),
        // BSDs
        (
            "x86_64-unknown-freebsd",
            "x86_64",
            Some("unknown"),
            Some("freebsd"),
            None,
            false,
        ),
        (
            "x86_64-unknown-netbsd",
            "x86_64",
            Some("unknown"),
            Some("netbsd"),
            None,
            false,
        ),
        (
            "x86_64-unknown-openbsd",
            "x86_64",
            Some("unknown"),
            Some("openbsd"),
            None,
            false,
        ),
        // Apple
        (
            "x86_64-apple-darwin",
            "x86_64",
            Some("apple"),
            Some("darwin"),
            None,
            false,
        ),
        (
            "aarch64-apple-darwin",
            "aarch64",
            Some("apple"),
            Some("darwin"),
            None,
            false,
        ),
        (
            "aarch64-apple-ios",
            "aarch64",
            Some("apple"),
            Some("ios"),
            None,
            false,
        ),
        (
            "aarch64-apple-ios-sim",
            "aarch64",
            Some("apple"),
            Some("ios"),
            Some("sim"),
            false,
        ),
        (
            "x86_64-apple-ios",
            "x86_64",
            Some("apple"),
            Some("ios"),
            None,
            false,
        ),
        // Windows
        (
            "x86_64-pc-windows-msvc",
            "x86_64",
            Some("pc"),
            Some("windows"),
            Some("msvc"),
            false,
        ),
        (
            "i686-pc-windows-msvc",
            "i686",
            Some("pc"),
            Some("windows"),
            Some("msvc"),
            false,
        ),
        (
            "aarch64-pc-windows-msvc",
            "aarch64",
            Some("pc"),
            Some("windows"),
            Some("msvc"),
            false,
        ),
        (
            "x86_64-pc-windows-gnu",
            "x86_64",
            Some("pc"),
            Some("windows"),
            Some("gnu"),
            false,
        ),
        // Android: `linux` is the OS, `android` the environment; no vendor.
        (
            "aarch64-linux-android",
            "aarch64",
            None,
            Some("linux"),
            Some("android"),
            false,
        ),
        (
            "armv7-linux-androideabi",
            "armv7",
            None,
            Some("linux"),
            Some("androideabi"),
            false,
        ),
        (
            "x86_64-linux-android",
            "x86_64",
            None,
            Some("linux"),
            Some("android"),
            false,
        ),
        // WebAssembly. Note `unknown` os is NOT bare metal.
        (
            "wasm32-unknown-unknown",
            "wasm32",
            Some("unknown"),
            Some("unknown"),
            None,
            false,
        ),
        ("wasm32-wasi", "wasm32", None, Some("wasi"), None, false),
        (
            "wasm32-unknown-emscripten",
            "wasm32",
            Some("unknown"),
            Some("emscripten"),
            None,
            false,
        ),
        // ARM bare metal: `none` fills the vendor slot, os is absent.
        (
            "arm-none-eabi",
            "arm",
            Some("none"),
            None,
            Some("eabi"),
            true,
        ),
        (
            "arm-none-eabihf",
            "arm",
            Some("none"),
            None,
            Some("eabihf"),
            true,
        ),
        (
            "thumbv6m-none-eabi",
            "thumbv6m",
            Some("none"),
            None,
            Some("eabi"),
            true,
        ),
        (
            "thumbv7m-none-eabi",
            "thumbv7m",
            Some("none"),
            None,
            Some("eabi"),
            true,
        ),
        (
            "thumbv7em-none-eabi",
            "thumbv7em",
            Some("none"),
            None,
            Some("eabi"),
            true,
        ),
        (
            "thumbv7em-none-eabihf",
            "thumbv7em",
            Some("none"),
            None,
            Some("eabihf"),
            true,
        ),
        (
            "thumbv8m.base-none-eabi",
            "thumbv8m.base",
            Some("none"),
            None,
            Some("eabi"),
            true,
        ),
        (
            "thumbv8m.main-none-eabihf",
            "thumbv8m.main",
            Some("none"),
            None,
            Some("eabihf"),
            true,
        ),
        (
            "armv7r-none-eabi",
            "armv7r",
            Some("none"),
            None,
            Some("eabi"),
            true,
        ),
        (
            "armebv7r-none-eabihf",
            "armebv7r",
            Some("none"),
            None,
            Some("eabihf"),
            true,
        ),
        // RISC-V bare metal: `none` explicitly fills the os slot here.
        (
            "riscv32imac-unknown-none-elf",
            "riscv32imac",
            Some("unknown"),
            Some("none"),
            Some("elf"),
            true,
        ),
        (
            "riscv32imc-unknown-none-elf",
            "riscv32imc",
            Some("unknown"),
            Some("none"),
            Some("elf"),
            true,
        ),
        (
            "riscv64gc-unknown-none-elf",
            "riscv64gc",
            Some("unknown"),
            Some("none"),
            Some("elf"),
            true,
        ),
        // Espressif: chip name in the vendor slot, `elf` bound to env.
        (
            "xtensa-esp32-elf",
            "xtensa",
            Some("esp32"),
            None,
            Some("elf"),
            true,
        ),
        (
            "xtensa-esp32s3-elf",
            "xtensa",
            Some("esp32s3"),
            None,
            Some("elf"),
            true,
        ),
        (
            "xtensa-esp32s2-elf",
            "xtensa",
            Some("esp32s2"),
            None,
            Some("elf"),
            true,
        ),
        (
            "riscv32-esp-elf",
            "riscv32",
            Some("esp"),
            None,
            Some("elf"),
            true,
        ),
        // Minimal and GCC-style bare metal.
        ("avr", "avr", None, None, None, true),
        ("msp430-elf", "msp430", None, None, Some("elf"), true),
    ];

    #[test]
    fn corpus_parses_as_expected() {
        for (raw, arch, vendor, os, env, bare) in CORPUS {
            let t = TargetTriple::parse(raw);
            assert_eq!(t.arch(), *arch, "arch mismatch for {raw}");
            assert_eq!(t.vendor(), *vendor, "vendor mismatch for {raw}");
            assert_eq!(t.os(), *os, "os mismatch for {raw}");
            assert_eq!(t.env(), *env, "env mismatch for {raw}");
            assert_eq!(t.is_bare_metal(), *bare, "is_bare_metal mismatch for {raw}");
        }
    }

    #[test]
    fn corpus_round_trips_losslessly() {
        for (raw, ..) in CORPUS {
            assert_eq!(TargetTriple::parse(raw).as_str(), *raw);
        }
    }

    #[test]
    fn corpus_is_fully_recognized() {
        for (raw, ..) in CORPUS {
            let t = TargetTriple::parse(raw);
            assert!(t.fully_recognized(), "{raw} had an unrecognized component");
            assert!(
                t.extra().is_empty(),
                "{raw} produced leftovers: {:?}",
                t.extra()
            );
        }
    }

    // --- The `elf` override, which is the load-bearing disambiguation rule ---

    #[test]
    fn elf_binds_to_env_never_to_os() {
        // Without the override these would parse as an OS named "elf" and be
        // misclassified as hosted targets.
        for raw in ["msp430-elf", "xtensa-esp32-elf", "riscv32-esp-elf"] {
            let t = TargetTriple::parse(raw);
            assert_eq!(t.env(), Some("elf"), "{raw}");
            assert_eq!(t.os(), None, "{raw}");
            assert!(t.is_bare_metal(), "{raw}");
        }
    }

    #[test]
    fn unknown_os_is_not_bare_metal() {
        // The easy bug: treating "unknown" and "none" as synonyms.
        assert!(!TargetTriple::parse("wasm32-unknown-unknown").is_bare_metal());
        assert!(TargetTriple::parse("riscv32imac-unknown-none-elf").is_bare_metal());
    }

    #[test]
    fn rtos_targets_are_not_bare_metal_but_are_flagged() {
        let t = TargetTriple::parse("arm-unknown-zephyr-eabi");
        assert!(!t.is_bare_metal());
        assert!(t.is_embedded_rtos());

        let linux = TargetTriple::parse("x86_64-unknown-linux-gnu");
        assert!(!linux.is_embedded_rtos());

        let bare = TargetTriple::parse("arm-none-eabi");
        assert!(bare.is_bare_metal());
        assert!(!bare.is_embedded_rtos());
    }

    // --- Canonicalization ---

    #[test]
    fn equivalent_spellings_canonicalize_identically() {
        assert_eq!(
            TargetTriple::parse("arm-none-eabi").canonical(),
            TargetTriple::parse("arm-unknown-none-eabi").canonical(),
        );
    }

    #[test]
    fn distinct_targets_canonicalize_differently() {
        let pairs = [
            ("thumbv7em-none-eabi", "thumbv7em-none-eabihf"),
            ("x86_64-unknown-linux-gnu", "x86_64-unknown-linux-musl"),
            ("x86_64-apple-darwin", "aarch64-apple-darwin"),
        ];
        for (a, b) in pairs {
            assert_ne!(
                TargetTriple::parse(a).canonical(),
                TargetTriple::parse(b).canonical(),
                "{a} and {b} collided"
            );
        }
    }

    #[test]
    fn canonical_form_is_stable_under_reparsing() {
        for (raw, ..) in CORPUS {
            let once = TargetTriple::parse(raw).canonical();
            let twice = TargetTriple::parse(&once).canonical();
            assert_eq!(once, twice, "canonical form not idempotent for {raw}");
        }
    }

    // --- Target-derived properties that used to read the host ---

    #[test]
    fn shared_lib_extension_follows_the_target_not_the_host() {
        assert_eq!(
            TargetTriple::parse("x86_64-apple-darwin").shared_lib_extension(),
            "dylib"
        );
        assert_eq!(
            TargetTriple::parse("x86_64-unknown-linux-gnu").shared_lib_extension(),
            "so"
        );
        assert_eq!(
            TargetTriple::parse("x86_64-pc-windows-msvc").shared_lib_extension(),
            "dll"
        );
        assert_eq!(
            TargetTriple::parse("thumbv7em-none-eabi").shared_lib_extension(),
            "so"
        );
    }

    #[test]
    fn exe_extension_follows_the_target() {
        assert_eq!(
            TargetTriple::parse("x86_64-pc-windows-gnu").exe_extension(),
            "exe"
        );
        assert_eq!(
            TargetTriple::parse("x86_64-unknown-linux-gnu").exe_extension(),
            ""
        );
    }

    // --- Malformed and degenerate input is tolerated, never rejected ---

    #[test]
    fn trailing_hyphen_is_ignored() {
        let t = TargetTriple::parse("x86_64-unknown-linux-gnu-");
        assert_eq!(t.arch(), "x86_64");
        assert_eq!(t.os(), Some("linux"));
        assert_eq!(t.env(), Some("gnu"));
        // ...but the raw form is still preserved exactly.
        assert_eq!(t.as_str(), "x86_64-unknown-linux-gnu-");
    }

    #[test]
    fn doubled_hyphen_means_absent_vendor() {
        let t = TargetTriple::parse("x86_64--linux-gnu");
        assert_eq!(t.arch(), "x86_64");
        assert_eq!(t.vendor(), None);
        assert_eq!(t.os(), Some("linux"));
        assert_eq!(t.env(), Some("gnu"));
    }

    #[test]
    fn unrecognized_arch_is_preserved_not_rejected() {
        let t = TargetTriple::parse("loongarch128-unknown-linux-gnu");
        assert_eq!(t.arch(), "loongarch128");
        assert_eq!(t.vendor(), Some("unknown"));
        assert_eq!(t.os(), Some("linux"));
        assert_eq!(t.env(), Some("gnu"));
        assert!(!t.fully_recognized());
    }

    #[test]
    fn overflow_components_land_in_extra() {
        let t = TargetTriple::parse("x86_64-pc-linux-gnu-eabi");
        assert_eq!(t.arch(), "x86_64");
        assert_eq!(t.vendor(), Some("pc"));
        assert_eq!(t.os(), Some("linux"));
        assert_eq!(t.env(), Some("gnu"));
        assert_eq!(t.extra(), ["eabi"]);
    }

    #[test]
    fn empty_input_does_not_panic() {
        let t = TargetTriple::parse("");
        assert_eq!(t.arch(), "");
        assert!(!t.fully_recognized());
        assert_eq!(t.as_str(), "");
    }

    #[test]
    fn single_unrecognized_component_is_tolerated() {
        let t = TargetTriple::parse("notarealarch");
        assert_eq!(t.arch(), "notarealarch");
        assert!(!t.fully_recognized());
        // No OS was named, so it reads as freestanding. Worth knowing: an
        // unknown single-component triple is treated as bare metal.
        assert!(t.is_bare_metal());
    }

    // --- Host ---

    #[test]
    fn host_is_self_consistent() {
        let h = TargetTriple::host();
        assert!(!h.arch().is_empty());
        assert!(h.is_host());
        assert!(!h.is_bare_metal(), "the host is never bare metal");
    }

    #[test]
    fn is_host_compares_canonically_not_by_substring() {
        // The old implementation substring-matched, so a cross triple that
        // merely contained the host OS name was misreported as host.
        let cross = TargetTriple::parse("thumbv7em-none-eabi");
        assert!(!cross.is_host());
    }
}
