//! Which registry `verify` reads, and the source cache it reads it through.
//!
//! There is nothing here that fetches, patches or resolves. `verify` used to
//! carry its own copy of all three, and the copies disagreed with the real ones
//! (a patch applied a second time to a tree that already had it; a manifest
//! the shim's patch had just added, overwritten by a generated one). It now
//! hands a throwaway consumer project to the ordinary resolver and the ordinary
//! [`SourceCache`], so the only thing left to decide is *which registry* that
//! consumer resolves against.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use url::Url;

use super::types::VerifyOptions;
use crate::core::SourceId;
use crate::sources::registry::{RegistrySource, Shim};
use crate::sources::SourceCache;
use crate::util::context::GlobalContext;

/// The registry a verification resolves against.
pub(crate) struct RegistrySelection {
    /// The URL the consumer's dependency resolves against. For a local
    /// registry this is the `file://` URL of its directory.
    pub url: Url,
    /// The checkout it is served from, when it is not cloned.
    pub local: Option<PathBuf>,
}

/// `--registry-path`, else the configured default registry: the one a
/// consumer's `pkg = "1"` would use.
pub(crate) fn select_registry(
    options: &VerifyOptions,
    ctx: &GlobalContext,
) -> Result<RegistrySelection> {
    let Some(path) = &options.registry_path else {
        return Ok(RegistrySelection {
            url: ctx.default_registry_url()?,
            local: None,
        });
    };

    if !path.is_dir() {
        bail!(
            "registry path {} is not a directory\n\
             hint: --registry-path takes a checkout of a registry (the directory holding \
             `config.toml` and `index/`)",
            path.display()
        );
    }
    let path = crate::util::fs::normalize_path(path);
    if !path.join("config.toml").is_file() {
        bail!(
            "{} is not a registry checkout: it has no `config.toml`",
            path.display()
        );
    }
    let url = Url::from_file_path(&path)
        .map_err(|()| anyhow::anyhow!("registry path {} cannot be a URL", path.display()))?;
    Ok(RegistrySelection {
        url,
        local: Some(path),
    })
}

/// The source cache a consumer's build uses, pointed at `registry`.
///
/// The cache directory is the global one, not a private `verify` directory:
/// that is what makes a second run find what the first fetched, and what makes
/// `verify` and `harbour build` fetch (and patch) a package the same way.
pub(crate) fn source_cache(registry: &RegistrySelection, ctx: &GlobalContext) -> SourceCache {
    let cache = SourceCache::new(ctx.cache_dir()).with_default_registry(registry.url.as_str());
    match &registry.local {
        Some(path) => cache.with_local_registry(registry.url.as_str(), path.clone()),
        None => cache,
    }
}

/// The shim for `name` `version`, read from the registry's index.
///
/// Verification needs it for one thing the resolver does not carry: the
/// shim's `[metadata.harness]`. It is read from the index checkout the
/// resolver has just updated, so no second fetch happens.
pub(crate) fn load_shim(
    registry: &RegistrySelection,
    ctx: &GlobalContext,
    name: &str,
    version: &str,
) -> Result<Option<Shim>> {
    let source_id = SourceId::for_registry(&registry.url)?;
    let mut source = match &registry.local {
        Some(path) => RegistrySource::at_path(path, &ctx.cache_dir(), source_id)?,
        None => RegistrySource::new(registry.url.clone(), &ctx.cache_dir(), source_id),
    };
    source
        .load_shim(name, version)
        .with_context(|| format!("failed to read the shim for {name} {version}"))
}

/// The version requirement a consumer writes to get what was asked for:
/// exactly `version`, or whatever is newest.
pub(crate) fn version_requirement(version: Option<&str>) -> String {
    match version {
        Some(v) => format!("={}", v.trim_start_matches('=')),
        None => "*".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requirement_pins_an_explicit_version_and_floats_otherwise() {
        assert_eq!(version_requirement(Some("1.3.1")), "=1.3.1");
        assert_eq!(version_requirement(Some("=1.3.1")), "=1.3.1");
        assert_eq!(version_requirement(None), "*");
    }

    #[test]
    fn a_registry_path_without_config_is_refused_by_name() {
        let tmp = tempfile::tempdir().unwrap();
        let options = VerifyOptions {
            registry_path: Some(tmp.path().to_path_buf()),
            ..VerifyOptions::for_package("zlib")
        };
        let ctx = GlobalContext::with_cwd_isolated(tmp.path().to_path_buf()).unwrap();
        let err = select_registry(&options, &ctx).err().unwrap().to_string();
        assert!(err.contains("config.toml"), "{err}");
    }
}
