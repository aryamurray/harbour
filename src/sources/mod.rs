//! Package sources.
//!
//! Sources are responsible for fetching packages from various locations
//! (local paths, git repositories, registries).

pub mod cache;
pub mod git;
pub mod path;
pub mod registry;
pub mod source;

pub use cache::SourceCache;
pub use git::GitSource;
pub use path::PathSource;
pub use registry::RegistrySource;
pub use source::Source;
