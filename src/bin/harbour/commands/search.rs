//! `harbour search` command

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use harbour::sources::registry::index::{list_index_files, read_index_file};
use harbour::sources::registry::{GitTransport, IndexRecord, RegistryTransport};

use crate::cli::SearchArgs;
use crate::GlobalOptions;

/// A search result for a package.
#[derive(Debug)]
struct PackageResult {
    name: String,
    version: String,
    category: Option<String>,
    /// Every version is yanked, so none is offered to a new resolution.
    all_yanked: bool,
}

pub fn execute(args: SearchArgs, global_opts: &GlobalOptions) -> Result<()> {
    let query = args.query.to_lowercase();

    // `--registry-path` searches one local directory; otherwise search every
    // registry `harbour add` and resolution would, in the same priority order.
    let index_roots = if let Some(path) = args.registry_path {
        let index_path = path.join("index");
        if !index_path.exists() {
            bail!(
                "registry index not found at: {}\n\
                 hint: --registry-path must point at a registry checkout containing `index/`",
                index_path.display()
            );
        }
        vec![(path.display().to_string(), index_path)]
    } else {
        configured_index_roots(global_opts.offline)?
    };

    // A package in several registries is shown once, from the first (highest
    // priority) registry, as resolution would pick it.
    let mut results: Vec<PackageResult> = Vec::new();
    for (_, index_path) in &index_roots {
        for result in search_packages(index_path, &query)? {
            if !results.iter().any(|r| r.name == result.name) {
                results.push(result);
            }
        }
    }
    results.sort_by(|a, b| a.name.cmp(&b.name));

    if results.is_empty() {
        println!("No packages found matching '{}'", args.query);
        return Ok(());
    }

    // Display results
    println!(
        "Found {} package{} matching '{}':\n",
        results.len(),
        if results.len() == 1 { "" } else { "s" },
        args.query
    );

    for result in &results {
        let category_str = result
            .category
            .as_ref()
            .map(|c| format!(" [{}]", c))
            .unwrap_or_default();

        let yanked_str = if result.all_yanked {
            " (all versions yanked)"
        } else {
            ""
        };

        println!(
            "  {} v{}{}{}",
            result.name, result.version, category_str, yanked_str
        );
    }

    Ok(())
}

/// The local `index/` directory of every enabled registry, as `(name, path)`.
///
/// Registries come from the same [`GlobalContext`] list `harbour add` resolves
/// against, and their clones from the same [`GitTransport`] cache location the
/// resolver uses, so search sees exactly what `add` can find. A clone is
/// fetched or updated first, except with `--offline`, where only a clone that is
/// already in the cache can be searched. Registries sharing a URL (the built-in
/// `curated` and `default`) are read once. One unreachable registry is reported
/// and skipped; it is an error only if none could be read.
fn configured_index_roots(offline: bool) -> Result<Vec<(String, PathBuf)>> {
    let ctx = harbour::util::GlobalContext::new().context("failed to create global context")?;
    let cache_dir = ctx.cache_dir();

    let mut roots: Vec<(String, PathBuf)> = Vec::new();
    let mut seen_urls = Vec::new();
    let mut failures = Vec::new();
    for registry in ctx.registries().enabled() {
        let url = harbour::util::remote_url::parse_remote_url(&registry.url)
            .with_context(|| format!("registry '{}' has an invalid URL", registry.name))?;
        if seen_urls.contains(&url) {
            continue;
        }
        seen_urls.push(url.clone());

        let mut transport = GitTransport::new(url, &cache_dir);
        let ready = if offline && !transport.index_path().exists() {
            Err(anyhow::anyhow!(
                "not in the local cache, and --offline is set"
            ))
        } else if offline {
            Ok(())
        } else {
            transport.ensure_ready()
        };
        match ready {
            Ok(()) => roots.push((registry.name.clone(), transport.index_path().join("index"))),
            Err(err) => failures.push((registry.name.clone(), err)),
        }
    }

    for (name, err) in &failures {
        eprintln!("warning: skipping registry `{name}`: {err:#}");
    }
    if roots.is_empty() {
        match failures.first() {
            Some((name, _)) => bail!(
                "could not read any configured registry (first failure: `{name}`)\n\
                 hint: use --registry-path to search a local registry directory"
            ),
            None => bail!(
                "no registry configured\n\
                 hint: use --registry-path to search a local registry directory"
            ),
        }
    }
    Ok(roots)
}

/// Search for packages in the registry index.
///
/// Packages come from the tier-1 `.idx` files, listed and read by the same
/// registry code `harbour add` resolves through, so what `search` offers and what
/// `add` can find are one set. An unreadable index file is reported and skipped:
/// one corrupt package must not hide every other result.
fn search_packages(index_path: &Path, query: &str) -> Result<Vec<PackageResult>> {
    let mut results = Vec::new();

    for (name, idx_path) in list_index_files(index_path)? {
        if !name.contains(query) {
            continue;
        }
        match package_info(index_path, &name, &idx_path) {
            Ok(Some(result)) => results.push(result),
            Ok(None) => {}
            Err(err) => eprintln!("warning: skipping `{name}`: {err:#}"),
        }
    }

    // `list_index_files` is sorted, but sort here too: the order printed is this
    // function's contract, not an accident of the helper.
    results.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(results)
}

/// What `search` shows for one package: the newest version a new resolution
/// would use.
///
/// Yanked versions are skipped (they are not offered to a new resolution). If
/// *every* version is yanked the newest is shown anyway and flagged, so a package
/// never looks like it does not exist.
fn package_info(index_root: &Path, name: &str, idx_path: &Path) -> Result<Option<PackageResult>> {
    let Some(records) = read_index_file(idx_path)? else {
        return Ok(None);
    };

    let mut versions: Vec<(semver::Version, &IndexRecord)> = records
        .iter()
        .filter_map(|r| semver::Version::parse(&r.version).ok().map(|v| (v, r)))
        .collect();
    if versions.is_empty() {
        return Ok(None);
    }
    versions.sort_by(|a, b| b.0.cmp(&a.0));

    let available = versions.iter().find(|(_, r)| r.is_available());
    let (version, record, all_yanked) = match available {
        Some((v, r)) => (v, *r, false),
        None => (&versions[0].0, versions[0].1, true),
    };

    Ok(Some(PackageResult {
        name: name.to_string(),
        version: version.to_string(),
        category: extract_category(&index_root.join(&record.shim)),
        all_yanked,
    }))
}

/// Extract the category from a tier-2 shim file, if the shim is present and has one.
fn extract_category(shim_path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(shim_path).ok()?;
    let value: toml::Value = toml::from_str(&content).ok()?;

    value
        .get("metadata")?
        .get("category")?
        .as_str()
        .map(|s| s.to_string())
}
