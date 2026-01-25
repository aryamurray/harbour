//! C/C++ build system.
//!
//! This module implements the native C/C++ compiler driver and build planning.

pub mod bindings;
pub mod cmake;
pub mod context;
pub mod events;
pub mod executor;
pub mod fingerprint;
pub mod interop;
pub mod native;
pub mod plan;
pub mod shim;
pub mod surface_resolver;
pub mod toolchain;

pub use context::BuildContext;
pub use events::BuildEvent;
pub use executor::BuildExecutor;
pub use native::NativeBuilder;
pub use plan::BuildPlan;
pub use surface_resolver::SurfaceResolver;
pub use toolchain::{
    detect_toolchain, CommandSpec, CxxOptions, GccToolchain, LinkMode, MsvcToolchain, Toolchain,
    ToolchainPlatform,
};
