//! `harbour flags` command
//!
//! # Why this is written the way it is
//!
//! This command exists to be *authoritative*: `MANIFEST.md` points at it
//! for "what the compiler and linker receive, without building". It used to
//! read a second, hand-maintained copy of the surface fold, and it had
//! drifted from the real one in four ways -- so every one of those was the
//! command telling the user something the compiler never saw.
//!
//! It is now built the way `harbour linkplan` derives its link line: the
//! flag *list* comes from the same call the build plan makes, and the
//! attribution is looked up alongside it. Nothing here recomputes what a
//! flag should be. If the fold changes, this output changes with it,
//! because there is nothing else for it to read.
//!
//! ## The one deliberate difference, and what makes it safe
//!
//! The compiler also receives, per translation unit, flags that are not
//! part of any manifest's surface: the profile's own (`-O`, `-g`, `NDEBUG`,
//! sanitizers) and, for a C++ source, the language options (`-std=`,
//! `-fno-exceptions`, `-fno-rtti`, `-stdlib=`). The profile's are printed
//! here, attributed to the profile, because they are the same for every
//! file. The C++ language options are not, because they are per-file and
//! depend on the graph-wide C++ standard; a C source in the same target
//! does not get them.
//!
//! A C target's own `c_std` *is* printed, because unlike the C++ options it
//! is a per-target setting written in this manifest, and this is the
//! command an author checks it with. It carries the same caveat in the
//! other direction: an assembly source in the same target is compiled
//! without it, since `-std=` describes a C dialect. On MSVC, where `cl` has
//! no `/std:` for C89/C99/C23, the build warns and drops it while this
//! command still prints the GCC spelling -- which is why the parity test
//! below is `cfg(not(windows))`.
//!
//! `tests/cli_integration.rs::test_flags_matches_the_real_compile_command`
//! pins this: it captures the real argv the compiler is handed and asserts
//! that it is exactly this command's output plus the source and output
//! operands. That test is the reason the boundary above can be trusted
//! rather than merely asserted.

use anyhow::Result;

use crate::cli::FlagsArgs;
use harbour::builder::surface_resolver::{
    Provenance, SurfaceKind, SurfaceResolver, WithProvenance,
};
use harbour::builder::BuildContext;
use harbour::core::target::TargetTriple;
use harbour::core::Workspace;
use harbour::ops::resolve::resolve_workspace;
use harbour::sources::SourceCache;
use harbour::util::config::load_config;
use harbour::util::GlobalContext;
use harbour::util::VcpkgIntegration;

/// Where a flag came from, for the `# from:` comment.
///
/// Not every flag on the command line comes from a manifest surface, so
/// this is deliberately wider than [`Provenance`]: a label that says
/// `vcpkg` or `profile debug` is the honest answer, and inventing a package
/// for it would be the kind of small lie this command is being fixed for.
enum Origin<'a> {
    Surface(&'a Provenance),
    Label(String),
}

impl std::fmt::Display for Origin<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Origin::Surface(p) => write!(f, "{p}"),
            Origin::Label(l) => write!(f, "{l}"),
        }
    }
}

/// An attributed flag, ready to print.
struct Attributed<'a> {
    flag: String,
    origin: Origin<'a>,
}

pub fn execute(args: FlagsArgs) -> Result<()> {
    let ctx = GlobalContext::new()?;

    let manifest_path = ctx.find_manifest()?;

    let ws = Workspace::new(&manifest_path, &ctx)?;

    let config = load_config(
        &ctx.config_path(),
        &ctx.project_harbour_dir().join("config.toml"),
    );
    let vcpkg = VcpkgIntegration::from_config(&config.vcpkg, &TargetTriple::host(), false);
    let mut source_cache = SourceCache::new_with_vcpkg(ctx.cache_dir(), vcpkg)
        .with_default_registry(ctx.default_registry_url().as_str());

    let resolve = resolve_workspace(&ws, &mut source_cache)?;

    let profile = args.profile_name();
    let build_ctx = BuildContext::new_with_vcpkg(&ws, &profile, &config.vcpkg, None)?;

    // Create surface resolver
    let mut surface_resolver = SurfaceResolver::new(&resolve, &build_ctx.platform);
    surface_resolver.load_packages(&mut source_cache)?;

    // Find the target
    let root_pkg = ws.root_package();
    let target = root_pkg.target(&args.target).ok_or_else(|| {
        anyhow::anyhow!(
            "target `{}` not found\n\
             help: Run `harbour tree` to see available targets",
            args.target
        )
    })?;

    // The attributed fold. `strip_provenance` on either of these is exactly
    // what the build plan resolves, so the flag list below is the build's
    // and the attribution comes along for free.
    let compile_surface =
        surface_resolver.resolve_compile_surface_with_provenance(ws.root_package_id(), target)?;
    let link_surface = surface_resolver.resolve_link_surface_with_provenance(
        ws.root_package_id(),
        target,
        &build_ctx.deps_dir,
    )?;

    // vcpkg's directories are folded in by the same method the plan uses,
    // so they land in the same position and take part in the same
    // deduplication.
    let mut compile_surface = compile_surface;
    let mut plain_compile = compile_surface.strip_provenance();
    let mut plain_link = link_surface.strip_provenance();
    build_ctx.merge_vcpkg_dirs(&mut plain_compile, &mut plain_link);

    // Probe answers, measured the same way and in the same place the build
    // measures them -- `answer_for_target` is the only implementation, and
    // it keys its cache on a path derived from the context rather than from
    // the build plan's output layout, so this shares the build's cache
    // instead of keeping a second one that could disagree.
    //
    // This command is documented as authoritative about what the compiler
    // receives, and §2.4 of the 2026-09-07 audit is four separate instances
    // of it not being. A probe define reaching the compiler but not this
    // listing would be the fifth.
    //
    // Probes are measured against the *pre-probe* surface, exactly as in
    // `BuildPlan::with_root_packages`, which is why this happens after the
    // fold and before the flags are printed.
    let probe_results = harbour::builder::probe::answer_for_target(
        &build_ctx,
        &ws.root_package_id(),
        target,
        &plain_compile,
    )?;
    match probe_results.contribution(&target.probes) {
        harbour::builder::probe::ProbeContribution::Defines(defs) => {
            for define in defs {
                compile_surface.defines.push(WithProvenance::new(
                    define.clone(),
                    ws.root_package_id(),
                    SurfaceKind::Probe,
                ));
                plain_compile.defines.push(define);
            }
        }
        harbour::builder::probe::ProbeContribution::IncludeDir(dir) => {
            // Front-inserted, matching `BuildPlan`: `-I` is
            // first-match-wins, so where it goes is part of what the
            // compiler receives, and printing it in the wrong position
            // would make this command wrong in a way that is easy to miss.
            compile_surface.include_dirs.insert(
                0,
                WithProvenance::new(dir.clone(), ws.root_package_id(), SurfaceKind::Probe),
            );
            plain_compile.include_dirs.insert(0, dir);
        }
    }

    if !args.link {
        println!("# Compile flags for `{}`:", args.target);
        for item in compile_flags(
            &compile_surface,
            &plain_compile,
            &build_ctx,
            &profile,
            target,
        )? {
            println!("  {}    # from: {}", item.flag, item.origin);
        }
    }

    if !args.compile && !args.link {
        println!();
    }

    if !args.compile {
        println!("# Link flags for `{}`:", args.target);
        for item in link_flags(&link_surface, &plain_link, &build_ctx, &profile)? {
            println!("  {}    # from: {}", item.flag, item.origin);
        }
    }

    Ok(())
}

/// The compile flags, in command-line order, each attributed.
///
/// Order is `to_flags`' order with the profile's flags spliced in ahead of
/// the surface's `cflags` -- which is where the toolchain puts them, and
/// deliberately so: `cflags` are last-wins, so a manifest's `-O2` has to be
/// able to beat the profile's `-O0`.
fn compile_flags<'a>(
    attributed: &'a harbour::builder::surface_resolver::EffectiveCompileSurfaceWithProvenance,
    authoritative: &harbour::builder::surface_resolver::EffectiveCompileSurface,
    ctx: &BuildContext,
    profile: &str,
    target: &harbour::core::target::Target,
) -> Result<Vec<Attributed<'a>>> {
    let mut out = Vec::new();

    // The target's `c_std`, first, because that is where the toolchain puts
    // it -- ahead of the include dirs, so a `-std=` in `cflags` can still
    // override it.
    //
    // Printed here even though the graph-wide C++ language options are not
    // (see the module docs): this one is a property of *this target*, named
    // in *this* manifest, and `harbour flags` is the command a manifest
    // author checks it with. The caveat is the same one the compiler has:
    // only C sources get it, so an assembly source in this target is
    // compiled without it.
    if target.lang == harbour::core::target::Language::C {
        if let Some(c_std) = target.c_std {
            out.push(Attributed {
                flag: format!("-std={}", c_std.as_flag_value()),
                origin: Origin::Label(format!("target {} (c_std)", target.name)),
            });
        }
    }

    // `merge_vcpkg_dirs` only ever *appends* to `include_dirs` (vcpkg's
    // directories are a fallback and belong last on a first-match-wins
    // search path), and deduplication drops the vcpkg copy rather than the
    // surface's, so the attributed list is a prefix of the authoritative
    // one. Asserted rather than assumed: if that ever stops holding, the
    // attributions would silently slide by one.
    debug_assert!(
        authoritative.include_dirs.len() >= attributed.include_dirs.len()
            && attributed
                .include_dirs
                .iter()
                .zip(&authoritative.include_dirs)
                .all(|(a, b)| &a.value == b),
        "the attributed include dirs must be a prefix of the authoritative ones"
    );

    for item in &attributed.include_dirs {
        out.push(Attributed {
            flag: format!("-I{}", item.value.display()),
            origin: Origin::Surface(&item.provenance),
        });
    }
    for dir in authoritative
        .include_dirs
        .iter()
        .skip(attributed.include_dirs.len())
    {
        out.push(Attributed {
            flag: format!("-I{}", dir.display()),
            origin: Origin::Label("vcpkg".to_string()),
        });
    }

    for item in &attributed.defines {
        out.push(Attributed {
            flag: item.value.to_flag(),
            origin: Origin::Surface(&item.provenance),
        });
    }

    for flag in ctx.profile_cflags()? {
        out.push(Attributed {
            flag,
            origin: Origin::Label(format!("profile {profile}")),
        });
    }

    for item in &attributed.cflags {
        out.push(Attributed {
            flag: item.value.clone(),
            origin: Origin::Surface(&item.provenance),
        });
    }

    Ok(out)
}

/// The link flags, in command-line order, each attributed.
///
/// Dependency archives come before the search paths and `-lNAME`, because
/// that is where the linker driver gets them: `NativeBuilder` appends them
/// to the object files, so a single-pass static linker sees the consumer of
/// a symbol before the archive defining it.
fn link_flags<'a>(
    attributed: &'a harbour::builder::surface_resolver::EffectiveLinkSurfaceWithProvenance,
    authoritative: &harbour::builder::surface_resolver::EffectiveLinkSurface,
    ctx: &BuildContext,
    profile: &str,
) -> Result<Vec<Attributed<'a>>> {
    let mut out = Vec::new();

    // Same prefix relationship as `compile_flags`, for the same reason.
    debug_assert!(
        authoritative.lib_dirs.len() >= attributed.lib_dirs.len()
            && attributed
                .lib_dirs
                .iter()
                .zip(&authoritative.lib_dirs)
                .all(|(a, b)| &a.value == b),
        "the attributed lib dirs must be a prefix of the authoritative ones"
    );

    for item in &attributed.dep_libs {
        out.push(Attributed {
            flag: item.value.display().to_string(),
            origin: Origin::Surface(&item.provenance),
        });
    }

    for item in &attributed.lib_dirs {
        out.push(Attributed {
            flag: format!("-L{}", item.value.display()),
            origin: Origin::Surface(&item.provenance),
        });
    }
    for dir in authoritative
        .lib_dirs
        .iter()
        .skip(attributed.lib_dirs.len())
    {
        out.push(Attributed {
            flag: format!("-L{}", dir.display()),
            origin: Origin::Label("vcpkg".to_string()),
        });
    }

    for item in &attributed.libs {
        for flag in item.value.to_flags() {
            out.push(Attributed {
                flag,
                origin: Origin::Surface(&item.provenance),
            });
        }
    }

    // One line per *option*, so a two-token flag keeps its operand next to
    // it rather than on a line of its own attributed to nothing.
    for item in &attributed.frameworks {
        out.push(Attributed {
            flag: format!("-framework {}", item.value),
            origin: Origin::Surface(&item.provenance),
        });
    }

    for flag in ctx.profile_ldflags()? {
        out.push(Attributed {
            flag,
            origin: Origin::Label(format!("profile {profile}")),
        });
    }

    for item in &attributed.ldflags {
        out.push(Attributed {
            flag: item.value.clone(),
            origin: Origin::Surface(&item.provenance),
        });
    }

    Ok(out)
}
