//! CLI integration tests for Harbour.
//!
//! These tests verify the full CLI workflow from project creation through building.
//!
//! ## Hermeticity
//!
//! These tests must never touch the network or the developer's real home
//! directory / global Harbour cache:
//!
//! - Every invocation of `harbour` goes through [`harbour`], which points
//!   `HOME` / the XDG base directories at a per-test temporary directory
//!   (see [`harbour_home`]), so nothing is ever read from or written to the
//!   real `~/.harbour` cache.
//! - Any test that needs `harbour add` to resolve a registry dependency
//!   points it at a local, git-backed fixture registry (see
//!   `harbour::test_support::fixtures::local_registry`) via the
//!   `HARBOUR_TEST_REGISTRY_URL` environment variable, instead of the real
//!   `https://github.com/aryamurray/harbour-registry`.
//!
//! ## Layout
//!
//! One test crate, one module per area (see `tests/README.md`). Shared helpers live in
//! `common.rs`; add a NEW module for a NEW area instead of appending to an old one.

// The one shared definition of how a test isolates the binary from the machine
// (`isolate`); `common.rs` below builds `harbour()` on it.
#[path = "../common/mod.rs"]
mod shared;

mod commands;
mod common;
mod cross;
mod dependencies;
mod errors;
mod feature_flags;
mod features;
mod flags_profiles;
mod generators;
mod host_tools;
mod incremental;
mod local_deps;
mod manifest;
mod probes;
mod registry;
mod workspaces;
