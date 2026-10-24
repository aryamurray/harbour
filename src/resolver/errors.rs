//! The errors dependency resolution reports to the user.
//!
//! Each is a [`miette::Diagnostic`] so the renderer (`util::report`) can draw it
//! with its code, its help and its related items, while `Display` stays the
//! one-line fact the tests and `grep` look for. They travel inside `anyhow`
//! (`resolver` returns `anyhow::Result`), and `report::as_diagnostic` names each
//! of them, since `anyhow` cannot downcast to a trait object.
//!
//! The codes are stable and meant to be searched for:
//!
//! | code | raised when |
//! |---|---|
//! | `harbour::resolver::cycle` | the resolved packages depend on each other in a loop |
//! | `harbour::resolver::duplicate_source` | one package name is reachable from two sources |
//! | `harbour::resolver::no_solution` | no set of versions satisfies every requirement |
//! | `harbour::resolver::failed` | resolution stopped for another reason (a source could not be read) |

use miette::Diagnostic;
use thiserror::Error;

/// The resolved packages depend on each other in a loop.
///
/// `Display` is the loop followed by one line per edge: whether the dependency is
/// required or which feature of the declaring package switched an optional one
/// on. The edges stay in the message rather than in `#[related]` diagnostics for
/// two reasons: miette draws each related item as its own block with its own
/// header, which spreads one short fact over a screen, and anything that prints
/// the error without going through the renderer (a test, a log line) would lose
/// them. The help says what would break the loop and is true of *this* loop: it
/// names the features to turn off when an edge is optional, and says the
/// manifests themselves are cyclic when none is.
#[derive(Debug, Diagnostic)]
#[diagnostic(code(harbour::resolver::cycle), help("{help}"))]
pub struct DependencyCycle {
    /// `a -> b -> a`.
    pub loop_path: String,
    pub help: String,
    pub edges: Vec<CycleEdge>,
}

impl std::fmt::Display for DependencyCycle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "cycle detected in dependency graph: {}", self.loop_path)?;
        for edge in &self.edges {
            write!(f, "\n  {edge}")?;
        }
        Ok(())
    }
}

impl std::error::Error for DependencyCycle {}

/// One edge of a [`DependencyCycle`]: `from` needs `to`, and why that edge exists.
#[derive(Debug)]
pub struct CycleEdge {
    pub from: String,
    pub to: String,
    pub how: String,
}

impl std::fmt::Display for CycleEdge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} -> {}  ({})", self.from, self.to, self.how)
    }
}

/// One package name is reachable from two different sources.
///
/// Harbour links one copy of each package, so building would silently link two
/// and risk duplicate symbols.
#[derive(Debug, Error, Diagnostic)]
#[error("package `{package}` is available from more than one source: {sources}")]
#[diagnostic(
    code(harbour::resolver::duplicate_source),
    help(
        "Harbour links one copy of each package, so all dependents of `{package}` must agree on \
         a single source (the same registry entry, or the same `git` URL) - otherwise the build \
         would silently link two copies and risk duplicate symbols"
    )
)]
pub struct DuplicateSource {
    pub package: String,
    /// The sources, comma-separated, as they are shown to the user.
    pub sources: String,
}

/// No set of versions satisfies every requirement.
///
/// The explanation is PubGrub's own derivation, kept in the message: it is the
/// chain of reasoning ("because A needs B and B has no version in ^2, A is
/// forbidden"), and splitting it into pieces would lose the order that makes it
/// readable. The help adds what to do about it, which PubGrub does not say.
#[derive(Debug, Error, Diagnostic)]
#[error("dependency resolution failed:\n{report}")]
#[diagnostic(
    code(harbour::resolver::no_solution),
    help(
        "every package is built once, so all of its dependents need a version that satisfies \
         them all: loosen one of the requirements above, or move the conflicting dependency \
         to a version they can agree on"
    )
)]
pub struct NoSolution {
    pub report: String,
}

/// Resolution stopped for a reason other than a conflict, typically a package's
/// source that could not be read part-way through. `message` is the cause, not a
/// debug dump of the solver's internal state.
#[derive(Debug, Error, Diagnostic)]
#[error("dependency resolution failed: {message}")]
#[diagnostic(code(harbour::resolver::failed))]
pub struct ResolutionFailed {
    pub message: String,
}
