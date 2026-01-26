//! High-level operations.
//!
//! This module contains the implementation of Harbour commands.

pub mod doctor;
pub mod ffi_bundle;
pub mod harbour_add;
pub mod harbour_build;
pub mod harbour_new;
pub mod harbour_update;
pub mod lockfile;
pub mod resolve;
pub mod verify;

pub use doctor::{doctor, format_report, DoctorOptions, DoctorReport};
pub use ffi_bundle::{
    create_ffi_bundle, BundleManifest, BundleOptions, BundleResult, EnumVariant, ExportedConstant,
    ExportedFunction, FunctionParam, StructField, TypeDefinition,
};
pub use harbour_add::{
    add_dependency, remove_dependency, AddOptions, AddResult, RegistryId, RemoveOptions,
    RemoveResult, SourceKind,
};
pub use harbour_build::{build, BuildOptions};
pub use harbour_new::{init_project, new_project};
pub use harbour_update::update;
pub use lockfile::{load_lockfile, save_lockfile};
pub use resolve::{resolve_workspace, resolve_workspace_with_opts, ResolveOptions};
pub use verify::{format_result, verify, VerifyOptions, VerifyResult};
