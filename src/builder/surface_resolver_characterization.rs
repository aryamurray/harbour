//! Characterization of the fold that the **builder** uses.
//!
//! This exists to make the unification of the plain and provenance folds
//! provable rather than asserted. [`SurfaceResolver::resolve_compile_surface`]
//! and [`SurfaceResolver::resolve_link_surface`] are what `plan.rs` feeds to
//! the compiler and linker, so their output is the reference behaviour: it is
//! pinned here, flag for flag and in order, over a graph that exercises
//! public and private target deps, an explicitly named dependency target, a
//! transitive dependency reached only through a static lib, features, a
//! target-level `[[targets.X.when]]` block and a surface-level
//! `[[targets.X.surface.when]]` block at once.
//!
//! If a change to the fold moves a single flag, this test says so.

use super::*;
use crate::core::manifest::Manifest;
use crate::core::package::Package;
use crate::core::source_id::SourceId;
use crate::util::context::DEFAULT_REGISTRY_URL;
use tempfile::TempDir;

/// A fixed platform, so no assertion here depends on the machine running
/// the test: `os`/`arch`/`compiler` are what the manifest conditions below
/// are written against.
pub(super) fn platform() -> TargetPlatform {
    TargetPlatform {
        build_target: None,
        os: "linux".to_string(),
        arch: "x86_64".to_string(),
        env: Some("gnu".to_string()),
        compiler: Some("gcc".to_string()),
    }
}

fn write_pkg(tmp: &Path, name: &str, toml: &str) -> Package {
    let dir = tmp.join(name);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("src").join("main.c"),
        "int main(void){return 0;}\n",
    )
    .unwrap();
    std::fs::write(dir.join("Harbour.toml"), toml).unwrap();
    let manifest = Manifest::load(&dir.join("Harbour.toml")).unwrap();
    let source = SourceId::for_path(&dir).unwrap();
    Package::with_source_id(manifest, dir, source).unwrap()
}

/// `app` -> `base` (explicit `target = "base"`, two libs in the package),
/// `app` -> `hidden` (`compile = "private"`), `app` -> `secret`
/// (`link = "private"`), and `base` -> `core`, so `core` is reached only
/// transitively through a static lib.
pub(super) struct Fixture {
    _tmp: TempDir,
    pub(super) root: PathBuf,
    resolve: Resolve,
    packages: HashMap<PackageId, Package>,
    pub(super) app_id: PackageId,
}

pub(super) fn fixture() -> Fixture {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();

    // No defines on `core`: it is the second contributor of public compile
    // requirements, and `Resolve::transitive_deps` returns a `HashSet`, so
    // the *relative* order of two dependencies' contributions is not
    // deterministic today. `include_dirs` and `cflags` are sorted by the
    // fold so they are stable regardless; `defines` are not, so exactly one
    // dependency contributes them.
    let core = write_pkg(
        &root,
        "core",
        r#"[package]
name = "core"
version = "2.0.0"

[targets.core]
kind = "staticlib"
sources = ["src/main.c"]

[targets.core.surface.compile.public]
include_dirs = ["include"]
cflags = ["-fcore-public"]

[targets.core.surface.compile.private]
cflags = ["-fcore-private-never-propagates"]

[targets.core.surface.link.public]
libs = ["m"]
ldflags = ["-Wl,-core"]
frameworks = ["CoreFoundation"]
"#,
    );

    let base = write_pkg(
        &root,
        "base",
        r#"[package]
name = "base"
version = "1.0.0"

[features]
extra = []

[dependencies]
core = { path = "../core" }

[targets.base]
kind = "staticlib"
sources = ["src/main.c"]

[targets.base.surface.compile.public]
defines = ["BASE_PUBLIC=1"]
include_dirs = ["include"]
cflags = ["-fbase-public"]

[targets.base.surface.compile.private]
defines = ["BASE_PRIVATE=1"]

[[targets.base.surface.when]]
feature = "extra"
[targets.base.surface.when."compile.public"]
defines = ["BASE_EXTRA=1"]

[targets.base_other]
kind = "staticlib"
sources = ["src/main.c"]

[targets.base_other.surface.compile.public]
defines = ["BASE_OTHER_TARGET=1"]
"#,
    );

    let hidden = write_pkg(
        &root,
        "hidden",
        r#"[package]
name = "hidden"
version = "0.1.0"

[targets.hidden]
kind = "staticlib"
sources = ["src/main.c"]

[targets.hidden.surface.compile.public]
defines = ["HIDDEN_PUBLIC=1"]
cflags = ["-fhidden-public"]

[targets.hidden.surface.link.public]
libs = ["dl"]
"#,
    );

    let secret = write_pkg(
        &root,
        "secret",
        r#"[package]
name = "secret"
version = "0.1.0"

[targets.secret]
kind = "staticlib"
sources = ["src/main.c"]

[targets.secret.surface.compile.public]
include_dirs = ["include"]
cflags = ["-fsecret-public"]

[targets.secret.surface.link.public]
libs = ["pthread"]
ldflags = ["-Wl,-secret"]
"#,
    );

    let app = write_pkg(
        &root,
        "app",
        r#"[package]
name = "app"
version = "0.1.0"

[dependencies]
base = { path = "../base", features = ["extra"] }
hidden = { path = "../hidden" }
secret = { path = "../secret" }

[targets.app]
kind = "exe"
sources = ["src/main.c"]

[targets.app.deps]
base = { target = "base" }
hidden = { compile = "private", link = "public" }
secret = { compile = "public", link = "private" }

[targets.app.surface.compile.private]
defines = ["APP_PRIVATE=1"]
include_dirs = ["private_inc"]
cflags = ["-Wall", "-Wno-error", "-Werror"]

[targets.app.surface.compile.public]
defines = ["APP_PUBLIC=1"]
include_dirs = ["include"]

[targets.app.surface.link.private]
ldflags = ["-Wl,-app-private"]

[[targets.app.when]]
os = "linux"
defines = ["APP_WHEN_LINUX=1"]
cflags = ["-fapp-when"]
include_dirs = ["linux_inc"]

[[targets.app.when]]
os = "macos"
defines = ["APP_WHEN_MACOS=1"]

[[targets.app.surface.when]]
os = "linux"
[targets.app.surface.when."compile.private"]
cflags = ["-fsurface-when-private"]
[targets.app.surface.when."link.private"]
ldflags = ["-Wl,--as-needed"]

[[targets.app.surface.when]]
arch = "riscv32"
[targets.app.surface.when."compile.private"]
cflags = ["-fnever-matches"]
"#,
    );

    let (core_id, base_id) = (core.package_id(), base.package_id());
    let (hidden_id, secret_id) = (hidden.package_id(), secret.package_id());
    let app_id = app.package_id();

    let mut resolve = Resolve::new();
    for pkg in [&core, &base, &hidden, &secret, &app] {
        resolve.add_package(pkg.package_id(), pkg.summary(DEFAULT_REGISTRY_URL).unwrap());
    }
    resolve.add_edge(app_id, base_id);
    resolve.add_edge(app_id, hidden_id);
    resolve.add_edge(app_id, secret_id);
    resolve.add_edge(base_id, core_id);

    let mut packages = HashMap::new();
    for pkg in [core, base, hidden, secret, app] {
        packages.insert(pkg.package_id(), pkg);
    }

    Fixture {
        _tmp: tmp,
        root,
        resolve,
        packages,
        app_id,
    }
}

/// Build a resolver over the fixture. Fields are set directly rather than
/// through `load_packages`, which would need a `SourceCache` for a graph
/// that is entirely local and already loaded.
pub(super) fn resolver<'a>(fx: &'a Fixture, platform: &'a TargetPlatform) -> SurfaceResolver<'a> {
    let mut r = SurfaceResolver::new(&fx.resolve, platform);
    r.packages = fx.packages.clone();
    r.features = compute_feature_sets(
        &fx.resolve,
        &r.packages,
        crate::builder::surface_resolver::FeaturePhase::Build,
    )
    .unwrap();
    r
}

pub(super) fn app_target(fx: &Fixture) -> &Target {
    fx.packages[&fx.app_id].target("app").unwrap()
}

/// Render with the temp root replaced by `<root>` so the expectation can be
/// written literally.
pub(super) fn normalize(flags: &[String], root: &Path) -> Vec<String> {
    // Deliberately *not* canonicalized: the fold joins onto the
    // uncanonicalized package root, and on macOS `/var` is a symlink to
    // `/private/var`, so canonicalizing here would stop the prefix matching
    // the paths under test.
    //
    // Separators are folded to `/` afterwards. These expectations pin the
    // *structure* of the fold's output -- which paths, in which order -- and
    // that is identical on every platform; only the separator differs. Pinning
    // the separator too would mean maintaining two copies of every expectation
    // to assert nothing extra.
    let root = root.display().to_string();
    flags
        .iter()
        .map(|f| f.replace(&root, "<root>").replace('\\', "/"))
        .collect()
}

#[test]
fn plain_compile_fold_output_is_pinned() {
    let platform = platform();
    let fx = fixture();
    let r = resolver(&fx, &platform);
    let surface = r
        .resolve_compile_surface(fx.app_id, app_target(&fx))
        .unwrap();

    assert_eq!(
        normalize(&surface.to_flags(), &fx.root),
        vec![
            // Fold order, not ASCII order. `-I` is first-match-wins, so
            // this target's own directories come before any dependency's:
            // private, then the matching `[[targets.app.when]]` block, then
            // public, then dependencies in reverse-topological order.
            "-I<root>/app/private_inc",
            "-I<root>/app/linux_inc",
            "-I<root>/app/include",
            "-I<root>/base/include",
            "-I<root>/core/include",
            "-I<root>/secret/include",
            "-DAPP_PRIVATE=1",
            "-DAPP_WHEN_LINUX=1",
            "-DAPP_PUBLIC=1",
            "-DBASE_PUBLIC=1",
            "-DBASE_EXTRA=1",
            // The whole point of this change: the manifest says
            // `["-Wall", "-Wno-error", "-Werror"]` and the compiler is
            // handed exactly that, so `-Werror` wins. Sorted, `-Wno-error`
            // came last and the author's `-Werror` did nothing.
            "-Wall",
            "-Wno-error",
            "-Werror",
            "-fsurface-when-private",
            "-fapp-when",
            "-fbase-public",
            "-fcore-public",
            "-fsecret-public",
        ],
    );
}

#[test]
fn plain_link_fold_output_is_pinned() {
    let platform = platform();
    let fx = fixture();
    let r = resolver(&fx, &platform);
    let deps_dir = fx.root.join("deps");
    let surface = r
        .resolve_link_surface(fx.app_id, app_target(&fx), &deps_dir, &fx.root.join("lib"))
        .unwrap();

    assert_eq!(
        normalize(&surface.to_flags(), &fx.root),
        vec![
            // Link order, dependents before dependencies: `core` is only
            // reachable through `base`, so it must follow it. `secret` is
            // absent: `link = "private"` on the target dep. No `-L` for
            // any of the three -- see the comment in `resolve_link_surface`
            // about a dependency named `c` shadowing the system libc.
            "<root>/deps/base-1.0.0/lib/libbase.a",
            "<root>/deps/core-2.0.0/lib/libcore.a",
            "<root>/deps/hidden-0.1.0/lib/libhidden.a",
            "-lm",
            "-ldl",
            "-framework",
            "CoreFoundation",
            // Fold order: this target's own private ldflags, then the
            // matching `surface.when` block, then dependencies.
            "-Wl,-app-private",
            "-Wl,--as-needed",
            "-Wl,-core",
        ],
    );
}

/// The three drifts that the separate provenance fold had, now closed by
/// there being one fold. Each was a thing `harbour flags` told the user
/// that the compiler or linker never saw.
mod one_fold_closes_the_drift {
    use super::*;

    /// `hidden = { compile = "private" }`: its public compile surface must
    /// not appear. The old provenance fold had no visibility check at all,
    /// so `harbour flags` listed flags the compile of `app`'s own sources
    /// never received.
    #[test]
    fn compile_private_on_a_target_dep_is_honoured() {
        let platform = platform();
        let fx = fixture();
        let r = resolver(&fx, &platform);
        let surface = r
            .resolve_compile_surface_with_provenance(fx.app_id, app_target(&fx))
            .unwrap();
        let flags = surface.to_flags();

        assert!(
            !flags.iter().any(|f| f.contains("HIDDEN_PUBLIC")),
            "compile = \"private\" must suppress the dep's public defines: {flags:?}"
        );
        assert!(
            !flags.iter().any(|f| f == "-fhidden-public"),
            "compile = \"private\" must suppress the dep's public cflags: {flags:?}"
        );
    }

    /// `base = { target = "base" }` in a package that has two library
    /// targets. The old provenance fold called `default_target()`, which is
    /// "the first library target" of an unordered map -- a coin flip per
    /// process, so the answer flapped run to run.
    #[test]
    fn an_explicitly_named_dep_target_is_honoured() {
        let platform = platform();
        let fx = fixture();
        let r = resolver(&fx, &platform);
        let surface = r
            .resolve_compile_surface_with_provenance(fx.app_id, app_target(&fx))
            .unwrap();
        let flags = surface.to_flags();

        assert!(
            flags.iter().any(|f| f == "-DBASE_PUBLIC=1"),
            "the named target's surface must be present: {flags:?}"
        );
        assert!(
            !flags.iter().any(|f| f.contains("BASE_OTHER_TARGET")),
            "the package's other library target must not contribute: {flags:?}"
        );
    }

    /// The link fold emits a dependency archive by absolute path and
    /// deliberately no matching `-L`; see the comment in
    /// `resolve_link_surface_with_provenance`. The old provenance fold
    /// pushed the `-L` anyway, so `harbour flags` advertised a search path
    /// whose *absence* is the safety property.
    #[test]
    fn no_search_path_is_reported_for_a_dependency_archive() {
        let platform = platform();
        let fx = fixture();
        let r = resolver(&fx, &platform);
        let surface = r
            .resolve_link_surface_with_provenance(
                fx.app_id,
                app_target(&fx),
                &fx.root.join("deps"),
                &fx.root.join("lib"),
            )
            .unwrap();

        assert!(
            surface.lib_dirs.is_empty(),
            "no -L may be invented for an archive passed by absolute path: {:?}",
            surface
                .lib_dirs
                .iter()
                .map(|d| d.value.display().to_string())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            surface.dep_libs.len(),
            3,
            "but the archives themselves are still there"
        );
    }
}

/// The deduplication that replaced the sort. Both directions are tested
/// because the choice between them is per-field and argued in
/// `dedup_for_build`, not a coin toss.
mod order_preserving_dedup {
    use super::*;

    #[test]
    fn keeping_first_leaves_the_earliest_position() {
        let mut v = vec!["a", "b", "a", "c", "b"];
        dedup_keeping_first(&mut v);
        assert_eq!(v, vec!["a", "b", "c"]);
    }

    #[test]
    fn keeping_last_leaves_the_latest_position() {
        let mut v = vec!["a", "b", "a", "c", "b"];
        dedup_keeping_last(&mut v);
        assert_eq!(v, vec!["a", "c", "b"]);
    }

    /// The distinction that matters: a flag re-asserted after its negation
    /// must end up after it, or last-wins does not hold.
    #[test]
    fn keeping_last_preserves_a_reasserted_flag_winning() {
        let mut v = vec!["-Werror", "-Wno-error", "-Werror"];
        dedup_keeping_last(&mut v);
        assert_eq!(
            v,
            vec!["-Wno-error", "-Werror"],
            "the author re-asserted -Werror last, so it must win"
        );

        let mut v = vec!["-Werror", "-Wno-error", "-Werror", "-Wno-error"];
        dedup_keeping_last(&mut v);
        assert_eq!(
            v,
            vec!["-Werror", "-Wno-error"],
            "and the other way round when -Wno-error is last"
        );
    }

    /// Neither helper needs sorted input, and neither may reorder anything
    /// it keeps -- the whole point, since `Vec::dedup` alone only removes
    /// *adjacent* duplicates and the sort that used to make them adjacent
    /// is what destroyed the meaning.
    #[test]
    fn non_adjacent_duplicates_are_removed_without_sorting() {
        let mut v = vec!["-Wall", "-fPIC", "-Wall", "-O2", "-fPIC"];
        dedup_keeping_first(&mut v);
        assert_eq!(v, vec!["-Wall", "-fPIC", "-O2"]);

        let mut naive = vec!["-Wall", "-fPIC", "-Wall", "-O2", "-fPIC"];
        naive.dedup();
        assert_eq!(
            naive.len(),
            5,
            "`Vec::dedup` on its own removes nothing here, which is why the \
             old code had to sort first"
        );
    }
}

/// A dependency exposing several libraries, consumed through `targets = [...]`
/// and the `local = true` edges between them.
mod several_archives_per_dependency {
    use super::*;

    /// `ossl` declares `ssl`, `extra`, `crypto` (in that order); `ssl` needs
    /// `crypto` (`local = true`), `extra` needs nothing, and `gen` is a host
    /// tool. `package_head` is spliced into `[package]` (a `default_target`).
    ///
    /// `app` depends on `ossl` directly with `app_entry` as its `deps` entry,
    /// or, when `with_mid` is set, on `mid`, which depends on `ossl` and is
    /// the only one that names targets: `app -> mid -> ossl`.
    fn multi(package_head: &str, app_entry: &str, with_mid: bool) -> Fixture {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();

        let ossl = write_pkg(
            &root,
            "ossl",
            &format!(
                r#"[package]
name = "ossl"
version = "3.0.0"
{package_head}

[targets.ssl]
kind = "staticlib"
sources = ["src/main.c"]

[targets.ssl.surface.compile.public]
include_dirs = ["include/ssl"]

[targets.ssl.surface.link.public]
ldflags = ["-Wl,-ssl"]

[targets.ssl.deps]
crypto = {{ local = true }}

[targets.extra]
kind = "staticlib"
sources = ["src/main.c"]

[targets.crypto]
kind = "staticlib"
sources = ["src/main.c"]

[targets.crypto.surface.compile.public]
include_dirs = ["include/crypto"]

[targets.crypto.surface.link.public]
ldflags = ["-Wl,-crypto"]

[targets.gen]
kind = "exe"
host = true
sources = ["src/main.c"]
"#
            ),
        );

        let mid = with_mid.then(|| {
            write_pkg(
                &root,
                "mid",
                r#"[package]
name = "mid"
version = "1.0.0"

[dependencies]
ossl = { path = "../ossl" }

[targets.mid]
kind = "staticlib"
sources = ["src/main.c"]

[targets.mid.deps]
ossl = { targets = ["ssl", "crypto"] }
"#,
            )
        });

        let (dep, entry) = if with_mid {
            ("mid", "{ }")
        } else {
            ("ossl", app_entry)
        };
        let app = write_pkg(
            &root,
            "app",
            &format!(
                r#"[package]
name = "app"
version = "0.1.0"

[dependencies]
{dep} = {{ path = "../{dep}" }}

[targets.app]
kind = "exe"
sources = ["src/main.c"]

[targets.app.deps]
{dep} = {entry}
"#
            ),
        );

        let mut resolve = Resolve::new();
        let app_id = app.package_id();
        let ossl_id = ossl.package_id();
        let mut all = vec![&ossl, &app];
        if let Some(mid) = &mid {
            all.push(mid);
        }
        for pkg in &all {
            resolve.add_package(pkg.package_id(), pkg.summary(DEFAULT_REGISTRY_URL).unwrap());
        }
        match &mid {
            Some(mid) => {
                resolve.add_edge(app_id, mid.package_id());
                resolve.add_edge(mid.package_id(), ossl_id);
            }
            None => resolve.add_edge(app_id, ossl_id),
        }

        let mut packages = HashMap::new();
        for pkg in [Some(ossl), mid, Some(app)].into_iter().flatten() {
            packages.insert(pkg.package_id(), pkg);
        }
        Fixture {
            _tmp: tmp,
            root,
            resolve,
            packages,
            app_id,
        }
    }

    fn link_surface(fx: &Fixture) -> Result<EffectiveLinkSurface> {
        let platform = platform();
        resolver(fx, &platform).resolve_link_surface(
            fx.app_id,
            app_target(fx),
            &fx.root.join("deps"),
            &fx.root.join("lib"),
        )
    }

    /// The archives on `app`'s link line, in order, as file names.
    fn archives(fx: &Fixture) -> Vec<String> {
        link_surface(fx)
            .unwrap()
            .dep_libs
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    fn include_dirs(fx: &Fixture) -> Vec<String> {
        let platform = platform();
        let surface = resolver(fx, &platform)
            .resolve_compile_surface(fx.app_id, app_target(fx))
            .unwrap();
        normalize(
            &surface
                .include_dirs
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>(),
            &fx.root,
        )
    }

    /// The dependent comes first whatever order the names were written in,
    /// and the compile and link surfaces of both libraries arrive once each,
    /// in that same order.
    #[test]
    fn targets_are_linked_dependents_first_whatever_order_they_are_listed_in() {
        for list in [r#"["ssl", "crypto"]"#, r#"["crypto", "ssl"]"#] {
            let fx = multi("", &format!("{{ targets = {list} }}"), false);
            assert_eq!(archives(&fx), ["libssl.a", "libcrypto.a"], "{list}");
            assert_eq!(
                include_dirs(&fx),
                ["<root>/ossl/include/ssl", "<root>/ossl/include/crypto"],
                "{list}"
            );
            assert_eq!(
                link_surface(&fx).unwrap().ldflags,
                ["-Wl,-ssl", "-Wl,-crypto"],
                "{list}"
            );
        }
    }

    /// Naming `ssl` alone gets `crypto` through the `local` edge, after it.
    #[test]
    fn a_single_target_still_brings_its_sibling_closure() {
        let fx = multi("", r#"{ target = "ssl" }"#, false);
        assert_eq!(archives(&fx), ["libssl.a", "libcrypto.a"]);
        // `targets = ["ssl"]` is the list spelling of the same request.
        let fx = multi("", r#"{ targets = ["ssl"] }"#, false);
        assert_eq!(archives(&fx), ["libssl.a", "libcrypto.a"]);
    }

    /// Targets with no edge between them keep declaration order (`ssl`,
    /// `extra`, `crypto`) however the manifest lists them, and `crypto` is
    /// not duplicated when it is both named and pulled in by `ssl`.
    #[test]
    fn unrelated_targets_keep_declaration_order() {
        for list in [r#"["crypto", "extra", "ssl"]"#, r#"["extra", "ssl"]"#] {
            let fx = multi("", &format!("{{ targets = {list} }}"), false);
            assert_eq!(
                archives(&fx),
                ["libssl.a", "libextra.a", "libcrypto.a"],
                "{list}"
            );
        }
    }

    /// The package's host tool is an executable: it is in no closure, and
    /// naming it in `targets` is refused along with an unknown name.
    #[test]
    fn an_unknown_or_non_linkable_name_is_refused_with_what_is_available() {
        let fx = multi("", r#"{ targets = ["ssl"] }"#, false);
        assert!(!archives(&fx).iter().any(|a| a.contains("gen")));

        for (list, needle) in [
            (r#"["ssl", "nope"]"#, "target `nope` not found in `ossl`"),
            (r#"["ssl", "gen"]"#, "cannot be listed in `targets`"),
        ] {
            let fx = multi("", &format!("{{ targets = {list} }}"), false);
            let err = link_surface(&fx).unwrap_err().to_string();
            assert!(err.contains(needle), "{list}: {err}");
            assert!(
                err.contains("crypto") && err.contains("ssl"),
                "the error must list what is available: {err}"
            );
        }
    }

    /// `app -> mid -> ossl`, `mid` selecting `[ssl, crypto]` and the package
    /// defaulting to `extra`: `app` names nothing, yet its link line carries
    /// the archives `mid` asked for, after `mid`'s own.
    #[test]
    fn a_transitive_consumer_gets_what_its_dependency_selected() {
        let fx = multi(r#"default_target = "extra""#, "", true);
        assert_eq!(archives(&fx), ["libmid.a", "libssl.a", "libcrypto.a"]);
        assert_eq!(
            include_dirs(&fx),
            ["<root>/ossl/include/ssl", "<root>/ossl/include/crypto"]
        );
    }

    /// The default-target fallback, which is what the multi-library warning
    /// is attached to, is taken only by a dependent that names nothing;
    /// naming any (`target` or `targets`) skips it. (Whether the warning is
    /// then printed is decided after this bookkeeping, and the dependency
    /// naming its own default suppresses it there.)
    #[test]
    fn the_fallback_is_for_a_dependent_that_names_nothing() {
        let platform = platform();
        let warned = |package_head: &str, entry: &str| {
            let fx = multi(package_head, entry, false);
            let r = resolver(&fx, &platform);
            r.resolve_link_surface(
                fx.app_id,
                app_target(&fx),
                &fx.root.join("deps"),
                &fx.root.join("lib"),
            )
            .unwrap();
            let warned = r.warned_multi_lib.borrow().len() == 1;
            warned
        };
        assert!(warned("", "{ }"));
        assert!(!warned("", r#"{ targets = ["ssl", "crypto"] }"#));
        assert!(!warned("", r#"{ target = "ssl" }"#));
    }
}
