//! Prebuilt-binary packages: a library that is downloaded and linked, with
//! nothing compiled.
//!
//! No real prebuilt package is used. Each test builds a small static and a
//! small shared library with the host toolchain -- by asking `harbour` itself
//! to build an ordinary package, which is what makes this run under MSVC too
//! -- then lays the results out the way an upstream binary archive is laid
//! out (`include/`, `lib/`) with a `kind = "prebuilt"` manifest, and builds a
//! consumer against that. Every assertion about linking is made from the
//! *running* consumer: a program that links and then cannot find its shared
//! library is exactly the failure this feature exists to prevent.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use harbour::test_support::fixtures::local_registry;
use tempfile::TempDir;

/// The host's `os`, as a `when` block spells it.
fn host_os() -> String {
    harbour::core::surface::TargetPlatform::host().os
}

/// An `os` that is certainly not this machine's.
fn other_os() -> &'static str {
    if host_os() == "windows" {
        "linux"
    } else {
        "windows"
    }
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// Run `harbour <args>` in `dir`; the combined output and whether it worked.
fn harbour(home: &Path, dir: &Path, args: &[&str]) -> common::Run {
    let out = common::harbour(home)
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    common::Run {
        success: out.status.success(),
        text: format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    }
}

fn harbour_ok(home: &Path, dir: &Path, args: &[&str]) -> String {
    let run = harbour(home, dir, args);
    assert!(run.success, "harbour {args:?} failed:\n{}", run.text);
    run.text
}

/// Start a built program by its path, with nothing but the ambient
/// environment, and return what it printed. It must exit successfully: this
/// is the "does the program start" assertion.
fn start(app: &Path, name: &str) -> String {
    let exe = common::built_exe(app, name);
    let out = Command::new(&exe).output().unwrap();
    assert!(
        out.status.success(),
        "{} did not start or failed ({}):\n{}{}",
        exe.display(),
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// The libraries the host toolchain built for the fixtures.
struct Built {
    /// `libpre_static.a` / `pre_static.lib`.
    static_lib: PathBuf,
    /// A C++ static library with a C interface.
    cxx_lib: PathBuf,
    /// `libpre_shared.so` / `.dylib` / `pre_shared.dll`.
    shared_lib: PathBuf,
    /// The import library of the DLL, on Windows.
    import_lib: Option<PathBuf>,
}

/// Build the fixture libraries in `dir`, returning `static_value` and
/// `shared_value` from `static_value()` and `shared_value()`.
///
/// The shared library is given the identity a real upstream one has, so that
/// the consumer cannot find it by accident: an ELF `SONAME` (the consumer then
/// records the bare name and needs an rpath), an `@rpath/` install name on
/// macOS, and nothing on Windows, where the loader looks beside the program.
fn build_fixture_libs(home: &Path, dir: &Path, static_value: i32, shared_value: i32) -> Built {
    write(
        &dir.join("Harbour.toml"),
        r#"[package]
name = "fixture"
version = "1.0.0"

[targets.pre_static]
kind = "staticlib"
sources = ["src/static.c"]

[targets.pre_cxx]
kind = "staticlib"
lang = "c++"
sources = ["src/cxx.cpp"]

[targets.pre_shared]
kind = "sharedlib"
sources = ["src/shared.c"]

[[targets.pre_shared.surface.when]]
os = "linux"
[targets.pre_shared.surface.when."link.private"]
ldflags = ["-Wl,-soname,libpre_shared.so"]

[[targets.pre_shared.surface.when]]
os = "macos"
[targets.pre_shared.surface.when."link.private"]
ldflags = ["-Wl,-install_name,@rpath/libpre_shared.dylib"]
"#,
    );
    write(
        &dir.join("src/static.c"),
        &format!("int static_value(void) {{ return {static_value}; }}\n"),
    );
    write(
        &dir.join("src/shared.c"),
        &format!(
            "#ifdef _WIN32\n#define EXPORT __declspec(dllexport)\n#else\n#define EXPORT\n#endif\n\
             EXPORT int shared_value(void) {{ return {shared_value}; }}\n"
        ),
    );
    // Needs the C++ runtime to link: `operator new`, `std::string`, and an
    // exception that is thrown and caught.
    write(
        &dir.join("src/cxx.cpp"),
        "#include <stdexcept>\n#include <string>\n\
         extern \"C\" int cxx_value(void) {\n\
         \x20   std::string *s = new std::string(\"prebuilt\");\n\
         \x20   int n = 0;\n\
         \x20   try { throw std::runtime_error(*s); } catch (const std::exception &e) {\n\
         \x20       n = static_cast<int>(std::string(e.what()).size());\n\
         \x20   }\n\
         \x20   delete s;\n\
         \x20   return n;\n}\n",
    );
    harbour_ok(home, dir, &["build"]);

    // The whole project, not only `lib/`: where MSVC's linker leaves a DLL's
    // import library is its decision.
    let files = common::walk_files(dir);
    let find = |name: &str| -> Option<PathBuf> {
        files
            .iter()
            .find(|p| p.file_name().is_some_and(|n| n == name))
            .cloned()
    };
    let must = |name: &str| -> PathBuf {
        find(name).unwrap_or_else(|| panic!("the fixture build produced no {name}: {files:?}"))
    };
    if cfg!(windows) {
        Built {
            static_lib: must("pre_static.lib"),
            cxx_lib: must("pre_cxx.lib"),
            shared_lib: must("pre_shared.dll"),
            import_lib: Some(must("pre_shared.lib")),
        }
    } else {
        let ext = if cfg!(target_os = "macos") {
            "dylib"
        } else {
            "so"
        };
        Built {
            static_lib: must("libpre_static.a"),
            cxx_lib: must("libpre_cxx.a"),
            shared_lib: must(&format!("libpre_shared.{ext}")),
            import_lib: None,
        }
    }
}

const PRE_H: &str = "#ifdef __cplusplus\nextern \"C\" {\n#endif\n\
                     int static_value(void);\nint shared_value(void);\nint cxx_value(void);\n\
                     #ifdef __cplusplus\n}\n#endif\n";

fn file_name(path: &Path) -> String {
    path.file_name().unwrap().to_string_lossy().into_owned()
}

/// Copy `files` into `<pkg>/lib` and write `include/pre.h`.
fn lay_out(pkg: &Path, files: &[&Path]) {
    write(&pkg.join("include/pre.h"), PRE_H);
    fs::create_dir_all(pkg.join("lib")).unwrap();
    for file in files {
        fs::copy(file, pkg.join("lib").join(file_name(file))).unwrap();
    }
}

/// The manifest of a package `pre` whose one target `pre` is prebuilt and
/// lists `libs` (and `runtime`) for this OS, plus a block for another OS
/// naming files that do not exist.
fn prebuilt_manifest(lang: &str, libs: &[String], runtime: &[String]) -> String {
    let list = |files: &[String]| {
        files
            .iter()
            .map(|f| format!("\"lib/{f}\""))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut text = format!(
        "[package]\nname = \"pre\"\nversion = \"1.0.0\"\n\n\
         [targets.pre]\nkind = \"prebuilt\"\nlang = \"{lang}\"\n\n\
         [targets.pre.public]\ninclude_dirs = [\"include\"]\n\n\
         [[targets.pre.when]]\nos = \"{}\"\nprebuilt_libs = [{}]\n",
        host_os(),
        list(libs)
    );
    if !runtime.is_empty() {
        text.push_str(&format!("runtime_libs = [{}]\n", list(runtime)));
    }
    text.push_str(&format!(
        "\n[[targets.pre.when]]\nos = \"{}\"\n\
         prebuilt_libs = [\"lib/not-for-this-platform.lib\"]\n\
         runtime_libs = [\"lib/not-for-this-platform.dll\"]\n",
        other_os()
    ));
    text
}

/// A prebuilt package `pre` holding the static library.
fn static_package(pkg: &Path, built: &Built) {
    lay_out(pkg, &[&built.static_lib]);
    write(
        &pkg.join("Harbour.toml"),
        &prebuilt_manifest("c", &[file_name(&built.static_lib)], &[]),
    );
}

/// What a shared prebuilt package lists: `(prebuilt_libs, runtime_libs)`.
fn shared_lists(built: &Built) -> (Vec<String>, Vec<String>) {
    match &built.import_lib {
        Some(import) => (vec![file_name(import)], vec![file_name(&built.shared_lib)]),
        None => (vec![file_name(&built.shared_lib)], Vec::new()),
    }
}

/// A prebuilt package `pre` holding the shared library (and, on Windows, its
/// import library).
fn shared_package(pkg: &Path, built: &Built) {
    let mut files: Vec<&Path> = vec![&built.shared_lib];
    if let Some(import) = &built.import_lib {
        files.push(import);
    }
    lay_out(pkg, &files);
    let (libs, runtime) = shared_lists(built);
    write(
        &pkg.join("Harbour.toml"),
        &prebuilt_manifest("c", &libs, &runtime),
    );
}

/// An application at `app` depending on `pre` by path, printing `call`.
fn app_using(app: &Path, pre: &Path, call: &str) {
    let rel = pathdiff::diff_paths(pre, app).unwrap();
    common::write_app(
        app,
        &format!(
            "pre = {{ path = \"{}\" }}",
            rel.display().to_string().replace('\\', "/")
        ),
        &["pre.h"],
        call,
    );
}

struct Scene {
    tmp: TempDir,
    home: PathBuf,
    built: Built,
}

fn scene() -> Scene {
    let tmp = TempDir::new().unwrap();
    let home = common::home(&tmp);
    let built = build_fixture_libs(&home, &tmp.path().join("fixture"), 42, 7);
    Scene { tmp, home, built }
}

// ---------------------------------------------------------------------------
// Linking and starting
// ---------------------------------------------------------------------------

/// A static prebuilt library is linked into the program.
#[test]
fn a_static_prebuilt_library_is_linked() {
    let s = scene();
    let pre = s.tmp.path().join("pre");
    static_package(&pre, &s.built);
    let app = s.tmp.path().join("app");
    app_using(&app, &pre, "static_value()");

    let out = harbour_ok(&s.home, &app, &["build"]);
    assert_eq!(start(&app, "app"), "42");
    assert!(
        out.contains("Compiling 1 file"),
        "only the consumer's own file is compiled:\n{out}"
    );
}

/// A shared prebuilt library is linked, and the program then *starts* when
/// run by its path with no help: by rpath on Linux and macOS, and because the
/// DLL was placed beside it on Windows.
#[test]
fn a_program_linking_a_shared_prebuilt_library_starts() {
    let s = scene();
    let pre = s.tmp.path().join("pre");
    shared_package(&pre, &s.built);
    let app = s.tmp.path().join("app");
    app_using(&app, &pre, "shared_value()");

    harbour_ok(&s.home, &app, &["build"]);
    assert_eq!(start(&app, "app"), "7");

    let beside = common::built_exe(&app, "app")
        .parent()
        .unwrap()
        .join(file_name(&s.built.shared_lib));
    if cfg!(windows) {
        assert!(
            beside.is_file(),
            "the DLL must be placed beside the executable: {}",
            beside.display()
        );
    } else {
        assert!(
            !beside.exists(),
            "nothing is copied where there is an rpath: {}",
            beside.display()
        );
    }

    // `harbour run` starts it too, and passes its output through.
    let run = harbour_ok(&s.home, &app, &["run"]);
    assert!(run.lines().any(|l| l.trim() == "7"), "{run}");
}

/// On Windows the DLL beside the executable is kept current without being
/// copied again on every build: it is left alone while the package's DLL is
/// unchanged, and replaced when that changes.
#[cfg(windows)]
#[test]
fn the_dll_beside_the_executable_follows_the_package() {
    let s = scene();
    let pre = s.tmp.path().join("pre");
    shared_package(&pre, &s.built);
    let app = s.tmp.path().join("app");
    app_using(&app, &pre, "shared_value()");
    harbour_ok(&s.home, &app, &["build"]);
    assert_eq!(start(&app, "app"), "7");

    let beside = common::built_exe(&app, "app")
        .parent()
        .unwrap()
        .join(file_name(&s.built.shared_lib));
    let first = fs::metadata(&beside).unwrap().modified().unwrap();

    let again = harbour_ok(&s.home, &app, &["build"]);
    assert!(!again.contains("Placed"), "nothing to place:\n{again}");
    assert_eq!(fs::metadata(&beside).unwrap().modified().unwrap(), first);

    // A new DLL in the package: same exports, another answer. (Written over
    // the old file, so where the copy beside the executable is a hard link
    // there is nothing left to place; what matters is what the program loads.)
    let newer = build_fixture_libs(&s.home, &s.tmp.path().join("fixture2"), 42, 8);
    fs::copy(
        &newer.shared_lib,
        pre.join("lib").join(file_name(&newer.shared_lib)),
    )
    .unwrap();
    harbour_ok(&s.home, &app, &["build"]);
    assert_eq!(start(&app, "app"), "8");

    // Removed by hand: put back by the next build, even with nothing to link.
    fs::remove_file(&beside).unwrap();
    harbour_ok(&s.home, &app, &["build"]);
    assert_eq!(start(&app, "app"), "8");
}

/// `harbour test` starts a test program that links a shared prebuilt library.
#[test]
fn harbour_test_runs_a_program_linking_a_shared_prebuilt_library() {
    let s = scene();
    let pre = s.tmp.path().join("pre");
    shared_package(&pre, &s.built);
    let app = s.tmp.path().join("app");
    write(
        &app.join("Harbour.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\npre = { path = \"../pre\" }\n\n\
         [targets.unit_test]\nkind = \"exe\"\nsources = [\"src/t.c\"]\n",
    );
    write(
        &app.join("src/t.c"),
        "#include \"pre.h\"\nint main(void) { return shared_value() == 7 ? 0 : 1; }\n",
    );
    let out = harbour_ok(&s.home, &app, &["test"]);
    assert!(out.contains("unit_test ... ok"), "{out}");
    assert!(out.contains("1 passed; 0 failed"), "{out}");
}

/// A prebuilt library reaches a program through an ordinary library that
/// depends on it: `app -> mid (compiled) -> pre (prebuilt)`. The static one
/// has to be on `app`'s link line after `mid`'s archive; the shared one has
/// to be found when `app` starts.
#[test]
fn a_prebuilt_library_is_linked_transitively() {
    let s = scene();
    for (kind, call, expect) in [
        ("static", "static_value()", "43"),
        ("shared", "shared_value()", "8"),
    ] {
        let root = s.tmp.path().join(kind);
        let pre = root.join("pre");
        if kind == "static" {
            static_package(&pre, &s.built);
        } else {
            shared_package(&pre, &s.built);
        }
        let mid = root.join("mid");
        write(
            &mid.join("Harbour.toml"),
            "[package]\nname = \"mid\"\nversion = \"0.1.0\"\n\n\
             [dependencies]\npre = { path = \"../pre\" }\n\n\
             [targets.mid]\nkind = \"staticlib\"\nsources = [\"mid.c\"]\n\n\
             [targets.mid.public]\ninclude_dirs = [\".\"]\n",
        );
        write(&mid.join("mid.h"), "int mid_value(void);\n");
        write(
            &mid.join("mid.c"),
            &format!("#include \"pre.h\"\nint mid_value(void) {{ return {call} + 1; }}\n"),
        );
        let app = root.join("app");
        common::write_app(
            &app,
            "mid = { path = \"../mid\" }",
            &["mid.h"],
            "mid_value()",
        );

        harbour_ok(&s.home, &app, &["build"]);
        assert_eq!(start(&app, "app"), expect, "{kind}");
    }
}

/// A C program linking a `lang = "c++"` prebuilt library is linked with the
/// C++ driver, as it is for a C++ static library Harbour built: the archive's
/// objects need the C++ runtime, and the C driver does not add it.
#[test]
fn a_c_program_links_a_cxx_prebuilt_library_with_the_cxx_driver() {
    let s = scene();
    let pre = s.tmp.path().join("pre");
    lay_out(&pre, &[&s.built.cxx_lib]);
    write(
        &pre.join("Harbour.toml"),
        &prebuilt_manifest("c++", &[file_name(&s.built.cxx_lib)], &[]),
    );
    let app = s.tmp.path().join("app");
    app_using(&app, &pre, "cxx_value()");

    harbour_ok(&s.home, &app, &["build"]);
    assert_eq!(start(&app, "app"), "8");

    // The driver in the plan, where there is a choice of driver at all.
    let plan = harbour_ok(&s.home, &app, &["build", "--plan"]);
    assert!(plan.contains("\"use_cxx_linker\": true"), "{plan}");
}

/// A prebuilt target sits beside ordinary targets of one package and is
/// linked through `local = true`.
#[test]
fn a_prebuilt_target_beside_ordinary_targets_of_one_package() {
    let s = scene();
    let pkg = s.tmp.path().join("pkg");
    lay_out(&pkg, &[&s.built.static_lib]);
    write(
        &pkg.join("Harbour.toml"),
        &format!(
            "[package]\nname = \"pkg\"\nversion = \"0.1.0\"\n\n\
             [targets.vendor]\nkind = \"prebuilt\"\nprebuilt_libs = [\"lib/{}\"]\n\n\
             [targets.vendor.public]\ninclude_dirs = [\"include\"]\n\n\
             [targets.wrap]\nkind = \"staticlib\"\nsources = [\"wrap.c\"]\n\n\
             [targets.wrap.deps]\nvendor = {{ local = true }}\n\n\
             [targets.app]\nkind = \"exe\"\nsources = [\"main.c\"]\n\n\
             [targets.app.deps]\nwrap = {{ local = true }}\n",
            file_name(&s.built.static_lib)
        ),
    );
    write(
        &pkg.join("wrap.c"),
        "#include \"pre.h\"\nint wrapped(void) { return static_value() * 2; }\n",
    );
    write(
        &pkg.join("main.c"),
        "#include <stdio.h>\nint wrapped(void);\n\
         int main(void) { printf(\"%d\\n\", wrapped()); return 0; }\n",
    );
    harbour_ok(&s.home, &pkg, &["build"]);
    assert_eq!(start(&pkg, "app"), "84");
}

// ---------------------------------------------------------------------------
// What is listed must exist, for the platform being built for
// ---------------------------------------------------------------------------

/// A listed file that is not in the package is an error naming the file and
/// the block that asked for it -- not a linker error.
#[test]
fn a_missing_listed_file_is_an_error_naming_the_file_and_its_block() {
    let s = scene();
    let pre = s.tmp.path().join("pre");
    static_package(&pre, &s.built);
    fs::remove_file(pre.join("lib").join(file_name(&s.built.static_lib))).unwrap();
    let app = s.tmp.path().join("app");
    app_using(&app, &pre, "static_value()");

    let run = harbour(&s.home, &app, &["build"]);
    assert!(!run.success, "{}", run.text);
    for needle in [
        "prebuilt target `pre` of package `pre`".to_string(),
        format!("lib/{}", file_name(&s.built.static_lib)),
        "`prebuilt_libs` in `[[targets.pre.when]]` block 1".to_string(),
        format!("os = \"{}\"", host_os()),
    ] {
        assert!(
            run.text.contains(&needle),
            "missing `{needle}` in:\n{}",
            run.text
        );
    }
    assert!(
        !run.text.contains("not-for-this-platform"),
        "the other platform's files are not this build's business:\n{}",
        run.text
    );
}

/// The block for another platform is not selected: its files do not exist
/// (see `prebuilt_manifest`) and the build does not look for them. And a
/// package with no block for this platform says so.
#[test]
fn only_the_block_for_this_platform_is_used() {
    let s = scene();
    let pre = s.tmp.path().join("pre");
    static_package(&pre, &s.built);
    let app = s.tmp.path().join("app");
    app_using(&app, &pre, "static_value()");
    harbour_ok(&s.home, &app, &["build"]);
    assert_eq!(start(&app, "app"), "42");
    let plan = harbour_ok(&s.home, &app, &["build", "--plan"]);
    assert!(!plan.contains("not-for-this-platform"), "{plan}");

    // Now a package that has binaries for some other OS only.
    write(
        &pre.join("Harbour.toml"),
        &format!(
            "[package]\nname = \"pre\"\nversion = \"1.0.0\"\n\n\
             [targets.pre]\nkind = \"prebuilt\"\n\n\
             [[targets.pre.when]]\nos = \"{}\"\nprebuilt_libs = [\"lib/x.lib\"]\n",
            other_os()
        ),
    );
    let run = harbour(&s.home, &app, &["build"]);
    assert!(!run.success, "{}", run.text);
    assert!(
        run.text.contains("has no library for this platform")
            && run.text.contains(&format!("os = \"{}\"", other_os())),
        "{}",
        run.text
    );
}

/// `supports` says of a prebuilt package what it says of any other: a target
/// it does not list is reported, by the same warning, before the build.
#[test]
fn supports_is_reported_for_a_prebuilt_package_as_for_any_other() {
    let s = scene();
    let pre = s.tmp.path().join("pre");
    static_package(&pre, &s.built);
    let manifest = fs::read_to_string(pre.join("Harbour.toml")).unwrap();
    write(
        &pre.join("Harbour.toml"),
        &manifest.replace(
            "version = \"1.0.0\"\n",
            "version = \"1.0.0\"\nsupports = [\"riscv64-unknown-linux-gnu\"]\n",
        ),
    );
    let app = s.tmp.path().join("app");
    app_using(&app, &pre, "static_value()");
    let run = harbour(&s.home, &app, &["build"]);
    assert!(
        run.text.contains("package `pre` does not list")
            && run.text.contains("riscv64-unknown-linux-gnu"),
        "{}",
        run.text
    );
}

/// Keys that describe a compile are refused on a prebuilt target, and the
/// prebuilt keys are refused everywhere else.
#[test]
fn the_manifest_keeps_prebuilt_and_compiled_targets_apart() {
    let s = TempDir::new().unwrap();
    let home = common::home(&s);
    let check = |manifest: &str, expect: &[&str]| {
        let dir = s.path().join("pkg");
        let _ = fs::remove_dir_all(&dir);
        write(&dir.join("Harbour.toml"), manifest);
        let run = harbour(&home, &dir, &["build"]);
        assert!(!run.success, "must be refused:\n{manifest}\n{}", run.text);
        for needle in expect {
            assert!(
                run.text.contains(needle),
                "missing `{needle}` in:\n{}",
                run.text
            );
        }
    };
    let head = "[package]\nname = \"pkg\"\nversion = \"0.1.0\"\n\n";

    check(
        &format!("{head}[targets.pkg]\nkind = \"prebuilt\"\nsources = [\"a.c\"]\nprebuilt_libs = [\"lib/a.a\"]\n"),
        &["prebuilt target 'pkg' must not set `sources`"],
    );
    check(
        &format!("{head}[targets.pkg]\nkind = \"prebuilt\"\n"),
        &["prebuilt target 'pkg' lists no library", "prebuilt_libs"],
    );
    check(
        &format!("{head}[targets.pkg]\nkind = \"staticlib\"\nsources = [\"a.c\"]\nprebuilt_libs = [\"lib/a.a\"]\n"),
        &["only a `kind = \"prebuilt\"` target takes"],
    );
    check(
        &format!(
            "{head}[targets.pkg]\nkind = \"prebuilt\"\nprebuilt_libs = [\"../outside/a.a\"]\n"
        ),
        &["must be a path inside the package"],
    );
    check(
        &format!(
            "{head}[targets.pkg]\nkind = \"prebuilt\"\nprebuilt_libs = [\"lib/a.a\"]\n\n\
             [[targets.pkg.when]]\nos = \"linux\"\ncflags = [\"-O2\"]\n"
        ),
        &["`cflags` describes a compile"],
    );
    check(
        &format!("{head}[targets.pkg]\nkind = \"prebuilt\"\nprebuilt_libs = [\"lib/*.a\"]\n"),
        &["is a pattern; name each file"],
    );
}

// ---------------------------------------------------------------------------
// Incremental builds
// ---------------------------------------------------------------------------

/// A second build links nothing; replacing the prebuilt library relinks, and
/// the program then has the new library's code in it.
#[test]
fn a_rebuild_is_a_no_op_and_a_replaced_library_relinks() {
    let s = scene();
    let pre = s.tmp.path().join("pre");
    static_package(&pre, &s.built);
    let app = s.tmp.path().join("app");
    app_using(&app, &pre, "static_value()");

    harbour_ok(&s.home, &app, &["build"]);
    assert_eq!(start(&app, "app"), "42");
    let exe = common::built_exe(&app, "app");
    let linked_at = fs::metadata(&exe).unwrap().modified().unwrap();

    let again = harbour_ok(&s.home, &app, &["build"]);
    assert!(again.contains("up to date"), "{again}");
    assert_eq!(
        fs::metadata(&exe).unwrap().modified().unwrap(),
        linked_at,
        "an unchanged prebuilt library must not relink the program"
    );

    // The package's library is replaced by one that answers differently.
    let newer = build_fixture_libs(&s.home, &s.tmp.path().join("fixture2"), 4242, 7);
    fs::copy(
        &newer.static_lib,
        pre.join("lib").join(file_name(&newer.static_lib)),
    )
    .unwrap();
    let after = harbour_ok(&s.home, &app, &["build"]);
    assert!(
        after.contains("up to date"),
        "nothing is recompiled, only relinked:\n{after}"
    );
    assert_eq!(start(&app, "app"), "4242");
}

// ---------------------------------------------------------------------------
// The other commands
// ---------------------------------------------------------------------------

/// `flags`, `linkplan`, `tree`, `explain`, `clean`, `--strict-sources` and
/// `compile_commands.json` all do something sensible with a prebuilt
/// dependency, and with a prebuilt package as the subject.
#[test]
fn the_other_commands_handle_a_prebuilt_target() {
    let s = scene();
    let pre = s.tmp.path().join("pre");
    shared_package(&pre, &s.built);
    let app = s.tmp.path().join("app");
    app_using(&app, &pre, "shared_value()");
    harbour_ok(&s.home, &app, &["build", "--strict-sources"]);

    let (libs, _) = shared_lists(&s.built);
    let linked = &libs[0];

    let flags = harbour_ok(&s.home, &app, &["flags", "app"]);
    assert!(flags.contains("include"), "{flags}");
    assert!(flags.contains(linked.as_str()), "{flags}");

    let linkplan = harbour_ok(&s.home, &app, &["linkplan", "app"]);
    assert!(linkplan.contains("Prebuilt, from: pre"), "{linkplan}");
    assert!(linkplan.contains(linked.as_str()), "{linkplan}");
    if !cfg!(windows) {
        assert!(flags.contains("-Wl,-rpath,"), "{flags}");
        assert!(linkplan.contains("-Wl,-rpath,"), "{linkplan}");
    }

    let tree = harbour_ok(&s.home, &app, &["tree"]);
    assert!(tree.contains("pre v1.0.0"), "{tree}");
    let explain = harbour_ok(&s.home, &app, &["explain", "pre"]);
    assert!(explain.contains("required by: app"), "{explain}");

    // The database has the consumer's file, compiled with the package's
    // include directory, and nothing for the package itself.
    let db = fs::read_to_string(app.join(".harbour/compile_commands.json")).unwrap();
    let entries: serde_json::Value = serde_json::from_str(&db).unwrap();
    let entries = entries.as_array().unwrap();
    assert_eq!(entries.len(), 1, "{db}");
    assert!(
        entries[0]["file"].as_str().unwrap().ends_with("main.c"),
        "{db}"
    );
    let include = pre.join("include");
    let include_name = include.file_name().unwrap().to_string_lossy();
    assert!(
        entries[0]["arguments"].as_array().unwrap().iter().any(|a| a
            .as_str()
            .is_some_and(|a| a.contains("pre") && a.ends_with(&*include_name))),
        "{db}"
    );

    // The lockfile has the package and no source list for it.
    let lock = fs::read_to_string(app.join("Harbour.lock")).unwrap();
    assert!(lock.contains("name = \"pre\""), "{lock}");
    assert!(!lock.contains("[package.sources]"), "{lock}");

    harbour_ok(&s.home, &app, &["clean"]);
    assert!(!app.join(".harbour/target/debug/bin").exists());
    assert!(
        pre.join("lib")
            .join(file_name(&s.built.shared_lib))
            .is_file(),
        "`clean` removes what Harbour built, never a package's own libraries"
    );
    harbour_ok(&s.home, &app, &["build"]);
    assert_eq!(start(&app, "app"), "7");

    // With the prebuilt package itself as the subject.
    harbour_ok(&s.home, &pre, &["build"]);
    let own = harbour_ok(&s.home, &pre, &["linkplan", "pre"]);
    assert!(own.contains("is prebuilt"), "{own}");
    assert!(own.contains(linked.as_str()), "{own}");
    harbour_ok(&s.home, &pre, &["flags", "pre"]);
    let run = harbour(&s.home, &pre, &["run"]);
    assert!(
        !run.success && run.text.contains("no executable target"),
        "{}",
        run.text
    );
}

// ---------------------------------------------------------------------------
// The registry: one archive per platform
// ---------------------------------------------------------------------------

fn file_url(path: &Path) -> String {
    url::Url::from_file_path(path).unwrap().to_string()
}

/// A unified diff that adds `Harbour.toml` with `manifest` as its content:
/// how a registry gives an upstream binary archive the manifest it lacks.
fn manifest_patch(manifest: &str) -> String {
    let added: String = manifest.lines().map(|l| format!("+{l}\n")).collect();
    format!(
        "diff --git a/Harbour.toml b/Harbour.toml\nnew file mode 100644\n--- /dev/null\n\
         +++ b/Harbour.toml\n@@ -0,0 +1,{} @@\n{added}",
        manifest.lines().count()
    )
}

#[derive(Clone, Copy, PartialEq)]
enum Format {
    Zip,
    TarGz,
}

struct Registry {
    dir: PathBuf,
    url: String,
    /// The archive for this machine.
    archive: PathBuf,
}

/// A registry with `pre` 1.0.0 as a prebuilt package: an archive of `files`
/// for this OS (in `format`, with top-level directory `pre-1.0.0`), and
/// archives for two other platforms whose files do not exist anywhere. Any
/// attempt to fetch those fails, which is how "only the matching archive is
/// downloaded" is proved.
fn registry_with_prebuilt(
    tmp: &Path,
    format: Format,
    files: &[(&str, &[u8], u32)],
    manifest: &str,
    claimed_sha: Option<&str>,
    include_host: bool,
) -> Registry {
    let dir = tmp.join("registry");
    local_registry::init(&dir).unwrap();

    let archive = tmp.join(match format {
        Format::Zip => "pre-1.0.0-host.zip",
        Format::TarGz => "pre-1.0.0-host.tar.gz",
    });
    let sha = match format {
        Format::Zip => local_registry::write_zip(&archive, "pre-1.0.0", files).unwrap(),
        Format::TarGz => {
            local_registry::write_binary_tarball(&archive, "pre-1.0.0", files).unwrap()
        }
    };
    let sha = claimed_sha.map(str::to_string).unwrap_or(sha);

    let (patch, patch_sha) = common::write_patch(&dir, "pre", "1.0.0", &manifest_patch(manifest));

    let host = host_os();
    let host_url = file_url(&archive);
    let never_a = file_url(&tmp.join("never-fetched/pre-1.0.0-other.zip"));
    let never_b = file_url(&tmp.join("never-fetched/pre-1.0.0-riscv.tar.xz"));
    let zeros = "0".repeat(64);
    let host_conditions = [("os", host.as_str())];
    let other_conditions = [("os", other_os())];
    let riscv_conditions = [("os", "freebsd"), ("arch", "riscv64")];
    let mut archives = Vec::new();
    if include_host {
        archives.push(local_registry::Archive {
            conditions: &host_conditions,
            url: &host_url,
            sha256: &sha,
            strip_prefix: Some("pre-1.0.0"),
        });
    }
    archives.push(local_registry::Archive {
        conditions: &other_conditions,
        url: &never_a,
        sha256: &zeros,
        strip_prefix: Some("pre-1.0.0"),
    });
    archives.push(local_registry::Archive {
        conditions: &riscv_conditions,
        url: &never_b,
        sha256: &zeros,
        strip_prefix: None,
    });
    local_registry::add_archive_shim(
        &dir,
        "pre",
        "1.0.0",
        &archives,
        &[(patch.as_str(), patch_sha.as_str())],
    )
    .unwrap();
    local_registry::publish(&dir, &tmp.join("scratch")).unwrap();

    let url = local_registry::file_url(&dir);
    Registry { dir, url, archive }
}

/// The files of a prebuilt archive holding `libs` and the header.
fn archive_files(libs: &[(String, Vec<u8>)]) -> Vec<(&str, &[u8], u32)> {
    let mut files: Vec<(&str, &[u8], u32)> = vec![("include/pre.h", PRE_H.as_bytes(), 0o644)];
    for (name, bytes) in libs {
        files.push((name.as_str(), bytes.as_slice(), 0o755));
    }
    files
}

fn lib_entry(path: &Path) -> (String, Vec<u8>) {
    (format!("lib/{}", file_name(path)), fs::read(path).unwrap())
}

fn registry_app(app: &Path, call: &str) {
    common::write_app(app, "pre = \"1.0.0\"", &["pre.h"], call);
}

/// A prebuilt package from a registry, as a `.zip` holding a *shared*
/// library: only this platform's archive is fetched, the program links and
/// starts against the library in Harbour's cache, a second build fetches
/// nothing, and the lockfile pins the release.
#[test]
fn a_registry_prebuilt_package_in_a_zip_is_fetched_linked_and_run() {
    let s = scene();
    let mut libs = vec![lib_entry(&s.built.shared_lib)];
    if let Some(import) = &s.built.import_lib {
        libs.push(lib_entry(import));
    }
    let (link, runtime) = shared_lists(&s.built);
    let registry = registry_with_prebuilt(
        s.tmp.path(),
        Format::Zip,
        &archive_files(&libs),
        &prebuilt_manifest("c", &link, &runtime),
        None,
        true,
    );

    // The index marks the package as prebuilt and carries the release pin.
    let idx = fs::read_to_string(registry.dir.join("index/p/pre.idx")).unwrap();
    assert!(idx.contains("\"format_version\":2"), "{idx}");
    assert!(idx.contains("\"checksum\":\"archives-sha256:"), "{idx}");

    let app = s.tmp.path().join("app");
    registry_app(&app, "shared_value()");
    let first = common::run_in_registry(&s.home, &app, &["build"], &registry.url);
    assert!(first.success, "{}", first.text);
    assert!(first.text.contains("Fetching archive"), "{}", first.text);
    assert!(
        !first.text.contains("never-fetched"),
        "another platform's archive was touched:\n{}",
        first.text
    );
    assert_eq!(start(&app, "app"), "7");

    let lock = fs::read_to_string(app.join("Harbour.lock")).unwrap();
    assert!(lock.contains("checksum = \"archives-sha256:"), "{lock}");

    // The archive is gone, and so is everything Harbour built: the second
    // build has only the cache to go on.
    fs::remove_file(&registry.archive).unwrap();
    harbour_ok(&s.home, &app, &["clean"]);
    let second = common::run_in_registry(&s.home, &app, &["build", "--locked"], &registry.url);
    assert!(second.success, "{}", second.text);
    assert!(
        !second.text.contains("Fetching archive"),
        "a cached package must not be fetched again:\n{}",
        second.text
    );
    assert_eq!(start(&app, "app"), "7");
}

/// The same from a `.tar.gz` holding a *static* library.
#[test]
fn a_registry_prebuilt_package_in_a_tarball_is_fetched_and_linked() {
    let s = scene();
    let libs = vec![lib_entry(&s.built.static_lib)];
    let registry = registry_with_prebuilt(
        s.tmp.path(),
        Format::TarGz,
        &archive_files(&libs),
        &prebuilt_manifest("c", &[file_name(&s.built.static_lib)], &[]),
        None,
        true,
    );
    let app = s.tmp.path().join("app");
    registry_app(&app, "static_value()");
    let run = common::run_in_registry(&s.home, &app, &["build"], &registry.url);
    assert!(run.success, "{}", run.text);
    assert_eq!(start(&app, "app"), "42");
}

/// An archive whose bytes are not the ones the registry pinned is refused,
/// and nothing of it is left in the cache to be picked up later.
#[test]
fn an_archive_with_the_wrong_sha256_is_refused() {
    let s = scene();
    let libs = vec![lib_entry(&s.built.static_lib)];
    let wrong = "ab".repeat(32);
    let registry = registry_with_prebuilt(
        s.tmp.path(),
        Format::Zip,
        &archive_files(&libs),
        &prebuilt_manifest("c", &[file_name(&s.built.static_lib)], &[]),
        Some(&wrong),
        true,
    );
    let app = s.tmp.path().join("app");
    registry_app(&app, "static_value()");
    for _ in 0..2 {
        let run = common::run_in_registry(&s.home, &app, &["build"], &registry.url);
        assert!(!run.success, "{}", run.text);
        assert!(run.text.contains("archive hash mismatch"), "{}", run.text);
        assert!(run.text.contains(&wrong), "{}", run.text);
    }
    let cache = common::registry_source_cache(&s.home, &registry.url)
        .expect("the failed fetch created this registry's source cache directory");
    let left: Vec<_> = common::walk_files(&cache)
        .into_iter()
        .filter(|p| p.file_name().is_some_and(|n| n == "pre.h"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

/// A release with no archive for this platform is refused with the list of
/// what it does offer.
#[test]
fn no_archive_for_this_platform_is_refused_with_the_list() {
    let s = scene();
    let libs = vec![lib_entry(&s.built.static_lib)];
    let registry = registry_with_prebuilt(
        s.tmp.path(),
        Format::Zip,
        &archive_files(&libs),
        &prebuilt_manifest("c", &[file_name(&s.built.static_lib)], &[]),
        None,
        false,
    );
    let app = s.tmp.path().join("app");
    registry_app(&app, "static_value()");
    let run = common::run_in_registry(&s.home, &app, &["build"], &registry.url);
    assert!(!run.success, "{}", run.text);
    assert!(
        run.text.contains("no prebuilt archive of `pre` 1.0.0 for"),
        "{}",
        run.text
    );
    assert!(run.text.contains("available:"), "{}", run.text);
    assert!(
        run.text.contains(&format!("os = \"{}\"", other_os()))
            && run.text.contains("os = \"freebsd\", arch = \"riscv64\""),
        "{}",
        run.text
    );
}

/// `--locked` means something for a prebuilt package on every platform: the
/// lockfile's one checksum covers every platform's archive, so a registry
/// entry whose archive for *another* platform changed is refused here too,
/// until `harbour update` accepts it.
#[test]
fn locked_refuses_a_release_whose_archives_changed() {
    let s = scene();
    let libs = vec![lib_entry(&s.built.static_lib)];
    let registry = registry_with_prebuilt(
        s.tmp.path(),
        Format::Zip,
        &archive_files(&libs),
        &prebuilt_manifest("c", &[file_name(&s.built.static_lib)], &[]),
        None,
        true,
    );
    let app = s.tmp.path().join("app");
    registry_app(&app, "static_value()");
    let first = common::run_in_registry(&s.home, &app, &["build"], &registry.url);
    assert!(first.success, "{}", first.text);
    let locked = common::run_in_registry(&s.home, &app, &["build", "--locked"], &registry.url);
    assert!(locked.success, "{}", locked.text);

    // Someone edits the published version: the other platform's archive now
    // has a different hash. Re-indexed and committed, as a registry would.
    let shim = registry.dir.join("index/p/pre/1.0.0.toml");
    let text = fs::read_to_string(&shim).unwrap();
    let zeros = "0".repeat(64);
    assert!(text.contains(&zeros));
    fs::write(&shim, text.replacen(&zeros, &"1".repeat(64), 1)).unwrap();
    local_registry::publish(&registry.dir, &s.tmp.path().join("scratch")).unwrap();

    // A locked build does not go looking for registry changes, so this
    // machine learns of the edit the way it would in practice: something
    // else resolves against the registry and brings its copy up to date.
    let other = s.tmp.path().join("other-app");
    registry_app(&other, "static_value()");
    let fresh = common::run_in_registry(&s.home, &other, &["build"], &registry.url);
    assert!(fresh.success, "{}", fresh.text);

    for args in [&["build", "--locked"][..], &["build"][..]] {
        let run = common::run_in_registry(&s.home, &app, args, &registry.url);
        assert!(!run.success, "{args:?} must be refused:\n{}", run.text);
        assert!(
            run.text
                .contains("the prebuilt archives of `pre` 1.0.0 are not the ones recorded"),
            "{}",
            run.text
        );
        assert!(run.text.contains("harbour update"), "{}", run.text);
    }

    // Accepting it is explicit.
    let update = common::run_in_registry(&s.home, &app, &["update"], &registry.url);
    assert!(update.success, "{}", update.text);
    let run = common::run_in_registry(&s.home, &app, &["build", "--locked"], &registry.url);
    assert!(run.success, "{}", run.text);
    assert_eq!(start(&app, "app"), "42");
}

/// `harbour verify` holds a prebuilt registry package to what it can be held
/// to: it resolves, its archive is fetched, a consumer links it, its listed
/// libraries exist, and a program including its headers and linked against it
/// runs. "Builds from source" does not apply, and is not reported as missing.
#[test]
fn verify_passes_for_a_prebuilt_registry_package() {
    let s = scene();
    let libs = vec![lib_entry(&s.built.static_lib)];
    let registry = registry_with_prebuilt(
        s.tmp.path(),
        Format::Zip,
        &archive_files(&libs),
        &prebuilt_manifest("c", &[file_name(&s.built.static_lib)], &[]),
        None,
        true,
    );
    let out = common::harbour(&s.home)
        .current_dir(s.tmp.path())
        .args([
            "verify",
            "pre",
            "--registry-path",
            &registry.dir.display().to_string(),
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{text}");
    let json: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("verify did not print JSON ({e}):\n{text}"));
    let step = |name: &str| {
        json["steps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == name)
            .unwrap_or_else(|| panic!("no step {name}: {text}"))
            .clone()
    };
    for name in ["Resolve", "Fetch", "Build", "Artifacts", "Harness"] {
        assert_eq!(step(name)["status"], "passed", "{name}: {text}");
    }
    assert!(
        step("Artifacts")["message"]
            .as_str()
            .unwrap()
            .contains(&file_name(&s.built.static_lib)),
        "{text}"
    );
    assert!(
        !text.contains("recorded no translation units"),
        "a prebuilt package is not warned about compiling nothing:\n{text}"
    );
}
