//! Core data structures for Harbour.
//!
//! This module contains the foundational types used throughout Harbour:
//! - Interned identifiers (SourceId, PackageId)
//! - Surface contracts (compile/link surfaces)
//! - Manifests and dependencies
//! - Workspace management

pub mod abi;
pub mod dependency;
pub mod manifest;
pub mod package;
pub mod package_id;
pub mod registry;
pub mod source_id;
pub mod summary;
pub mod surface;
pub mod target;
pub mod workspace;

pub use dependency::Dependency;
pub use manifest::Manifest;
pub use package::Package;
pub use package_id::PackageId;
pub use source_id::SourceId;
pub use summary::Summary;
pub use surface::Surface;
pub use target::Target;
pub use workspace::{
    find_lockfile, find_manifest, Workspace, LOCKFILE_ALIAS, LOCKFILE_NAME, MANIFEST_ALIAS,
    MANIFEST_NAME,
};
