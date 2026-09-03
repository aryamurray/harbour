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
/// This module is normally only available when compiling with `--cfg test`
/// (i.e. `cargo test --lib`). It provides mock implementations for
/// filesystem, process execution, and HTTP operations, plus fixture
/// builders (e.g. a local git-backed registry fixture) for hermetic
/// integration tests.
///
/// The `test-support` feature additionally exposes it to integration tests
/// under `tests/`, which link this crate as an ordinary (non-`#[cfg(test)]`)
/// dependency; see the `harbour-cli` self-dependency in `[dev-dependencies]`.
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

pub use core::{
    dependency::Dependency, manifest::Manifest, package::Package, package_id::PackageId,
    source_id::SourceId, surface::Surface, target::Target, workspace::Workspace,
};

pub use resolver::Resolve;
pub use util::context::GlobalContext;
