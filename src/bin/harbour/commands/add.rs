//! `harbour add` command

use anyhow::{bail, Result};

use crate::cli::AddArgs;
use crate::GlobalOptions;
use harbour::ops::harbour_add::{add_dependency, AddOptions, AddResult, SourceKind};
use harbour::util::{GlobalContext, Status};

pub fn execute(args: AddArgs, global_opts: &GlobalOptions) -> Result<()> {
    let shell = &global_opts.shell;

    // Validate arguments - can't specify both path and git
    if args.path.is_some() && args.git.is_some() {
        shell.error("cannot specify both --path and --git");
        bail!("cannot specify both --path and --git");
    }

    // If neither path nor git specified, it's a registry dependency
    // The version defaults to "*" if not specified

    let ctx = GlobalContext::new()?;

    let manifest_path = ctx.find_manifest()?;

    let opts = AddOptions {
        name: args.name.clone(),
        path: args.path,
        git: args.git,
        branch: args.branch,
        tag: args.tag,
        rev: args.rev,
        version: args.version,
        optional: args.optional,
        dry_run: args.dry_run,
        offline: global_opts.offline,
    };

    let result = add_dependency(&manifest_path, &opts)?;

    // Format source for display
    fn format_source(source: &SourceKind) -> &'static str {
        match source {
            SourceKind::Registry(_) => "registry",
            SourceKind::Git(_) => "git",
            SourceKind::Path(_) => "path",
        }
    }

    // Output result based on what happened
    match result {
        AddResult::Added {
            name,
            version,
            source,
        } => {
            if args.dry_run {
                shell.status(
                    Status::Info,
                    format!(
                        "Would add {} v{} ({})",
                        name,
                        version,
                        format_source(&source)
                    ),
                );
            } else {
                shell.status(
                    Status::Added,
                    format!("{} v{} ({})", name, version, format_source(&source)),
                );
            }
        }
        AddResult::Updated {
            name,
            from,
            to,
            source,
        } => {
            if args.dry_run {
                shell.status(
                    Status::Info,
                    format!(
                        "Would update {} v{} -> v{} ({})",
                        name,
                        from,
                        to,
                        format_source(&source)
                    ),
                );
            } else {
                shell.status(
                    Status::Updated,
                    format!("{} v{} -> v{} ({})", name, from, to, format_source(&source)),
                );
            }
        }
        AddResult::AlreadyPresent { name, version } => {
            shell.status(
                Status::Skipped,
                format!("{} v{} (already in dependencies)", name, version),
            );
        }
        AddResult::NotFound { name, looked_in } => {
            let registries = looked_in
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            shell.error(format!(
                "package `{}` not found in registries: {}",
                name, registries
            ));
            bail!("package `{}` not found", name);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::AddArgs;
    use clap::Parser;

    /// Helper to parse AddArgs from command-line strings.
    fn parse_add_args(args: &[&str]) -> AddArgs {
        #[derive(Parser)]
        struct TestCli {
            #[command(flatten)]
            add: AddArgs,
        }
        let cli = TestCli::parse_from(args);
        cli.add
    }

    // =========================================================================
    // AddArgs Default Values Tests
    // =========================================================================

    #[test]
    fn test_add_args_with_name_only() {
        let args = parse_add_args(&["test", "zlib"]);

        assert_eq!(args.name, "zlib");
        assert!(args.path.is_none());
        assert!(args.git.is_none());
        assert!(args.branch.is_none());
        assert!(args.tag.is_none());
        assert!(args.rev.is_none());
        assert!(args.version.is_none());
        assert!(!args.optional);
        assert!(!args.dry_run);
    }

    // =========================================================================
    // Package Name Tests
    // =========================================================================

    #[test]
    fn test_add_simple_package_name() {
        let args = parse_add_args(&["test", "openssl"]);
        assert_eq!(args.name, "openssl");
    }

    #[test]
    fn test_add_package_name_with_hyphen() {
        let args = parse_add_args(&["test", "my-package"]);
        assert_eq!(args.name, "my-package");
    }

    #[test]
    fn test_add_package_name_with_underscore() {
        let args = parse_add_args(&["test", "my_package"]);
        assert_eq!(args.name, "my_package");
    }

    // =========================================================================
    // Version Tests
    // =========================================================================

    #[test]
    fn test_add_with_exact_version() {
        let args = parse_add_args(&["test", "zlib", "--version", "1.3.1"]);
        assert_eq!(args.name, "zlib");
        assert_eq!(args.version, Some("1.3.1".to_string()));
    }

    #[test]
    fn test_add_with_version_range() {
        let args = parse_add_args(&["test", "openssl", "--version", ">=1.1.0"]);
        assert_eq!(args.version, Some(">=1.1.0".to_string()));
    }

    #[test]
    fn test_add_with_caret_version() {
        let args = parse_add_args(&["test", "boost", "--version", "^1.80"]);
        assert_eq!(args.version, Some("^1.80".to_string()));
    }

    #[test]
    fn test_add_with_tilde_version() {
        let args = parse_add_args(&["test", "curl", "--version", "~8.0"]);
        assert_eq!(args.version, Some("~8.0".to_string()));
    }

    #[test]
    fn test_add_with_wildcard_version() {
        let args = parse_add_args(&["test", "json-c", "--version", "*"]);
        assert_eq!(args.version, Some("*".to_string()));
    }

    // =========================================================================
    // Path Dependency Tests
    // =========================================================================

    #[test]
    fn test_add_path_dependency() {
        let args = parse_add_args(&["test", "mylib", "--path", "../mylib"]);
        assert_eq!(args.name, "mylib");
        assert_eq!(args.path, Some("../mylib".to_string()));
        assert!(args.git.is_none());
    }

    #[test]
    fn test_add_path_dependency_absolute() {
        let args = parse_add_args(&["test", "mylib", "--path", "/home/user/libs/mylib"]);
        assert_eq!(args.path, Some("/home/user/libs/mylib".to_string()));
    }

    // =========================================================================
    // Git Dependency Tests
    // =========================================================================

    #[test]
    fn test_add_git_dependency() {
        let args = parse_add_args(&[
            "test",
            "mylib",
            "--git",
            "https://github.com/user/mylib.git",
        ]);
        assert_eq!(args.name, "mylib");
        assert_eq!(
            args.git,
            Some("https://github.com/user/mylib.git".to_string())
        );
        assert!(args.path.is_none());
    }

    #[test]
    fn test_add_git_with_branch() {
        let args = parse_add_args(&[
            "test",
            "mylib",
            "--git",
            "https://github.com/user/mylib.git",
            "--branch",
            "develop",
        ]);
        assert_eq!(
            args.git,
            Some("https://github.com/user/mylib.git".to_string())
        );
        assert_eq!(args.branch, Some("develop".to_string()));
    }

    #[test]
    fn test_add_git_with_tag() {
        let args = parse_add_args(&[
            "test",
            "mylib",
            "--git",
            "https://github.com/user/mylib.git",
            "--tag",
            "v1.0.0",
        ]);
        assert_eq!(args.tag, Some("v1.0.0".to_string()));
    }

    #[test]
    fn test_add_git_with_rev() {
        let args = parse_add_args(&[
            "test",
            "mylib",
            "--git",
            "https://github.com/user/mylib.git",
            "--rev",
            "abc123def",
        ]);
        assert_eq!(args.rev, Some("abc123def".to_string()));
    }

    // =========================================================================
    // Optional Dependency Tests
    // =========================================================================

    #[test]
    fn test_add_optional_dependency() {
        let args = parse_add_args(&["test", "libpng", "--optional"]);
        assert!(args.optional);
    }

    #[test]
    fn test_add_non_optional_by_default() {
        let args = parse_add_args(&["test", "libpng"]);
        assert!(!args.optional);
    }

    // =========================================================================
    // Dry Run Tests
    // =========================================================================

    #[test]
    fn test_add_dry_run() {
        let args = parse_add_args(&["test", "zlib", "--dry-run"]);
        assert!(args.dry_run);
    }

    // =========================================================================
    // Combined Flags Tests
    // =========================================================================

    #[test]
    fn test_add_registry_dependency_with_all_options() {
        let args = parse_add_args(&[
            "test",
            "openssl",
            "--version",
            "^3.0",
            "--optional",
            "--dry-run",
        ]);

        assert_eq!(args.name, "openssl");
        assert_eq!(args.version, Some("^3.0".to_string()));
        assert!(args.optional);
        assert!(args.dry_run);
    }

    #[test]
    fn test_add_git_dependency_with_branch_and_options() {
        let args = parse_add_args(&[
            "test",
            "mylib",
            "--git",
            "https://github.com/user/mylib.git",
            "--branch",
            "feature-x",
            "--optional",
        ]);

        assert_eq!(args.name, "mylib");
        assert_eq!(
            args.git,
            Some("https://github.com/user/mylib.git".to_string())
        );
        assert_eq!(args.branch, Some("feature-x".to_string()));
        assert!(args.optional);
    }

    // =========================================================================
    // AddOptions Construction Tests
    // =========================================================================

    #[test]
    fn test_add_options_from_args() {
        let args = parse_add_args(&[
            "test",
            "zlib",
            "--version",
            "1.3.1",
            "--optional",
            "--dry-run",
        ]);

        let opts = AddOptions {
            name: args.name.clone(),
            path: args.path,
            git: args.git,
            branch: args.branch,
            tag: args.tag,
            rev: args.rev,
            version: args.version,
            optional: args.optional,
            dry_run: args.dry_run,
            offline: false,
        };

        assert_eq!(opts.name, "zlib");
        assert_eq!(opts.version, Some("1.3.1".to_string()));
        assert!(opts.optional);
        assert!(opts.dry_run);
        assert!(!opts.offline);
    }
}
