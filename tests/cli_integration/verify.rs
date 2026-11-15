//! `harbour verify`: the consumer-shaped check of a registry package.
//!
//! `verify` is the merge gate for a registry that bots edit, so what these
//! pin is that it exercises the path a consumer exercises: the same fetch and
//! patch (once, cached), the same default library target and sibling closure,
//! the same generators. Every test publishes a real package into a real
//! `file://` registry and runs the real binary.
#![allow(unused_imports)]

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use tempfile::TempDir;

use harbour::test_support::fixtures::local_registry;

use crate::common::*;

/// A throwaway registry and the HOME the binary runs in.
struct Fixture {
    tmp: TempDir,
    home: PathBuf,
    registry: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = temp_dir();
        let home = harbour_home(&tmp);
        let registry = tmp.path().join("registry");
        local_registry::init(&registry).unwrap();
        Fixture {
            tmp,
            home,
            registry,
        }
    }

    /// Publish `name` 0.1.0. `files` are the upstream tree; `patches` are
    /// `(file, sha256)` pairs already written beside the shim; `metadata` is
    /// appended to the shim (a `[metadata.harness]` table, say). Returns the
    /// upstream directory.
    fn publish(
        &self,
        name: &str,
        files: &[(&str, &str)],
        patches: &[(String, String)],
        metadata: &str,
    ) -> PathBuf {
        let upstream = self.tmp.path().join(format!("upstream-{name}"));
        for (file, contents) in files {
            let path = upstream.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
        let rev = local_registry::commit_all(&upstream, "upstream").unwrap();
        let patch_refs: Vec<(&str, &str)> = patches
            .iter()
            .map(|(f, s)| (f.as_str(), s.as_str()))
            .collect();
        let shim_dir = local_registry::add_shim(
            &self.registry,
            name,
            "0.1.0",
            &local_registry::file_url(&upstream),
            &rev,
            &patch_refs,
        )
        .unwrap();
        if !metadata.is_empty() {
            let shim = shim_dir.join("0.1.0.toml");
            let mut text = fs::read_to_string(&shim).unwrap();
            text.push_str(metadata);
            fs::write(&shim, text).unwrap();
        }
        local_registry::publish(&self.registry, &self.tmp.path().join("scratch")).unwrap();
        upstream
    }

    /// `harbour verify <name> --registry-path <registry> --format json`.
    fn verify(&self, name: &str, extra: &[&str]) -> (RunLog, Value) {
        let registry = self.registry.display().to_string();
        let mut args = vec![
            "verify",
            name,
            "--registry-path",
            &registry,
            "--format",
            "json",
        ];
        args.extend_from_slice(extra);
        let run = harbour_run(&self.home, self.tmp.path(), &args);
        let json = serde_json::from_str(&run.stdout)
            .unwrap_or_else(|e| panic!("verify did not print JSON ({e})\n{run}"));
        (run, json)
    }

    fn verify_ok(&self, name: &str) -> Value {
        let (run, json) = self.verify(name, &[]);
        assert!(run.status.success(), "verify must pass\n{run}");
        assert_eq!(json["passed"], true, "{run}");
        json
    }
}

/// The step named `name` in a verify result.
fn step<'a>(json: &'a Value, name: &str) -> &'a Value {
    json["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == name)
        .unwrap_or_else(|| panic!("no `{name}` step in {json:#}"))
}

fn names(json: &Value, path: &[&str]) -> Vec<String> {
    let mut v = json;
    for p in path {
        v = &v[*p];
    }
    v.as_array()
        .unwrap_or_else(|| panic!("{path:?} is not a list in {json:#}"))
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect()
}

/// The simplest package: a manifest, one header, one source.
fn simple_files() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "Harbour.toml",
            "[package]\nname = \"simple\"\nversion = \"0.1.0\"\n\n\
             [targets.simple]\nkind = \"staticlib\"\nsources = [\"simple.c\"]\n\n\
             [targets.simple.surface.compile.public]\ninclude_dirs = [\".\"]\n",
        ),
        ("simple.h", "int simple_value(void);\n"),
        ("simple.c", "int simple_value(void) { return 7; }\n"),
    ]
}

/// As [`simple_files`], with the header in an `include/` directory of its own
/// rather than beside the sources, so every header in it is public.
fn simple_files_with_include_dir() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "Harbour.toml",
            "[package]\nname = \"simple\"\nversion = \"0.1.0\"\n\n\
             [targets.simple]\nkind = \"staticlib\"\nsources = [\"simple.c\"]\n\n\
             [targets.simple.surface.compile.public]\ninclude_dirs = [\"include\"]\n",
        ),
        ("include/simple.h", "int simple_value(void);\n"),
        ("simple.c", "int simple_value(void) { return 7; }\n"),
    ]
}

#[test]
fn verify_passes_for_a_simple_package_and_reports_what_it_checked() {
    let fx = Fixture::new();
    fx.publish("simple", &simple_files(), &[], "");

    let json = fx.verify_ok("simple");

    assert_eq!(json["package"], "simple");
    assert_eq!(json["version"], "0.1.0");
    assert!(
        !json["triple"].as_str().unwrap().is_empty(),
        "the triple verified is always reported: {json:#}"
    );
    assert!(json.get("failure").is_none(), "{json:#}");

    // Every stage is there, with a status and a duration a CI system can read.
    for name in ["Resolve", "Fetch", "Build", "Artifacts", "Harness"] {
        let s = step(&json, name);
        assert_eq!(s["status"], "passed", "{name}: {json:#}");
        assert!(s["duration"].is_u64(), "{name} has a duration: {json:#}");
    }

    // The translation units the package itself compiled.
    assert_eq!(json["translation_units"]["count"], 1);
    assert_eq!(names(&json, &["translation_units", "files"]), ["simple.c"]);

    // No `[metadata.harness]`, so the default one ran: it included the public
    // header and linked a symbol out of the archive.
    assert_eq!(json["harness"]["level"], "default");
    assert_eq!(names(&json, &["harness", "headers"]), ["simple.h"]);
    assert_eq!(json["harness"]["link_symbol"], "simple_value");
    assert_eq!(json["harness"]["executed"], true);
}

#[test]
fn verify_human_output_names_the_harness_level() {
    let fx = Fixture::new();
    fx.publish("simple", &simple_files(), &[], "");

    let registry = fx.registry.display().to_string();
    let run = harbour_run(
        &fx.home,
        fx.tmp.path(),
        &["verify", "simple", "--registry-path", &registry],
    )
    .success();
    let out = run.stdout;
    assert!(out.contains("Result: PASSED"), "{out}");
    assert!(out.contains("harness: default (headers + link)"), "{out}");
    assert!(out.contains("translation units in simple: 1"), "{out}");
}

/// The shim's patch ADDS `Harbour.toml` to the upstream tree. Applied twice,
/// `git apply` refuses ("already exists in working directory"), which is what
/// made the old `verify` fail on every run after the first.
///
/// The second run also proves the source cache is what is used: the upstream
/// repository and the patch file are taken away, so a second download or a
/// second patch application could not succeed.
#[test]
fn verify_passes_twice_on_one_cache_for_a_package_whose_patch_adds_the_manifest() {
    let fx = Fixture::new();
    let (patch, sha) = write_manifest_patch(&fx.registry, "lib", "0.1.0");
    let upstream = fx.publish(
        "lib",
        &[
            ("lib.h", "int lib_value(void);\n"),
            ("lib.c", "int lib_value(void) { return 42; }\n"),
        ],
        &[(patch.clone(), sha)],
        "",
    );

    let first = fx.verify_ok("lib");
    assert_eq!(first["harness"]["level"], "default");
    assert_eq!(names(&first, &["harness", "headers"]), ["lib.h"]);

    // Nothing may be fetched or patched again.
    fs::rename(&upstream, fx.tmp.path().join("upstream-lib-gone")).unwrap();
    let patch_path = fx.registry.join("index/l/lib").join(&patch);
    fs::rename(&patch_path, fx.tmp.path().join("lib.patch.gone")).unwrap();

    let second = fx.verify_ok("lib");
    assert_eq!(second["translation_units"]["count"], 1);
    assert_eq!(second["version"], first["version"]);
}

/// A package with two libraries, where the default is the *second* one and it
/// depends on the first through `local = true`. The first is split over two
/// translation units so that only a correct link order (dependent first) pulls
/// in the symbol the default library needs.
#[test]
fn verify_builds_the_default_library_and_its_siblings_when_the_default_is_not_first() {
    let fx = Fixture::new();
    fx.publish(
        "twolib",
        &[
            (
                "Harbour.toml",
                "[package]\nname = \"twolib\"\nversion = \"0.1.0\"\n\
                 default_target = \"top\"\n\n\
                 [targets.base]\nkind = \"staticlib\"\n\
                 sources = [\"base_a.c\", \"base_b.c\"]\n\n\
                 [targets.base.surface.compile.public]\ninclude_dirs = [\"base\"]\n\n\
                 [targets.top]\nkind = \"staticlib\"\nsources = [\"top.c\"]\n\n\
                 [targets.top.surface.compile.public]\ninclude_dirs = [\"top\"]\n\n\
                 [targets.top.deps]\nbase = { local = true }\n",
            ),
            (
                "base/base.h",
                "int base_value(void);\nint base_secret(void);\n",
            ),
            ("top/top.h", "int top_value(void);\n"),
            ("base_a.c", "int base_value(void) { return 1; }\n"),
            ("base_b.c", "int base_secret(void) { return 40; }\n"),
            (
                "top.c",
                "#include <base.h>\nint top_value(void) { return base_secret() + 1; }\n",
            ),
        ],
        &[],
        "",
    );

    let json = fx.verify_ok("twolib");

    // Both libraries were built and checked, not just the first declared.
    let artifacts = names(&json, &["artifacts"]).join("\n");
    assert!(artifacts.contains("top"), "{artifacts}");
    assert!(artifacts.contains("base"), "{artifacts}");
    assert!(
        step(&json, "Artifacts")["message"]
            .as_str()
            .unwrap()
            .contains("2 libraries"),
        "{json:#}"
    );

    // The harness linked the default library, and so had to link its sibling.
    assert_eq!(json["harness"]["link_symbol"], "top_value");
    let headers = names(&json, &["harness", "headers"]);
    assert!(headers.contains(&"top.h".to_string()), "{headers:?}");
}

/// A package whose sources are produced by a `prebuild` generator, which the
/// real build runs while planning. The translation units `verify` reports are
/// the ones an ordinary consumer's lockfile records, because both come from
/// the same build.
#[test]
fn verify_runs_prebuild_generators_and_agrees_with_an_ordinary_consumer() {
    let fx = Fixture::new();
    let generator_dir = fx.tmp.path().join("gen-scratch");
    fs::create_dir_all(&generator_dir).unwrap();
    let prebuild = write_answer_generator(&generator_dir, "gen", 5);
    let script = if cfg!(windows) { "gen.cmd" } else { "gen.sh" };
    let script_text = fs::read_to_string(generator_dir.join(script)).unwrap();
    let manifest = format!(
        "[package]\nname = \"gen\"\nversion = \"0.1.0\"\n\n\
         [targets.gen]\nkind = \"staticlib\"\n\
         sources = [\"gen.c\", \"generated/table.c\"]\n\
         public_headers = [\"gen.h\", \"generated/table.h\"]\n\n\
         [targets.gen.surface.compile.public]\ninclude_dirs = [\".\", \"generated\"]\n\n\
         {prebuild}"
    );
    fx.publish(
        "gen",
        &[
            ("Harbour.toml", manifest.as_str()),
            (script, script_text.as_str()),
            ("gen.h", "int gen_value(void);\n"),
            (
                "gen.c",
                "#include \"table.h\"\nint gen_value(void) { return generated_answer; }\n",
            ),
        ],
        &[],
        "",
    );

    let json = fx.verify_ok("gen");
    let units = names(&json, &["translation_units", "files"]);
    assert_eq!(units, ["gen.c", "generated/table.c"], "{json:#}");
    let headers = names(&json, &["harness", "headers"]);
    assert!(
        headers.contains(&"table.h".to_string()) && headers.contains(&"gen.h".to_string()),
        "the generated header is a public header the harness includes: {headers:?}"
    );

    // The same package, depended on by an ordinary project and built with
    // `harbour build`, records the same translation units.
    let app = fx.tmp.path().join("app");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(
        app.join("src/main.c"),
        "#include <gen.h>\nint main(void) { return gen_value() == 5 ? 0 : 1; }\n",
    )
    .unwrap();
    fs::write(
        app.join("Harbour.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\ngen = \"^0.1\"\n\n\
         [targets.app]\nkind = \"exe\"\nsources = [\"src/main.c\"]\n",
    )
    .unwrap();
    harbour_run_env(
        &fx.home,
        &app,
        &["build"],
        &[(
            "HARBOUR_TEST_REGISTRY_URL",
            &local_registry::file_url(&fx.registry),
        )],
    )
    .success();
    let lock = fs::read_to_string(app.join("Harbour.lock")).unwrap();
    for unit in &units {
        assert!(
            lock.contains(&format!("\"{unit}\"")),
            "the consumer's lockfile must record `{unit}` too:\n{lock}"
        );
    }
}

/// A library whose source patterns match nothing builds no archive. That is a
/// failure naming the target, not a pass over an empty graph.
#[test]
fn verify_fails_when_a_library_target_produces_no_artifact() {
    let fx = Fixture::new();
    fx.publish(
        "hollow",
        &[
            (
                "Harbour.toml",
                "[package]\nname = \"hollow\"\nversion = \"0.1.0\"\n\n\
                 [targets.hollow]\nkind = \"staticlib\"\nsources = [\"nothing-here/*.c\"]\n\n\
                 [targets.hollow.surface.compile.public]\ninclude_dirs = [\".\"]\n",
            ),
            ("hollow.h", "int hollow_value(void);\n"),
        ],
        &[],
        "",
    );

    let (run, json) = fx.verify("hollow", &[]);
    assert!(!run.status.success(), "must exit non-zero\n{run}");
    assert_eq!(json["passed"], false);
    let failure = &json["failure"];
    assert_eq!(failure["stage"], "Build", "{json:#}");
    let reason = failure["reason"].as_str().unwrap();
    assert!(
        reason.contains("hollow") && reason.contains("nothing-here/*.c"),
        "the reason names the target and the patterns that matched no file: {reason}"
    );
}

/// A declared `[metadata.harness]` is compiled, linked and run.
#[test]
fn verify_runs_a_declared_harness() {
    let fx = Fixture::new();
    fx.publish(
        "simple",
        &simple_files(),
        &[],
        "\n[metadata.harness]\nheader = \"simple.h\"\ntest_call = \"simple_value()\"\nlang = \"c\"\n",
    );

    let json = fx.verify_ok("simple");
    assert_eq!(json["harness"]["level"], "declared");
    assert_eq!(names(&json, &["harness", "headers"]), ["simple.h"]);
    assert_eq!(json["harness"]["executed"], true);
}

/// A declared call that compiles but does not link fails the Harness stage,
/// with the linker's own words.
#[test]
fn verify_fails_when_the_declared_harness_call_does_not_link() {
    let fx = Fixture::new();
    let mut files = simple_files();
    files[1] = (
        "simple.h",
        "int simple_value(void);\nint declared_but_never_defined(void);\n",
    );
    fx.publish(
        "simple",
        &files,
        &[],
        "\n[metadata.harness]\nheader = \"simple.h\"\n\
         test_call = \"declared_but_never_defined()\"\nlang = \"c\"\n",
    );

    let (run, json) = fx.verify("simple", &[]);
    assert!(!run.status.success(), "must exit non-zero\n{run}");
    assert_eq!(json["failure"]["stage"], "Harness", "{json:#}");
    let reason = json["failure"]["reason"].as_str().unwrap();
    assert!(
        reason.contains("declared_but_never_defined"),
        "the linker's message names the symbol: {reason}"
    );
    // Everything before the harness was fine, and says so.
    assert_eq!(step(&json, "Build")["status"], "passed");
    assert_eq!(step(&json, "Artifacts")["status"], "passed");
}

/// With no declared harness, a public header that does not compile on its own
/// is a warning (with the compiler's first error) as long as another one does.
#[test]
fn verify_warns_about_a_public_header_that_does_not_compile_alone() {
    let fx = Fixture::new();
    let mut files = simple_files_with_include_dir();
    files.push(("include/needs_other.h", "#include <not_a_real_header.h>\n"));
    fx.publish("simple", &files, &[], "");

    let json = fx.verify_ok("simple");
    assert_eq!(names(&json, &["harness", "headers"]), ["simple.h"]);
    let rejected = json["harness"]["rejected_headers"].as_array().unwrap();
    assert_eq!(rejected.len(), 1, "{json:#}");
    assert_eq!(rejected[0]["header"], "needs_other.h");
    assert!(
        rejected[0]["error"]
            .as_str()
            .unwrap()
            .contains("not_a_real_header.h"),
        "{json:#}"
    );
    let warnings = step(&json, "Harness")["warnings"].to_string();
    assert!(warnings.contains("needs_other.h"), "{warnings}");
}

/// When the package root is itself the include directory (zlib's layout) its
/// public headers cannot be told from its private ones, so the one named for
/// the package is the entry point and the rest are not held against it.
#[test]
fn verify_does_not_hold_a_root_include_directory_package_to_its_private_headers() {
    let fx = Fixture::new();
    let mut files = simple_files();
    files.push(("simple_internal.h", "#include <not_a_real_header.h>\n"));
    fx.publish("simple", &files, &[], "");

    let json = fx.verify_ok("simple");
    assert_eq!(names(&json, &["harness", "headers"]), ["simple.h"]);
    assert!(
        json["harness"]["rejected_headers"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{json:#}"
    );
    assert!(
        step(&json, "Harness")["warnings"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{json:#}"
    );
}

/// ...and fails when none of them does: a consumer could not include the
/// package at all.
#[test]
fn verify_fails_when_no_public_header_compiles() {
    let fx = Fixture::new();
    let mut files = simple_files_with_include_dir();
    files[1] = ("include/simple.h", "#include <not_a_real_header.h>\n");
    fx.publish("simple", &files, &[], "");

    let (run, json) = fx.verify("simple", &[]);
    assert!(!run.status.success(), "must exit non-zero\n{run}");
    assert_eq!(json["failure"]["stage"], "Harness", "{json:#}");
    let reason = json["failure"]["reason"].as_str().unwrap();
    assert!(reason.contains("none of the 1 public header"), "{reason}");
    assert!(reason.contains("not_a_real_header.h"), "{reason}");
}

#[test]
fn verify_skip_harness_records_the_skip() {
    let fx = Fixture::new();
    fx.publish("simple", &simple_files(), &[], "");

    let (run, json) = fx.verify("simple", &["--skip-harness"]);
    assert!(run.status.success(), "{run}");
    assert_eq!(step(&json, "Harness")["status"], "skipped", "{json:#}");
    assert_eq!(json["harness"]["level"], "none");
}

/// A package the registry does not have is a Resolve failure that says so.
#[test]
fn verify_reports_a_package_the_registry_does_not_have() {
    let fx = Fixture::new();
    fx.publish("simple", &simple_files(), &[], "");

    let (run, json) = fx.verify("nonesuch", &[]);
    assert!(!run.status.success(), "must exit non-zero\n{run}");
    assert_eq!(json["failure"]["stage"], "Resolve", "{json:#}");
    assert!(
        json["failure"]["reason"]
            .as_str()
            .unwrap()
            .contains("nonesuch"),
        "{json:#}"
    );
}

/// Without `--registry-path` it resolves against the configured registry,
/// which is also how a consumer finds the package.
#[test]
fn verify_uses_the_configured_registry_by_default() {
    let fx = Fixture::new();
    fx.publish("simple", &simple_files(), &[], "");

    let run = harbour_run_env(
        &fx.home,
        fx.tmp.path(),
        &["verify", "simple", "--format", "json"],
        &[(
            "HARBOUR_TEST_REGISTRY_URL",
            &local_registry::file_url(&fx.registry),
        )],
    );
    assert!(run.status.success(), "{run}");
    let json: Value = serde_json::from_str(&run.stdout).unwrap();
    assert_eq!(json["passed"], true, "{json:#}");
}
