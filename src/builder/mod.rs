//! C build system.
//!
//! This module implements the native C compiler driver and build planning.

pub mod cmake;
pub mod context;
pub mod executor;
pub mod fingerprint;
pub mod interop;
pub mod native;
pub mod plan;
pub mod surface_resolver;
pub mod toolchain;

pub use context::BuildContext;
pub use executor::BuildExecutor;
pub use native::NativeBuilder;
pub use plan::BuildPlan;
pub use surface_resolver::SurfaceResolver;
pub use toolchain::{
    detect_toolchain, CommandSpec, GccToolchain, MsvcToolchain, Toolchain, ToolchainPlatform,
};
