//! Resolution error types and diagnostics.

use thiserror::Error;

use crate::util::diagnostic::Diagnostic;

/// Error during dependency resolution.
#[derive(Debug, Error)]
pub enum ResolveError {
    #[error("no matching version for `{package}`")]
    NoMatchingVersion {
        package: String,
        requirement: String,
        available: Vec<String>,
    },

    #[error("version conflict for `{package}`")]
    VersionConflict {
        package: String,
        requirements: Vec<(String, String)>, // (requirer, requirement)
    },

    #[error("feature conflict for `{package}`")]
    FeatureConflict {
        package: String,
        conflicts: Vec<(String, String)>, // (requirer, feature)
    },

    #[error("cycle detected in dependency graph")]
    CycleDetected { packages: Vec<String> },

    #[error("package not found: `{package}`")]
    PackageNotFound {
        package: String,
        suggestions: Vec<String>,
    },

    #[error("source error for `{source_name}`: {message}")]
    SourceError { source_name: String, message: String },
}

impl ResolveError {
    /// Convert to a user-friendly diagnostic.
    pub fn to_diagnostic(&self) -> Diagnostic {
        match self {
            ResolveError::NoMatchingVersion {
                package,
                requirement,
                available,
            } => {
                let mut diag = Diagnostic::error(format!(
                    "no version of `{}` matches requirement `{}`",
                    package, requirement
                ));

                if !available.is_empty() {
                    diag = diag.with_context(format!(
                        "available versions: {}",
                        available.join(", ")
                    ));
                }

                diag = diag.with_suggestion(format!(
                    "Update your version requirement for `{}`",
                    package
                ));

                diag
            }

            ResolveError::VersionConflict {
                package,
                requirements,
            } => {
                let mut diag =
                    Diagnostic::error(format!("version conflict for `{}`", package));

                for (requirer, req) in requirements {
                    diag = diag.with_context(format!("`{}` requires {} {}", requirer, package, req));
                }

                diag = diag
                    .with_suggestion(format!(
                        "Upgrade packages to compatible versions of `{}`",
                        package
                    ))
                    .with_suggestion(format!(
                        "Vendor one copy of `{}`: `harbour vendor {}`",
                        package, package
                    ));

                diag
            }

            ResolveError::FeatureConflict { package, conflicts } => {
                let mut diag =
                    Diagnostic::error(format!("conflicting features for `{}`", package));

                for (requirer, feature) in conflicts {
                    diag = diag.with_context(format!(
                        "`{}` requires {}[{}]",
                        requirer, package, feature
                    ));
                }

                diag = diag.with_suggestion(
                    "Align feature selection across all dependencies".to_string(),
                );

                diag
            }

            ResolveError::CycleDetected { packages } => {
                let mut diag = Diagnostic::error("cycle detected in dependency graph");

                diag = diag.with_context(format!("cycle: {}", packages.join(" -> ")));

                diag = diag.with_suggestion(
                    "Break the cycle by removing or restructuring dependencies".to_string(),
                );

                diag
            }

            ResolveError::PackageNotFound {
                package,
                suggestions,
            } => {
                let mut diag =
                    Diagnostic::error(format!("could not find package `{}`", package));

                if !suggestions.is_empty() {
                    diag = diag.with_context(format!(
                        "did you mean: {}?",
                        suggestions.join(", ")
                    ));
                }

                diag = diag
                    .with_suggestion("Check that the package name is spelled correctly".to_string())
                    .with_suggestion("Ensure the package source is accessible".to_string());

                diag
            }

            ResolveError::SourceError { source_name, message } => {
                Diagnostic::error(format!("error fetching from `{}`: {}", source_name, message))
                    .with_suggestion("Check your network connection".to_string())
                    .with_suggestion("Verify the source URL is correct".to_string())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_conflict_diagnostic() {
        let err = ResolveError::VersionConflict {
            package: "openssl".to_string(),
            requirements: vec![
                ("myapp".to_string(), "^3.0".to_string()),
                ("legacy-lib".to_string(), "^1.1".to_string()),
            ],
        };

        let diag = err.to_diagnostic();
        let output = diag.format(false);

        assert!(output.contains("version conflict"));
        assert!(output.contains("openssl"));
        assert!(output.contains("myapp"));
        assert!(output.contains("legacy-lib"));
    }

    #[test]
    fn test_feature_conflict_diagnostic() {
        let err = ResolveError::FeatureConflict {
            package: "zlib".to_string(),
            conflicts: vec![
                ("libfoo".to_string(), "shared".to_string()),
                ("libbar".to_string(), "static".to_string()),
            ],
        };

        let diag = err.to_diagnostic();
        let output = diag.format(false);

        assert!(output.contains("conflicting features"));
        assert!(output.contains("zlib[shared]"));
        assert!(output.contains("zlib[static]"));
    }
}
