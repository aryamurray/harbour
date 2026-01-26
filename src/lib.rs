//! Harbour - A Cargo-like package manager and build system for C
//!
//! This crate provides the core library functionality for Harbour,
//! including dependency resolution, build planning, and execution.

pub mod builder;
pub mod core;
pub mod ops;
pub mod resolver;
pub mod sources;
pub mod util;

/// Test utilities and mocks for Harbour unit tests.
///
/// This module is only available when compiling with `--cfg test` or
/// running tests. It provides mock implementations for filesystem,
/// process execution, and HTTP operations.
#[cfg(test)]
pub mod test_support;

pub use core::{
    dependency::Dependency, manifest::Manifest, package::Package, package_id::PackageId,
    source_id::SourceId, surface::Surface, target::Target, workspace::Workspace,
};

pub use resolver::Resolve;
pub use util::context::GlobalContext;
