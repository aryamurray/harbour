//! `[build] msvc_runtime` reaches the compile command of a C file.
//!
//! The runtime flag (`/MD`, `/MDd`, `/MT`, `/MTd`) used to be emitted only for
//! C++ sources. `cl` then compiled every C file with its own default, `/MT`,
//! so `msvc_runtime = "dynamic"`, which is the documented default, did nothing
//! for C, and a build with both languages put two C runtimes into one link.
//! These run from any host: they read the argv Harbour builds for `cl`, not what
//! `cl` does with it.

use super::*;
use crate::builder::plan::CompileStep;
use crate::builder::toolchain::MsvcToolchain;
use crate::core::target::Language;

/// The argv for compiling one source of `lang` under a manifest whose targets are
/// all `lang`, as `profile_name` ("debug" or "release"), with `build_body` as the
/// `[build]` table.
fn msvc_argv(lang: Language, build_body: &str, profile_name: &str) -> Vec<String> {
    msvc_context(lang, build_body, profile_name).1
}

fn msvc_context(
    lang: Language,
    build_body: &str,
    profile_name: &str,
) -> (BuildContext, Vec<String>) {
    let (lang_name, file, std_line) = match lang {
        Language::Cxx => ("c++", "m.cpp", "cpp_std = \"17\"\n"),
        _ => ("c", "m.c", ""),
    };
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path().join("p");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("Harbour.toml"),
        format!(
            "[package]\nname = \"p\"\nversion = \"1.0.0\"\n\n{build_body}\n\
             [targets.p]\nkind = \"exe\"\nlang = \"{lang_name}\"\n{std_line}\
             sources = [\"src/{file}\"]\n"
        ),
    )
    .unwrap();
    std::fs::write(dir.join("src").join(file), "int main(){}\n").unwrap();

    let manifest = crate::core::Manifest::load(&dir.join("Harbour.toml")).unwrap();
    let profile = manifest.resolved_profile(profile_name).unwrap();
    let release_like = manifest.profile_is_release_like(profile_name);
    let build_config = manifest.build.clone();
    let source = crate::core::SourceId::for_path(&dir).unwrap();
    let pkg_id = crate::core::PackageId::new("p", "1.0.0".parse().unwrap(), source);
    let package = crate::core::Package::with_source_id(manifest, dir.clone(), source).unwrap();

    let mut resolve = crate::resolver::Resolve::new();
    resolve.add_package(pkg_id, crate::core::Summary::new(pkg_id, vec![], None));
    let mut packages = std::collections::HashMap::new();
    packages.insert(pkg_id, package);
    let constraints = crate::resolver::cpp_constraints::CppConstraints::compute(
        &resolve,
        &packages,
        &build_config,
        None,
    )
    .unwrap();

    let ctx = BuildContext {
        toolchain: std::sync::Arc::new(MsvcToolchain::new(
            PathBuf::from("cl"),
            PathBuf::from("lib"),
            PathBuf::from("link"),
        )),
        target: TargetTriple::host(),
        compiler: CompilerIdentity::new("msvc", "19.0"),
        platform: TargetPlatform::host(),
        profile,
        profile_name: profile_name.to_string(),
        release_like,
        output_dir: PathBuf::from("target"),
        deps_dir: PathBuf::from("target/deps"),
        workspace_root: dir.clone(),
        cpp_constraints: Some(constraints),
        vcpkg: None,
        target_cflags: Vec::new(),
        target_ldflags: Vec::new(),
        build_target: None,
        feature_selection: FeatureSelection::default(),
    };
    let step = CompileStep {
        source: dir.join("src").join(file),
        output: dir.join("m.obj"),
        include_dirs: vec![],
        defines: vec![],
        cflags: vec![],
        lang,
        package: "p".to_string(),
        target: "p".to_string(),
        c_std: None,
    };
    let args = ctx.compile_spec(&step).unwrap().args;
    (ctx, args)
}

/// The runtime flags in an argv, in order.
fn runtime_flags(args: &[String]) -> Vec<&str> {
    args.iter()
        .map(String::as_str)
        .filter(|a| matches!(*a, "/MD" | "/MDd" | "/MT" | "/MTd"))
        .collect()
}

#[test]
fn a_c_file_gets_the_dynamic_runtime_by_default() {
    let args = msvc_argv(Language::C, "", "release");
    assert_eq!(runtime_flags(&args), ["/MD"], "{args:?}");
}

#[test]
fn a_c_file_in_a_debug_build_gets_the_debug_runtime() {
    let args = msvc_argv(Language::C, "", "debug");
    assert_eq!(runtime_flags(&args), ["/MDd"], "{args:?}");
}

#[test]
fn a_c_only_build_honours_msvc_runtime_static() {
    let build = "[build]\nmsvc_runtime = \"static\"\n";
    assert_eq!(
        runtime_flags(&msvc_argv(Language::C, build, "release")),
        ["/MT"]
    );
    assert_eq!(
        runtime_flags(&msvc_argv(Language::C, build, "debug")),
        ["/MTd"]
    );
}

/// The failure that made this visible: C and C++ objects in one link must name
/// the same runtime, or `cl`'s `RuntimeLibrary` mismatch stops the link.
#[test]
fn c_and_cxx_files_in_one_configuration_name_the_same_runtime() {
    for build in ["", "[build]\nmsvc_runtime = \"static\"\n"] {
        for profile in ["debug", "release"] {
            let c = msvc_argv(Language::C, build, profile);
            let cxx = msvc_argv(Language::Cxx, build, profile);
            assert_eq!(
                runtime_flags(&c),
                runtime_flags(&cxx),
                "C and C++ disagree for build {build:?}, profile {profile}: {c:?} vs {cxx:?}"
            );
            assert_eq!(runtime_flags(&c).len(), 1, "exactly one flag: {c:?}");
        }
    }
}

/// An object compiled with one runtime must not be reused for another, so the
/// runtime is part of what a C-only build fingerprints.
#[test]
fn changing_msvc_runtime_changes_a_c_only_builds_fingerprint() {
    let dynamic = msvc_context(Language::C, "", "release").0;
    let statik = msvc_context(
        Language::C,
        "[build]\nmsvc_runtime = \"static\"\n",
        "release",
    )
    .0;
    assert_ne!(
        dynamic.toolchain_fingerprint().hash(),
        statik.toolchain_fingerprint().hash(),
        "a stale /MD object would be reused for a /MT build"
    );
}

/// The other compilers are untouched: a runtime flag is `cl`'s.
#[test]
fn only_msvc_gets_runtime_options_for_a_c_only_build() {
    let (mut ctx, _) = msvc_context(Language::C, "", "release");
    assert!(ctx.compile_options().is_some());
    ctx.compiler = CompilerIdentity::new("gcc", "14.0");
    assert!(
        ctx.compile_options().is_none(),
        "a GCC C-only build has no C++ options at all"
    );
}
