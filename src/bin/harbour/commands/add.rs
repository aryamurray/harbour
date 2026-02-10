//! `harbour add` command

use anyhow::{bail, Result};

use crate::cli::AddArgs;
use crate::GlobalOptions;
use harbour::core::abi::TargetTriple;
use harbour::ops::harbour_add::validate_registry_dependency;
use harbour::ops::harbour_add::{add_dependency, AddOptions, AddResult, SourceKind};
use harbour::util::config::load_config;
use harbour::util::VcpkgIntegration;
use harbour::util::{GlobalContext, Status};

/// Parsed package specification from inline syntax.
/// Supports: `name`, `name[feat1,feat2]`, `name:triplet`, `name[feat1,feat2]:triplet`
#[derive(Debug, Default)]
struct PackageSpec {
    name: String,
    features: Vec<String>,
    triplet: Option<String>,
}

/// Parse a package spec like `glfw3[wayland,x11]:x64-linux`
fn parse_package_spec(input: &str) -> PackageSpec {
    let mut spec = PackageSpec::default();
    let mut remaining = input;

    // Parse triplet suffix first (after last colon not inside brackets)
    // e.g., "glfw3[feat]:x64-linux" -> triplet = "x64-linux"
    if let Some(colon_pos) = remaining.rfind(':') {
        let before_colon = &remaining[..colon_pos];
        let after_colon = &remaining[colon_pos + 1..];

        // Only treat as triplet if colon is after any brackets
        if !before_colon.contains('[') || before_colon.contains(']') {
            if !after_colon.is_empty() && !after_colon.contains('[') {
                spec.triplet = Some(after_colon.to_string());
                remaining = before_colon;
            }
        }
    }

    // Parse features in brackets
    // e.g., "glfw3[wayland,x11]" -> features = ["wayland", "x11"]
    if let Some(bracket_start) = remaining.find('[') {
        if let Some(bracket_end) = remaining.find(']') {
            if bracket_end > bracket_start {
                let features_str = &remaining[bracket_start + 1..bracket_end];
                spec.features = features_str
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                spec.name = remaining[..bracket_start].to_string();
                return spec;
            }
        }
    }

    // No brackets, just the name
    spec.name = remaining.to_string();
    spec
}

/// Validates that --path and --git are not both specified.
///
/// Returns an error message if both are specified.
pub fn validate_source_args(
    path: &Option<String>,
    git: &Option<String>,
) -> Result<(), &'static str> {
    if path.is_some() && git.is_some() {
        Err("cannot specify both --path and --git")
    } else {
        Ok(())
    }
}

/// Validates git source arguments.
///
/// Returns an error if conflicting git ref arguments are specified.
pub fn validate_git_ref_args(
    branch: &Option<String>,
    tag: &Option<String>,
    rev: &Option<String>,
) -> Result<(), &'static str> {
    let ref_count = [branch.is_some(), tag.is_some(), rev.is_some()]
        .iter()
        .filter(|&&x| x)
        .count();

    if ref_count > 1 {
        Err("cannot specify more than one of --branch, --tag, or --rev")
    } else {
        Ok(())
    }
}

pub fn execute(args: AddArgs, global_opts: &GlobalOptions) -> Result<()> {
    let shell = &global_opts.shell;

    // Validate arguments
    if let Err(msg) = validate_source_args(&args.path, &args.git) {
        shell.error(msg);
        bail!(msg);
    }

    if let Err(msg) = validate_git_ref_args(&args.branch, &args.tag, &args.rev) {
        shell.error(msg);
        bail!(msg);
    }

    // Parse inline package spec: glfw3[wayland,x11]:x64-linux
    let spec = parse_package_spec(&args.name);

    // Merge features from inline spec and --features flag
    let mut features: Vec<String> = spec.features;
    features.extend(args.features.into_iter());

    // Merge triplet: CLI flag takes precedence over inline spec
    let triplet = args.triplet.or(spec.triplet);

    // Infer vcpkg mode if vcpkg-specific options are provided
    let has_vcpkg_options = triplet.is_some()
        || !features.is_empty()
        || args.baseline.is_some()
        || args.registry.is_some();
    let vcpkg = args.vcpkg || has_vcpkg_options;

    let ctx = GlobalContext::new()?;

    let manifest_path = ctx.find_manifest()?;

    let mut opts = AddOptions {
        name: spec.name,
        path: args.path,
        git: args.git,
        branch: args.branch,
        tag: args.tag,
        rev: args.rev,
        version: args.version,
        vcpkg,
        triplet,
        vcpkg_libs: None,
        vcpkg_features: if features.is_empty() {
            None
        } else {
            Some(features)
        },
        vcpkg_baseline: args.baseline,
        vcpkg_registry: args.registry,
        optional: args.optional,
        dry_run: args.dry_run,
        offline: global_opts.offline,
    };

    // If explicit vcpkg flag is set, we're done with source selection
    if opts.vcpkg {
        // Use provided triplet or auto-detect later
    } else if opts.path.is_none() && opts.git.is_none() {
        let registry_not_found = validate_registry_dependency(
            &opts.name,
            opts.version.as_deref(),
            ctx.registries(),
            &ctx.cache_dir(),
            global_opts.offline,
        )?;

        if registry_not_found.is_some() {
            let config = load_config(
                &ctx.config_path(),
                &ctx.project_harbour_dir().join("config.toml"),
            );
            let integration =
                VcpkgIntegration::from_config(&config.vcpkg, &TargetTriple::host(), false);

            if let Some(integration) = integration {
                opts.vcpkg = true;
                opts.triplet = Some(integration.triplet.clone());
            } else if let Some((name, looked_in)) = registry_not_found {
                let registries = looked_in
                    .iter()
                    .map(|r| r.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                shell.error(format!(
                    "package `{}` not found in registries: {}",
                    name, registries
                ));
                shell.error(
                    "vcpkg is not configured; set VCPKG_ROOT or configure [vcpkg]".to_string(),
                );
                bail!("package `{}` not found", name);
            }
        }
    }

    let result = add_dependency(&manifest_path, &opts)?;

    // Format source for display
    fn format_source(source: &SourceKind) -> &'static str {
        match source {
            SourceKind::Registry(_) => "registry",
            SourceKind::Git(_) => "git",
            SourceKind::Path(_) => "path",
            SourceKind::Vcpkg => "vcpkg",
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
    // Package Spec Parsing Tests
    // =========================================================================

    #[test]
    fn test_parse_package_spec_name_only() {
        let spec = parse_package_spec("zlib");
        assert_eq!(spec.name, "zlib");
        assert!(spec.features.is_empty());
        assert!(spec.triplet.is_none());
    }

    #[test]
    fn test_parse_package_spec_with_features() {
        let spec = parse_package_spec("glfw3[wayland,x11]");
        assert_eq!(spec.name, "glfw3");
        assert_eq!(spec.features, vec!["wayland", "x11"]);
        assert!(spec.triplet.is_none());
    }

    #[test]
    fn test_parse_package_spec_with_triplet() {
        let spec = parse_package_spec("zlib:x64-linux");
        assert_eq!(spec.name, "zlib");
        assert!(spec.features.is_empty());
        assert_eq!(spec.triplet, Some("x64-linux".to_string()));
    }

    #[test]
    fn test_parse_package_spec_full() {
        let spec = parse_package_spec("glfw3[wayland,x11]:x64-linux");
        assert_eq!(spec.name, "glfw3");
        assert_eq!(spec.features, vec!["wayland", "x11"]);
        assert_eq!(spec.triplet, Some("x64-linux".to_string()));
    }

    #[test]
    fn test_parse_package_spec_single_feature() {
        let spec = parse_package_spec("curl[ssl]");
        assert_eq!(spec.name, "curl");
        assert_eq!(spec.features, vec!["ssl"]);
    }

    #[test]
    fn test_parse_package_spec_features_with_spaces() {
        let spec = parse_package_spec("pkg[feat1, feat2 , feat3]");
        assert_eq!(spec.name, "pkg");
        assert_eq!(spec.features, vec!["feat1", "feat2", "feat3"]);
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
    // Vcpkg Features Flag Tests
    // =========================================================================

    #[test]
    fn test_add_vcpkg_with_comma_features() {
        let args = parse_add_args(&[
            "test",
            "glfw3",
            "--vcpkg",
            "--features",
            "wayland,x11,vulkan",
        ]);
        assert_eq!(args.features, vec!["wayland", "x11", "vulkan"]);
    }

    #[test]
    fn test_add_vcpkg_with_short_features_flag() {
        let args = parse_add_args(&["test", "glfw3", "--vcpkg", "-F", "wayland,x11"]);
        assert_eq!(args.features, vec!["wayland", "x11"]);
    }

    #[test]
    fn test_add_vcpkg_baseline_alias() {
        // Both --baseline and --vcpkg-baseline should work
        let args1 = parse_add_args(&["test", "zlib", "--vcpkg", "--baseline", "abc123"]);
        let args2 = parse_add_args(&["test", "zlib", "--vcpkg", "--vcpkg-baseline", "abc123"]);
        assert_eq!(args1.baseline, Some("abc123".to_string()));
        assert_eq!(args2.baseline, Some("abc123".to_string()));
    }

    #[test]
    fn test_add_vcpkg_registry_alias() {
        // Both --registry and --vcpkg-registry should work
        let args1 = parse_add_args(&["test", "mylib", "--vcpkg", "--registry", "internal"]);
        let args2 = parse_add_args(&["test", "mylib", "--vcpkg", "--vcpkg-registry", "internal"]);
        assert_eq!(args1.registry, Some("internal".to_string()));
        assert_eq!(args2.registry, Some("internal".to_string()));
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
            vcpkg: false,
            triplet: None,
            vcpkg_libs: None,
            vcpkg_features: None,
            vcpkg_baseline: None,
            vcpkg_registry: None,
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
