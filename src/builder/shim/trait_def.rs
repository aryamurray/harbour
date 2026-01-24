//! BackendShim trait definition and result types.
//!
//! The BackendShim trait defines the interface for build backends.
//! Operations only - validation is done externally via capabilities.

use std::path::PathBuf;

use anyhow::Result;

use crate::builder::shim::capabilities::BackendCapabilities;
use crate::builder::shim::defaults::BackendDefaults;
use crate::builder::shim::intent::{BackendOptions, BuildIntent};
use crate::builder::shim::validation::ValidationError;
use crate::core::surface::Surface;

/// Backend availability status.
#[derive(Debug, Clone)]
pub enum BackendAvailability {
    /// Backend tool is available and ready
    Available {
        /// Detected version of the backend tool
        version: semver::Version,
    },

    /// Backend tool is not installed
    NotInstalled {
        /// Name of the missing tool (e.g., "cmake")
        tool: String,
        /// Hint for how to install (e.g., "apt install cmake")
        install_hint: String,
    },

    /// Backend tool version is too old
    VersionTooOld {
        /// Found version
        found: semver::Version,
        /// Required version
        required: semver::VersionReq,
    },

    /// Backend is always available (no external tool needed)
    AlwaysAvailable,
}

impl BackendAvailability {
    /// Check if the backend is available.
    pub fn is_available(&self) -> bool {
        matches!(
            self,
            BackendAvailability::Available { .. } | BackendAvailability::AlwaysAvailable
        )
    }

    /// Get error message if not available.
    pub fn error_message(&self) -> Option<String> {
        match self {
            BackendAvailability::Available { .. } | BackendAvailability::AlwaysAvailable => None,
            BackendAvailability::NotInstalled { tool, install_hint } => {
                Some(format!("{} not found. {}", tool, install_hint))
            }
            BackendAvailability::VersionTooOld { found, required } => {
                Some(format!(
                    "version {} found, but {} required",
                    found, required
                ))
            }
        }
    }
}

/// Build context passed to shim operations.
#[derive(Debug, Clone)]
pub struct BuildContext {
    /// Package root directory
    pub package_root: PathBuf,

    /// Build output directory
    pub build_dir: PathBuf,

    /// Install prefix
    pub install_prefix: PathBuf,

    /// Release mode
    pub release: bool,

    /// Parallel job count
    pub jobs: Option<usize>,

    /// Verbose output
    pub verbose: bool,
}

impl BuildContext {
    /// Create a new build context.
    pub fn new(package_root: PathBuf, build_dir: PathBuf, install_prefix: PathBuf) -> Self {
        BuildContext {
            package_root,
            build_dir,
            install_prefix,
            release: false,
            jobs: None,
            verbose: false,
        }
    }

    /// Set release mode.
    pub fn with_release(mut self, release: bool) -> Self {
        self.release = release;
        self
    }

    /// Set job count.
    pub fn with_jobs(mut self, jobs: Option<usize>) -> Self {
        self.jobs = jobs;
        self
    }

    /// Set verbose mode.
    pub fn with_verbose(mut self, verbose: bool) -> Self {
        self.verbose = verbose;
        self
    }
}

/// Result of the configure phase.
#[derive(Debug, Clone, Default)]
pub struct ConfigureResult {
    /// Whether configuration was skipped (already configured)
    pub skipped: bool,

    /// Generator used (if applicable)
    pub generator: Option<String>,

    /// Warnings from configuration
    pub warnings: Vec<String>,
}

/// Result of the build phase.
#[derive(Debug, Clone, Default)]
pub struct BuildResult {
    /// Produced artifacts
    pub artifacts: Vec<Artifact>,

    /// Warnings from build
    pub warnings: Vec<String>,
}

/// A built artifact.
#[derive(Debug, Clone)]
pub struct Artifact {
    /// Path to the artifact
    pub path: PathBuf,

    /// Kind of artifact
    pub kind: ArtifactType,

    /// Target name that produced this
    pub target: String,
}

/// Type of artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactType {
    /// Static library
    StaticLib,
    /// Shared/dynamic library
    SharedLib,
    /// Executable
    Executable,
    /// Header files
    Headers,
}

/// Result of the test phase.
#[derive(Debug, Clone, Default)]
pub struct TestResult {
    /// Total number of tests
    pub total: usize,

    /// Number of passed tests
    pub passed: usize,

    /// Number of failed tests
    pub failed: usize,

    /// Number of skipped tests
    pub skipped: usize,

    /// Individual test results (if available)
    pub tests: Vec<TestCase>,
}

impl TestResult {
    /// Check if all tests passed.
    pub fn success(&self) -> bool {
        self.failed == 0
    }
}

/// A single test case result.
#[derive(Debug, Clone)]
pub struct TestCase {
    /// Test name
    pub name: String,

    /// Test status
    pub status: TestStatus,

    /// Output (if failed)
    pub output: Option<String>,
}

/// Test status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestStatus {
    Passed,
    Failed,
    Skipped,
}

/// Result of the install phase.
#[derive(Debug, Clone, Default)]
pub struct InstallResult {
    /// Installed files
    pub files: Vec<InstalledFile>,
}

/// An installed file.
#[derive(Debug, Clone)]
pub struct InstalledFile {
    /// Source path (in build dir)
    pub source: PathBuf,

    /// Destination path (in prefix)
    pub destination: PathBuf,

    /// File kind
    pub kind: InstalledFileKind,
}

/// Kind of installed file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstalledFileKind {
    /// Library (static or shared)
    Library,
    /// Header file
    Header,
    /// Executable
    Executable,
    /// CMake config file
    CMakeConfig,
    /// pkg-config file
    PkgConfig,
    /// Other
    Other,
}

/// Options for the clean operation.
#[derive(Debug, Clone, Default)]
pub struct CleanOptions {
    /// Remove build directory
    pub build_dir: bool,

    /// Remove install prefix
    pub install_prefix: bool,

    /// Dry run (don't actually delete)
    pub dry_run: bool,
}

/// Library kind for FFI and surface discovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LibraryKind {
    /// Static library
    Static,
    /// Shared/dynamic library
    Shared,
}

/// Library information with link mode and paths.
#[derive(Debug, Clone)]
pub struct LibraryInfo {
    /// Library name (without prefix/extension)
    pub name: String,

    /// Library kind
    pub kind: LibraryKind,

    /// Actual library file path
    pub path: PathBuf,

    /// soname (Linux: libfoo.so.1)
    pub soname: Option<String>,

    /// Import library path (Windows: foo.lib for foo.dll)
    pub import_lib: Option<PathBuf>,
}

/// Define for compile surface.
#[derive(Debug, Clone)]
pub struct Define {
    /// Define name
    pub name: String,

    /// Define value (None for -DFOO style)
    pub value: Option<String>,
}

impl Define {
    /// Create a simple define without value.
    pub fn simple(name: impl Into<String>) -> Self {
        Define {
            name: name.into(),
            value: None,
        }
    }

    /// Create a define with value.
    pub fn with_value(name: impl Into<String>, value: impl Into<String>) -> Self {
        Define {
            name: name.into(),
            value: Some(value.into()),
        }
    }
}

/// Discovered exports from a backend build.
#[derive(Debug, Clone, Default)]
pub struct DiscoveredSurface {
    /// Include directories
    pub include_dirs: Vec<PathBuf>,

    /// Libraries
    pub libraries: Vec<LibraryInfo>,

    /// Preprocessor defines
    pub defines: Vec<Define>,

    /// Compiler flags
    pub cflags: Vec<String>,

    /// Linker flags
    pub ldflags: Vec<String>,

    /// System libraries to link
    pub system_libs: Vec<String>,

    /// Runtime dependencies (for FFI bundle)
    pub runtime_deps: Vec<PathBuf>,
}

impl DiscoveredSurface {
    /// Convert to a Surface for consumption by dependents.
    pub fn to_surface(&self) -> Surface {
        use crate::core::surface::{CompileRequirements, CompileSurface, LinkRequirements, LinkSurface, LibRef, Define as SurfaceDefine};

        let mut compile_pub = CompileRequirements::default();
        for dir in &self.include_dirs {
            compile_pub.include_dirs.push(dir.clone());
        }
        for def in &self.defines {
            let surface_def = match &def.value {
                Some(v) => SurfaceDefine::key_value(&def.name, v),
                None => SurfaceDefine::flag(&def.name),
            };
            compile_pub.defines.push(surface_def);
        }
        compile_pub.cflags = self.cflags.clone();

        let mut link_pub = LinkRequirements::default();
        for lib in &self.libraries {
            link_pub.libs.push(LibRef::system(&lib.name));
        }
        for sys_lib in &self.system_libs {
            link_pub.libs.push(LibRef::system(sys_lib));
        }
        link_pub.ldflags = self.ldflags.clone();

        Surface {
            compile: CompileSurface {
                public: compile_pub,
                private: CompileRequirements::default(),
                requires_cpp: None,
            },
            link: LinkSurface {
                public: link_pub,
                private: LinkRequirements::default(),
            },
            abi: Default::default(),
            conditionals: Vec::new(),
        }
    }
}

/// Doctor/diagnostic report.
#[derive(Debug, Clone, Default)]
pub struct DoctorReport {
    /// Overall status
    pub ok: bool,

    /// Individual checks
    pub checks: Vec<DoctorCheck>,

    /// Suggestions for fixing issues
    pub suggestions: Vec<String>,
}

/// A single doctor check.
#[derive(Debug, Clone)]
pub struct DoctorCheck {
    /// Check name
    pub name: String,

    /// Check passed
    pub passed: bool,

    /// Message (details or error)
    pub message: String,
}

/// BackendShim trait - interface for build backends.
///
/// The shim trait is purely operational. Validation is done externally
/// via capabilities data returned by `capabilities()`.
pub trait BackendShim: Send + Sync {
    /// Get backend capabilities (data for validation).
    fn capabilities(&self) -> &BackendCapabilities;

    /// Get backend defaults (policy).
    fn defaults(&self) -> &BackendDefaults;

    /// Check if backend tool is available.
    ///
    /// This may run processes (e.g., `cmake --version`) and should be
    /// called lazily when the backend is actually needed.
    fn availability(&self) -> Result<BackendAvailability>;

    /// Configure the build (if phase supported).
    fn configure(&self, ctx: &BuildContext, opts: &BackendOptions) -> Result<ConfigureResult>;

    /// Execute the build.
    fn build(&self, ctx: &BuildContext, opts: &BackendOptions) -> Result<BuildResult>;

    /// Run tests (if phase supported).
    fn test(&self, ctx: &BuildContext, opts: &BackendOptions) -> Result<TestResult>;

    /// Install artifacts (if phase supported).
    fn install(&self, ctx: &BuildContext, opts: &BackendOptions) -> Result<InstallResult>;

    /// Clean build artifacts.
    fn clean(&self, ctx: &BuildContext, opts: &CleanOptions) -> Result<()>;

    /// Discover produced exports for Surface integration.
    ///
    /// Runs after install if `export_discovery.requires_install` is true.
    fn discover_exports(&self, ctx: &BuildContext) -> Result<Option<DiscoveredSurface>>;

    /// Run diagnostics/doctor checks.
    fn doctor(&self, ctx: &BuildContext) -> Result<DoctorReport>;

    /// Extra validation hook (optional, for backend-specific checks).
    ///
    /// Receives actual target's backend options. Default implementation
    /// does nothing and returns Ok.
    fn validate_extra(
        &self,
        _intent: &BuildIntent,
        _opts: &BackendOptions,
    ) -> Result<(), ValidationError> {
        Ok(())
    }
}

/// Validation result from successful validation.
#[derive(Debug, Clone, Default)]
pub struct ValidationResult {
    /// Warnings (non-fatal issues)
    pub warnings: Vec<String>,

    /// Adjusted intent (if backend made changes)
    pub adjusted_intent: Option<BuildIntent>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backend_availability() {
        let avail = BackendAvailability::Available {
            version: semver::Version::new(3, 20, 0),
        };
        assert!(avail.is_available());
        assert!(avail.error_message().is_none());

        let not_installed = BackendAvailability::NotInstalled {
            tool: "cmake".to_string(),
            install_hint: "apt install cmake".to_string(),
        };
        assert!(!not_installed.is_available());
        assert!(not_installed.error_message().unwrap().contains("cmake not found"));
    }

    #[test]
    fn test_build_context() {
        let ctx = BuildContext::new(
            PathBuf::from("/src"),
            PathBuf::from("/build"),
            PathBuf::from("/install"),
        )
        .with_release(true)
        .with_jobs(Some(4))
        .with_verbose(true);

        assert!(ctx.release);
        assert_eq!(ctx.jobs, Some(4));
        assert!(ctx.verbose);
    }

    #[test]
    fn test_test_result() {
        let result = TestResult {
            total: 10,
            passed: 8,
            failed: 2,
            skipped: 0,
            tests: Vec::new(),
        };
        assert!(!result.success());

        let success = TestResult {
            total: 10,
            passed: 10,
            failed: 0,
            skipped: 0,
            tests: Vec::new(),
        };
        assert!(success.success());
    }

    #[test]
    fn test_discovered_surface_to_surface() {
        let mut discovered = DiscoveredSurface::default();
        discovered.include_dirs.push(PathBuf::from("/usr/include/foo"));
        discovered.defines.push(Define::simple("FOO_ENABLED"));
        discovered.defines.push(Define::with_value("FOO_VERSION", "1"));
        discovered.libraries.push(LibraryInfo {
            name: "foo".to_string(),
            kind: LibraryKind::Static,
            path: PathBuf::from("/usr/lib/libfoo.a"),
            soname: None,
            import_lib: None,
        });

        let surface = discovered.to_surface();
        assert_eq!(surface.compile.public.include_dirs.len(), 1);
        assert_eq!(surface.compile.public.defines.len(), 2);
        assert_eq!(surface.link.public.libs.len(), 1);
    }
}
