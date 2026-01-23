//! Package sources.
//!
//! Sources are responsible for fetching packages from various locations
//! (local paths, git repositories, registries).

pub mod cache;
pub mod git;
pub mod path;
pub mod source;

pub use cache::SourceCache;
pub use git::GitSource;
pub use path::PathSource;
pub use source::Source;
