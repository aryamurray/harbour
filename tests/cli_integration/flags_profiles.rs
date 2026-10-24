//! `harbour flags` parity with the real command, `[profile]` flags, named profiles, `c_std`.
#![allow(unused_imports)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::prelude::*;
use predicates::prelude::*;
use tempfile::TempDir;

use harbour::test_support::fixtures::local_registry;

use crate::common::*;

/// Compiler flags reach the compiler in the order the manifest wrote them.
///
/// This is the sharpest available proof, because the two halves differ only
/// in the order of two flags and the expected outcomes are opposite:
///
/// - `["-Wall", "-Wno-error", "-Werror"]` must **fail** the build. The
///   author put `-Werror` last, so warnings are errors, and the source has
///   an unused variable.
/// - `["-Wall", "-Werror", "-Wno-error"]` must **succeed** and produce a
///   working binary. `-Wno-error` last downgrades it again.
///
/// Harbour used to sort the effective `cflags` before handing them over,
/// which collates to `-Wall -Werror -Wno-error` either way: both manifests
/// built, and the first one -- the one that says "treat warnings as errors"
/// -- was the one silently inverted. A test that only checked the second
/// half would still pass under the sort, which is why both directions are
/// asserted here.
#[cfg(not(target_env = "msvc"))]
#[test]
fn test_cflag_order_is_the_manifest_order_and_last_wins() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "flagorder"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let dir = tmp.path().join("flagorder");

    // An unused variable: diagnosed by -Wall, fatal under -Werror.
    fs::write(
        dir.join("src/main.c"),
        "#include <stdio.h>\n\
         int main(void) { int unused_here = 1; printf(\"ran\\n\"); return 0; }\n",
    )
    .unwrap();

    let manifest_with = |cflags: &str| {
        format!(
            "[package]\n\
             name = \"flagorder\"\n\
             version = \"0.1.0\"\n\
             \n\
             [targets.flagorder]\n\
             kind = \"exe\"\n\
             sources = [\"src/main.c\"]\n\
             \n\
             [targets.flagorder.private]\n\
             cflags = {cflags}\n"
        )
    };

    // -Werror last: the author asked for a hard failure and must get one.
    fs::write(
        dir.join("Harbour.toml"),
        manifest_with("[\"-Wall\", \"-Wno-error\", \"-Werror\"]"),
    )
    .unwrap();

    let strict = harbour_run(&home, &dir, &["build"]);
    assert!(
        !strict.status.success(),
        "`cflags = [\"-Wall\", \"-Wno-error\", \"-Werror\"]` means warnings are \
         errors. A successful build here means the flags reached the compiler in \
         some other order and `-Wno-error` won.\n{strict}"
    );
    assert!(
        strict.combined().contains("unused"),
        "and it must fail on the unused variable specifically, not on something \
         unrelated\n{strict}"
    );

    // The same two flags the other way round: now it must build and run.
    fs::write(
        dir.join("Harbour.toml"),
        manifest_with("[\"-Wall\", \"-Werror\", \"-Wno-error\"]"),
    )
    .unwrap();

    build_ok(&home, &dir);
    assert_eq!(
        run_built_exe(&dir, "flagorder").out(),
        "ran",
        "with -Wno-error last the warning is not fatal, and the binary must work"
    );
}

/// `harbour flags` reports `cflags` in the same order the compiler gets
/// them.
///
/// Reading the fold is not enough to know this: the command and the build
/// used to be two different folds, and this ordering was one of the four
/// ways they disagreed.
#[cfg(not(target_env = "msvc"))]
#[test]
fn test_flags_command_reports_cflags_in_manifest_order() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "flagecho"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let dir = tmp.path().join("flagecho");

    fs::write(
        dir.join("Harbour.toml"),
        "[package]\n\
         name = \"flagecho\"\n\
         version = \"0.1.0\"\n\
         \n\
         [targets.flagecho]\n\
         kind = \"exe\"\n\
         sources = [\"src/main.c\"]\n\
         \n\
         [targets.flagecho.private]\n\
         cflags = [\"-Wall\", \"-Wno-error\", \"-Werror\"]\n",
    )
    .unwrap();

    let reported = harbour_run(&home, &dir, &["flags", "flagecho"]).success();
    let order: Vec<&str> = reported
        .combined()
        .lines()
        .filter_map(|l| {
            let flag = l.split_whitespace().next()?;
            ["-Wall", "-Wno-error", "-Werror"]
                .into_iter()
                .find(|f| *f == flag)
        })
        .collect();

    assert_eq!(
        order,
        vec!["-Wall", "-Wno-error", "-Werror"],
        "`harbour flags` must print the manifest's order, which is also the \
         compiler's order\n{reported}"
    );
}

/// `harbour flags --compile` is exactly the compile command, minus the
/// source and output operands.
///
/// The fixture is deliberately not minimal. It has a dependency whose
/// public surface must propagate, a second dependency marked
/// `compile = "private"` whose surface must *not*, a `target = "..."`
/// naming one of two library targets in the same package, a flag both
/// packages ask for (so deduplication is exercised), and a conditional
/// block for the host OS. Every one of those was a way the two folds used
/// to disagree.
#[cfg(all(not(windows), not(target_env = "msvc")))]
#[test]
fn test_flags_matches_the_real_compile_command() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let (shim, records) = install_cc_recorder(tmp.path());

    let host_os = if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };

    // A dependency with two library targets, so `target = "..."` is load
    // bearing: "the first library target" is a coin flip across runs.
    let lib = tmp.path().join("paritylib");
    fs::create_dir_all(lib.join("src")).unwrap();
    fs::create_dir_all(lib.join("include")).unwrap();
    fs::write(lib.join("src/wanted.c"), "int wanted(void) { return 7; }\n").unwrap();
    fs::write(lib.join("src/other.c"), "int other(void) { return 9; }\n").unwrap();
    fs::write(lib.join("include/wanted.h"), "int wanted(void);\n").unwrap();
    fs::write(
        lib.join("Harbour.toml"),
        "[package]\n\
         name = \"paritylib\"\n\
         version = \"1.0.0\"\n\
         \n\
         [targets.wanted]\n\
         kind = \"staticlib\"\n\
         sources = [\"src/wanted.c\"]\n\
         \n\
         [targets.wanted.public]\n\
         include_dirs = [\"include\"]\n\
         defines = [\"FROM_WANTED=1\"]\n\
         cflags = [\"-fno-common\"]\n\
         \n\
         [targets.other]\n\
         kind = \"staticlib\"\n\
         sources = [\"src/other.c\"]\n\
         \n\
         [targets.other.public]\n\
         defines = [\"FROM_OTHER=1\"]\n",
    )
    .unwrap();

    // A dependency whose compile surface must not reach the consumer.
    let hidden = tmp.path().join("parityhidden");
    fs::create_dir_all(hidden.join("src")).unwrap();
    fs::write(
        hidden.join("src/h.c"),
        "int hidden_thing(void) { return 1; }\n",
    )
    .unwrap();
    fs::write(
        hidden.join("Harbour.toml"),
        "[package]\n\
         name = \"parityhidden\"\n\
         version = \"1.0.0\"\n\
         \n\
         [targets.parityhidden]\n\
         kind = \"staticlib\"\n\
         sources = [\"src/h.c\"]\n\
         \n\
         [targets.parityhidden.public]\n\
         defines = [\"MUST_NOT_REACH_CONSUMER=1\"]\n",
    )
    .unwrap();

    let app = tmp.path().join("parityapp");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(
        app.join("src/main.c"),
        "#include <stdio.h>\n\
         #include \"wanted.h\"\n\
         #ifdef MUST_NOT_REACH_CONSUMER\n\
         #error \"a compile = private dependency reached the consumer\"\n\
         #endif\n\
         int main(void) { printf(\"%d\\n\", wanted()); return 0; }\n",
    )
    .unwrap();
    fs::write(
        app.join("Harbour.toml"),
        format!(
            "[package]\n\
             name = \"parityapp\"\n\
             version = \"0.1.0\"\n\
             \n\
             [dependencies]\n\
             paritylib = {{ path = \"../paritylib\" }}\n\
             parityhidden = {{ path = \"../parityhidden\" }}\n\
             \n\
             [targets.parityapp]\n\
             kind = \"exe\"\n\
             sources = [\"src/main.c\"]\n\
             \n\
             [targets.parityapp.deps]\n\
             paritylib = {{ target = \"wanted\" }}\n\
             parityhidden = {{ compile = \"private\", link = \"public\" }}\n\
             \n\
             [targets.parityapp.private]\n\
             defines = [\"APP_PRIVATE=1\"]\n\
             cflags = [\"-Wall\", \"-fno-common\"]\n\
             \n\
             [[targets.parityapp.when]]\n\
             os = \"{host_os}\"\n\
             defines = [\"HOST_MATCHED=1\"]\n"
        ),
    )
    .unwrap();

    let build =
        harbour_run_env(&home, &app, &["build"], &[("CC", shim.to_str().unwrap())]).success();

    // The consumer's own translation unit -- the one whose flags the two
    // folds disagreed about. Canonicalized: Harbour resolves the manifest
    // path, and on macOS the temp dir lives under `/var`, a symlink to
    // `/private/var`, so the uncanonicalized path matches nothing.
    let main_c = app
        .join("src/main.c")
        .canonicalize()
        .unwrap()
        .display()
        .to_string();
    let argvs = recorded_argvs(&records);
    let compile: Vec<String> = argvs
        .into_iter()
        .find(|a| a.contains(&main_c))
        .unwrap_or_else(|| {
            panic!("no recorded compile of {main_c}; the CC wrapper never ran\n{build}")
        });

    // Strip the parts that are not flags: `-c` leads, and the source and
    // output operands trail (see `GccToolchain::compile_command`).
    assert_eq!(
        compile.first().map(String::as_str),
        Some("-c"),
        "{compile:?}"
    );
    let tail = compile.len() - 3;
    assert_eq!(compile[tail], main_c, "{compile:?}");
    assert_eq!(compile[tail + 1], "-o", "{compile:?}");
    let argv_flags: Vec<String> = compile[1..tail].to_vec();

    let reported = harbour_run(&home, &app, &["flags", "parityapp", "--compile"]).success();
    let printed = reported_flags(&reported.stdout);

    assert_eq!(
        printed, argv_flags,
        "`harbour flags` must print exactly the flags the compiler was \
         handed, in the same order.\n\
         reported: {printed:#?}\n\
         actual argv: {argv_flags:#?}\n{reported}"
    );

    // And the things that made the old copy wrong, asserted directly so a
    // regression names itself rather than showing up as a diff.
    assert!(
        printed.iter().any(|f| f == "-DFROM_WANTED=1"),
        "the named dependency target's public surface must be there: {printed:#?}"
    );
    assert!(
        !printed.iter().any(|f| f.contains("FROM_OTHER")),
        "the dependency's *other* library target must not contribute: {printed:#?}"
    );
    assert!(
        !printed
            .iter()
            .any(|f| f.contains("MUST_NOT_REACH_CONSUMER")),
        "a compile = \"private\" dependency must not appear: {printed:#?}"
    );
    assert_eq!(
        printed.iter().filter(|f| *f == "-fno-common").count(),
        1,
        "a flag both packages asked for reaches the compiler once, so it must \
         be printed once: {printed:#?}"
    );

    assert_eq!(
        run_built_exe(&app, "parityapp").out(),
        "7",
        "and the whole thing still has to build and run"
    );
}

/// `harbour flags --link` is exactly the link command's flags.
///
/// Separate from the compile assertion because the failure modes are
/// different: this is the one where a `-L` for a dependency's artifact
/// directory used to be reported, and its *absence* is deliberate -- every
/// `-L` also applies to the driver's implicit libraries, so a dependency
/// named `c` on the search path lets its `libc.a` shadow the system one.
#[cfg(all(not(windows), not(target_env = "msvc")))]
#[test]
fn test_flags_matches_the_real_link_command() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let (shim, records) = install_cc_recorder(tmp.path());

    let lib = tmp.path().join("linklib");
    fs::create_dir_all(lib.join("src")).unwrap();
    fs::write(lib.join("src/l.c"), "int lib_answer(void) { return 5; }\n").unwrap();
    fs::write(
        lib.join("Harbour.toml"),
        "[package]\n\
         name = \"linklib\"\n\
         version = \"1.0.0\"\n\
         \n\
         [targets.linklib]\n\
         kind = \"staticlib\"\n\
         sources = [\"src/l.c\"]\n\
         \n\
         [targets.linklib.public]\n\
         libs = [\"m\"]\n",
    )
    .unwrap();

    let app = tmp.path().join("linkapp");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(
        app.join("src/main.c"),
        "#include <stdio.h>\n\
         int lib_answer(void);\n\
         int main(void) { printf(\"%d\\n\", lib_answer()); return 0; }\n",
    )
    .unwrap();
    fs::write(
        app.join("Harbour.toml"),
        "[package]\n\
         name = \"linkapp\"\n\
         version = \"0.1.0\"\n\
         \n\
         [dependencies]\n\
         linklib = { path = \"../linklib\" }\n\
         \n\
         [targets.linkapp]\n\
         kind = \"exe\"\n\
         sources = [\"src/main.c\"]\n\
         \n\
         [targets.linkapp.deps]\n\
         linklib = \"linklib\"\n",
    )
    .unwrap();

    let build =
        harbour_run_env(&home, &app, &["build"], &[("CC", shim.to_str().unwrap())]).success();

    // Canonicalized for the same reason as the compile test.
    let output = built_exe_path(&app, "linkapp")
        .canonicalize()
        .unwrap()
        .display()
        .to_string();
    let argvs = recorded_argvs(&records);
    let link: Vec<String> = argvs
        .into_iter()
        .find(|a| a.contains(&output) && !a.iter().any(|t| t == "-c"))
        .unwrap_or_else(|| panic!("no recorded link of {output}\n{build}"));

    // Link argv is `-o <output> <objects...> <flags...>` (see
    // `GccToolchain::link_exe_command`). Everything after the last object
    // file is a flag.
    let after_output = link
        .iter()
        .position(|t| *t == output)
        .expect("output operand")
        + 1;
    let argv_flags: Vec<String> = link[after_output..]
        .iter()
        .filter(|t| !t.ends_with(".o"))
        .cloned()
        .collect();

    let reported = harbour_run(&home, &app, &["flags", "linkapp", "--link"]).success();
    let printed = reported_flags(&reported.stdout);

    assert_eq!(
        printed, argv_flags,
        "`harbour flags --link` must print exactly the flags the linker was \
         handed, in the same order.\n\
         reported: {printed:#?}\n\
         actual argv: {argv_flags:#?}\n{reported}"
    );
    assert!(
        printed.iter().any(|f| f.ends_with("liblinklib.a")),
        "the dependency archive is passed by absolute path: {printed:#?}"
    );
    assert!(
        !printed
            .iter()
            .any(|f| f.starts_with("-L") && f.contains("deps")),
        "and deliberately without a matching -L, so a dependency named `c` \
         cannot shadow the system libc: {printed:#?}"
    );

    assert_eq!(run_built_exe(&app, "linkapp").out(), "5");
}

/// A compiler warning on a *successful* compile must reach the user.
///
/// `NativeBuilder::compile` read `output.stderr` only inside
/// `if !output.status.success()`, so every diagnostic from a compile that
/// exited 0 was discarded. `harbour new` scaffolds `-Wall -Wextra` into every
/// generated project and those flags do reach the compiler -- verified
/// separately against the real `cc` argv -- so the effect was that the
/// scaffold's warning flags were decorative: enabled, fired, and thrown away.
///
/// The source below is written so the two flags catch one diagnostic each:
/// `-Wall` gives `-Wunused-variable`, `-Wextra` gives `-Wunused-parameter`.
/// Neither is reported without those flags, which is what makes this a test
/// of the plumbing rather than of the compiler's defaults.
#[test]
fn compiler_warnings_on_a_successful_build_reach_the_user() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");

    fs::write(
        app_dir.join("src/main.c"),
        r#"#include <stdio.h>
static int helper(int used, int ignored) { return used; }
int main(void) {
    int never_read = 1;
    printf("%d\n", helper(2, 3));
    return 0;
}
"#,
    )
    .unwrap();

    let out = harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .success()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();

    // The content assertion is deliberately not made under MSVC.
    //
    // The mechanism is confirmed there -- CI showed `cl`'s own
    // `D9002` command-line warnings arriving on stderr, which they did not
    // before this change. What is *not* established is which stream `cl`
    // puts file-level diagnostics (`C4189`, `C4100`) on; they did not appear
    // on stderr in that run, and I have no MSVC host to determine whether
    // they go to stdout instead. Asserting a guess here would either fail
    // spuriously or pass vacuously, and a vacuous assertion is what this
    // very test exists to prevent. Tracked separately; the regression this
    // test guards was found and is reproducible on the Unix toolchains.
    if !cfg!(target_env = "msvc") {
        for needle in ["warning", "unused variable", "unused parameter"] {
            assert!(
                stderr.contains(needle),
                "the build succeeded and the compiler emitted diagnostics, so \
                 `{needle}` must appear on stderr -- otherwise the scaffold's \
                 warning flags are decorative.\nstderr:\n{stderr}"
            );
        }
    }

    // And the build still succeeded: warnings are surfaced, not promoted.
    assert!(
        built_exe_path(&app_dir, "app").exists(),
        "surfacing warnings must not fail the build"
    );
}

/// `compile_commands.json` must be generated from the same options as the
/// build, C++ language flags included.
///
/// `BuildPlan::emit_compile_commands` used to build its own `CompileInput`
/// and pass `cxx_opts: None`, while `NativeBuilder::compile` passed the real
/// options. Every C++ flag -- `-std=`, `-fno-exceptions`, `-fno-rtti`,
/// `-stdlib=` -- is emitted inside a `if let Some(opts) = cxx_opts` in the
/// toolchain backends, so none of them appeared in the database. clangd read
/// the file as exceptions-enabled C++ at the default standard while the
/// compiler was given `-std=c++17 -fno-exceptions -fno-rtti`.
///
/// The witness for "the compiler really got these" is the produced *binary*,
/// not the flag list: it prints what the preprocessor saw, so the claim is
/// established independently of what the database says.
#[test]
fn compile_commands_carries_the_same_cxx_flags_as_the_build() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let app_dir = tmp.path().join("cpptest");
    fs::create_dir_all(app_dir.join("src")).unwrap();

    fs::write(
        app_dir.join("Harbour.toml"),
        r#"[package]
name = "cpptest"
version = "0.1.0"

[build]
cpp_std = "17"
exceptions = false
rtti = false

[targets.cpptest]
kind = "exe"
lang = "c++"
sources = ["src/main.cpp"]
"#,
    )
    .unwrap();
    // The probe has to mean the same thing under both toolchains, and the
    // obvious spellings do not:
    //
    // - `__cplusplus` reports `199711` under MSVC no matter which `/std:` is
    //   in force, unless `/Zc:__cplusplus` is passed (which Harbour does not
    //   pass). `_MSVC_LANG` carries the real value and is the documented way
    //   to read it.
    // - `__EXCEPTIONS` is a GCC/clang macro. MSVC never defines it, so the
    //   original probe reported `exceptions=off` under MSVC whether or not
    //   `/EHsc` was passed -- it would have "passed" for the wrong reason.
    //   MSVC's spelling is `_CPPUNWIND`.
    // - RTTI is `__GXX_RTTI` on GCC/clang and `_CPPRTTI` on MSVC.
    fs::write(
        app_dir.join("src/main.cpp"),
        r#"#include <cstdio>

#ifdef _MSVC_LANG
#  define HB_STD _MSVC_LANG
#else
#  define HB_STD __cplusplus
#endif

#if defined(__EXCEPTIONS) || defined(_CPPUNWIND)
#  define HB_EXCEPTIONS "on"
#else
#  define HB_EXCEPTIONS "off"
#endif

#if defined(__GXX_RTTI) || defined(_CPPRTTI)
#  define HB_RTTI "on"
#else
#  define HB_RTTI "off"
#endif

int main() {
    std::printf("std=%ld\n", (long)HB_STD);
    std::printf("exceptions=%s\n", HB_EXCEPTIONS);
    std::printf("rtti=%s\n", HB_RTTI);
    return 0;
}
"#,
    )
    .unwrap();

    harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .success();

    // What the compiler actually did, read off the artifact it produced.
    let exe = built_exe_path(&app_dir, "cpptest");
    let out = Command::new(&exe).output().unwrap();
    assert!(out.status.success(), "the built binary must run");
    let reported = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        reported.contains("std=201703"),
        "the compile must have selected C++17: {reported}"
    );
    assert!(
        reported.contains("exceptions=off"),
        "the manifest sets `exceptions = false`, so the compile must have \
         disabled them: {reported}"
    );
    assert!(
        reported.contains("rtti=off"),
        "the manifest sets `rtti = false`, so the compile must have disabled \
         it: {reported}"
    );

    // What the database claims it did. MSVC spells all three differently;
    // these spellings were read off `MsvcToolchain::compile_command`'s actual
    // output rather than guessed (`/EHs-c-`, not `/EHsc-`).
    let cc = fs::read_to_string(app_dir.join(".harbour/compile_commands.json")).unwrap();
    let expected: [&str; 3] = if cfg!(target_env = "msvc") {
        ["/std:c++17", "/EHs-c-", "/GR-"]
    } else {
        ["-std=c++17", "-fno-exceptions", "-fno-rtti"]
    };
    for flag in expected {
        assert!(
            cc.contains(flag),
            "the compiler received `{flag}` (the binary above proves it), so \
             compile_commands.json must list it too, or clangd parses this \
             file as a different dialect than the build compiles it as. \
             compile_commands.json:\n{cc}"
        );
    }
}

/// A project whose source can tell whether it was optimised, and by how much
/// debug information it was compiled with.
fn profile_fixture(tmp: &std::path::Path) -> PathBuf {
    let dir = tmp.join("profileflags");
    fs::create_dir_all(dir.join("src")).unwrap();
    // `__OPTIMIZE__` is defined by GCC and clang only when the optimiser is
    // actually enabled -- it is evidence about codegen, not about the command
    // line. MSVC defines no equivalent, so there the binary just runs.
    fs::write(
        dir.join("src/main.c"),
        "#include <stdio.h>\n\
         int main(void) {\n\
         #if defined(__OPTIMIZE__)\n\
             puts(\"optimised\");\n\
         #else\n\
             puts(\"unoptimised\");\n\
         #endif\n\
             return 0;\n\
         }\n",
    )
    .unwrap();
    fs::write(
        dir.join("Harbour.toml"),
        "[package]\n\
         name = \"profileflags\"\n\
         version = \"0.1.0\"\n\
         \n\
         [targets.profileflags]\n\
         kind = \"exe\"\n\
         sources = [\"src/main.c\"]\n",
    )
    .unwrap();
    dir
}

/// Every file under `root` whose extension is `ext`, searched recursively.
///
/// Object and program-database layout differs per toolchain, so the tests
/// that need "the objects this build produced" find them rather than
/// hard-coding a path -- which would also hard-code a separator.
fn files_with_extension(root: &std::path::Path, ext: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(next) = stack.pop() {
        let Ok(entries) = fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some(ext) {
                found.push(path);
            }
        }
    }
    found
}

/// Whether `haystack` contains `needle`, for looking at binaries.
fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Every argv `compile_commands.json` recorded, flattened.
///
/// The database is generated from `BuildContext::compile_spec` -- the same
/// call `NativeBuilder::compile` makes -- so this is the argv the compiler
/// was handed, not a reconstruction of it.
fn recorded_compile_args(app_dir: &std::path::Path) -> String {
    fs::read_to_string(app_dir.join(".harbour").join("compile_commands.json")).unwrap()
}

/// The default profiles must reach the real compiler in the syntax that
/// compiler understands, and must produce a binary that behaves accordingly.
#[test]
fn profile_flags_reach_the_real_compiler_in_the_toolchains_own_syntax() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let dir = profile_fixture(tmp.path());

    // `[profile.debug]`: opt_level = "0", debug = "2".
    let debug = harbour_run(&home, &dir, &["build"]).success();
    let debug_args = recorded_compile_args(&dir);

    // `[profile.release]`: opt_level = "3", debug = "0".
    let release = harbour_run(&home, &dir, &["build", "--release"]).success();
    let release_args = recorded_compile_args(&dir);

    let msvc = cfg!(target_env = "msvc");

    let (no_opt, max_opt, debug_info) = if msvc {
        ("/Od", "/O2", "/Z7")
    } else {
        ("-O0", "-O3", "-g3")
    };

    assert!(
        debug_args.contains(no_opt),
        "the debug profile asks for opt_level = \"0\"; the compiler must be \
         told so in its own syntax.\n{debug_args}"
    );
    assert!(
        debug_args.contains(debug_info),
        "the debug profile asks for debug = \"2\"; without this flag the \
         build carries no debug information at all -- which is what every \
         Windows build did.\n{debug_args}"
    );
    assert!(
        release_args.contains(max_opt),
        "the release profile asks for opt_level = \"3\"; without this the \
         release build is unoptimised.\n{release_args}"
    );
    assert!(
        !release_args.contains(debug_info),
        "the release profile asks for debug = \"0\"; nothing should request \
         debug information.\n{release_args}"
    );

    // The other half of the same claim: nothing in the *other* toolchain's
    // syntax may appear. On `main` a `cl` command line carried `-O3`.
    let wrong: &[&str] = if msvc {
        &["-O0", "-O3", "-g3", "-g "]
    } else {
        &["/Od", "/O2", "/Z7"]
    };
    for flag in wrong {
        for (which, args) in [("debug", &debug_args), ("release", &release_args)] {
            assert!(
                !args.contains(flag),
                "`{flag}` is not this compiler's syntax and would be ignored \
                 with a warning at best ({which} build):\n{args}"
            );
        }
    }

    // The diagnostic that was being thrown away. Warnings from successful
    // compiles are surfaced now, so if `cl` is ignoring an option we would
    // see it here.
    for build in [&debug, &release] {
        let out = build.combined();
        assert!(
            !out.contains("D9002") && !out.contains("ignoring unknown option"),
            "the compiler reported that it ignored an option we passed:\n{out}"
        );
    }

    // And the effect, where the compiler will tell us: `__OPTIMIZE__` is
    // defined from codegen settings, not from the command line text.
    if !msvc {
        // Debug information is in the object or it is not there at all. The
        // section is named `.debug_info` in ELF and `__debug_info` in
        // Mach-O, so the common substring covers Linux and macOS both.
        let debug_objects = files_with_extension(&target_dir(&dir).join("debug"), "o");
        assert!(
            !debug_objects.is_empty(),
            "no object files under the debug tree to inspect"
        );
        for obj in &debug_objects {
            let bytes = fs::read(obj).unwrap();
            assert!(
                contains_bytes(&bytes, b"debug_info"),
                "`debug = \"2\"` must put real debug information in {}, not \
                 just a flag on the command line",
                obj.display()
            );
        }
        for obj in files_with_extension(&target_dir(&dir).join("release"), "o") {
            let bytes = fs::read(&obj).unwrap();
            assert!(
                !contains_bytes(&bytes, b"debug_info"),
                "`debug = \"0\"` asked for none, yet {} carries debug \
                 information",
                obj.display()
            );
        }

        assert_eq!(
            run_built_exe(&dir, "profileflags").out(),
            "unoptimised",
            "the debug profile sets opt_level = \"0\", so the optimiser must \
             be off in the binary, not merely off on paper"
        );
        assert_eq!(
            run_built_exe_in(&dir, "release", "profileflags").out(),
            "optimised",
            "the release profile sets opt_level = \"3\", so the optimiser \
             must actually have run"
        );
    } else {
        // MSVC has no `__OPTIMIZE__`; all we can assert here is that both
        // binaries work. The optimisation itself is pinned by the argv
        // assertions above plus `cl` accepting the flag without a D9002.
        run_built_exe(&dir, "profileflags").success();
        run_built_exe_in(&dir, "release", "profileflags").success();
    }
}

/// The Windows artifact the old code could never produce: a PDB.
///
/// `/Z7` puts CodeView records in each `.obj` and `/DEBUG` makes the linker
/// turn them into a program database beside the image. With neither flag
/// emitted -- `-g` being ignored -- a Windows debug build produced a binary
/// with no symbols and no PDB, which is to say one you cannot set a
/// breakpoint in. The release profile asks for `debug = "0"`, so it must
/// *not* produce one; that direction is what makes this test non-vacuous.
#[cfg(target_env = "msvc")]
#[test]
fn a_windows_debug_build_produces_a_pdb_and_a_release_build_does_not() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let dir = profile_fixture(tmp.path());

    harbour_run(&home, &dir, &["build"]).success();
    let debug_pdbs = files_with_extension(&target_dir(&dir).join("debug"), "pdb");
    assert!(
        !debug_pdbs.is_empty(),
        "`debug = \"2\"` must produce debug information that survives into \
         the image; no .pdb exists under {}",
        target_dir(&dir).join("debug").display()
    );

    harbour_run(&home, &dir, &["build", "--release"]).success();
    let release_pdbs = files_with_extension(&target_dir(&dir).join("release"), "pdb");
    assert!(
        release_pdbs.is_empty(),
        "`debug = \"0\"` asked for no debug information, but a .pdb was \
         written anyway: {release_pdbs:?}"
    );
}

/// The control for the `D9002` assertion above.
///
/// "No `D9002` in the output" is only worth something if a `D9002` would
/// have shown up. This hands `cl` the exact flag `profile_cflags` used to
/// emit for `opt_level = "3"` -- `-O3`, via `cflags`, where a verbatim flag
/// is the user's business -- and asserts that the compiler says it ignored
/// it *and* that the build still succeeds. That pairing is the whole defect
/// in one test: an option that does nothing, a warning that says so, and a
/// green build.
#[cfg(target_env = "msvc")]
#[test]
fn cl_reports_a_gcc_style_flag_as_ignored_and_builds_anyway() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let dir = profile_fixture(tmp.path());

    let manifest = fs::read_to_string(dir.join("Harbour.toml")).unwrap();
    fs::write(
        dir.join("Harbour.toml"),
        format!("{manifest}\n[profile.debug]\ncflags = [\"-O3\"]\n"),
    )
    .unwrap();

    let build = harbour_run(&home, &dir, &["build"]).success();
    let out = build.combined();
    assert!(
        out.contains("D9002"),
        "`cl` ignores `-O3` and says so; if this does not appear, either the \
         compiler's warnings are being discarded again or the assertion in \
         `profile_flags_reach_the_real_compiler_in_the_toolchains_own_syntax` \
         is vacuous.\n{out}"
    );
    assert!(
        out.contains("-O3"),
        "the warning must name the option that was ignored\n{out}"
    );
}

/// A profile setting a toolchain cannot express must stop the build, not
/// vanish from the command line.
///
/// `sanitizers = ["thread"]` used to become `-fsanitize=thread`, which `cl`
/// ignored with a `D9002` nobody was reading -- a build that reported
/// success and was not sanitized. On GCC and clang the same manifest is
/// legitimate, so this is deliberately two different expectations.
#[test]
fn a_sanitizer_the_toolchain_lacks_fails_the_build_rather_than_disappearing() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let dir = profile_fixture(tmp.path());

    let manifest = fs::read_to_string(dir.join("Harbour.toml")).unwrap();
    fs::write(
        dir.join("Harbour.toml"),
        format!("{manifest}\n[profile.debug]\nsanitizers = [\"thread\"]\n"),
    )
    .unwrap();

    let build = harbour_run(&home, &dir, &["build"]);

    if cfg!(target_env = "msvc") {
        assert!(
            !build.status.success(),
            "MSVC implements no thread sanitizer; the build must say so \
             instead of quietly producing an unsanitized binary\n{build}"
        );
        assert!(
            build.combined().contains("MSVC does not implement"),
            "the error must name the reason\n{build}"
        );
    } else {
        // ThreadSanitizer exists here. Not every platform can *run* the
        // result (and macOS x86-only support makes running it unportable),
        // so this asserts only that the flag was accepted and recorded.
        build.success();
        let args = recorded_compile_args(&dir);
        assert!(
            args.contains("-fsanitize=thread"),
            "a sanitizer this toolchain supports must reach the compiler\n{args}"
        );
    }
}

/// A misspelled profile value must be rejected, not pasted into a flag.
///
/// `opt_level = "fastest"` used to be formatted straight into `-Ofastest`
/// and handed to the compiler; on MSVC it became a `D9002` and was ignored
/// outright.
#[test]
fn an_unknown_opt_level_is_an_error_with_the_valid_values_listed() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let dir = profile_fixture(tmp.path());

    let manifest = fs::read_to_string(dir.join("Harbour.toml")).unwrap();
    fs::write(
        dir.join("Harbour.toml"),
        format!("{manifest}\n[profile.debug]\nopt_level = \"fastest\"\n"),
    )
    .unwrap();

    let build = harbour_run(&home, &dir, &["build"]);
    assert!(
        !build.status.success(),
        "`opt_level = \"fastest\"` is not a thing; the build must not \
         proceed as if it were\n{build}"
    );
    assert!(
        build.combined().contains("fastest") && build.combined().contains("0, 1, 2, 3, s, z"),
        "the error must name the bad value and the good ones\n{build}"
    );
}

/// `c_std` reaches the compiler, the compile database, and the cache key.
///
/// This is the test the field never had. `c_std` parsed, validated, was
/// documented in `MANIFEST.md` as working, and was read by nothing: the only
/// `-std=` Harbour emitted came from `CxxOptions`, so a C package pinning
/// `c_std = "99"` for portability got the compiler's default in silence.
///
/// Every assertion here is on observed behaviour rather than on a flag
/// string alone:
///
/// * the built program prints `__STDC_VERSION__`, so the *compiler* has to
///   have agreed, not merely been handed an argument;
/// * it also prints whether `__STRICT_ANSI__` is defined, which is the only
///   thing that distinguishes `gnu99` from `c99` -- and the difference real
///   packages depend on (`typeof`, statement expressions, `asm`);
/// * "it entered the fingerprint" is proved by the compiler *not being
///   invoked* on an unchanged rebuild and being invoked again when only
///   `c_std` changed. A cache key that reads right but does not invalidate
///   would pass a hash-shaped assertion and fail this one.
#[cfg(all(not(windows), not(target_env = "msvc")))]
#[test]
fn test_c_std_reaches_the_compiler_the_compile_database_and_the_cache_key() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let (shim, records) = install_cc_recorder(tmp.path());

    let app = tmp.path().join("cstdapp");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(
        app.join("src/main.c"),
        "#include <stdio.h>\n\
         int main(void) {\n\
         #if defined(__STRICT_ANSI__)\n\
             const char *dialect = \"strict\";\n\
         #else\n\
             const char *dialect = \"gnu\";\n\
         #endif\n\
             printf(\"%ld %s\\n\", (long)__STDC_VERSION__, dialect);\n\
             return 0;\n\
         }\n",
    )
    .unwrap();

    let manifest = |c_std: &str| {
        format!(
            "[package]\n\
             name = \"cstdapp\"\n\
             version = \"0.1.0\"\n\
             \n\
             [targets.cstdapp]\n\
             kind = \"exe\"\n\
             sources = [\"src/main.c\"]\n\
             {c_std}"
        )
    };

    let main_c = app
        .join("src/main.c")
        .canonicalize()
        .unwrap()
        .display()
        .to_string();

    // Every recorded compile of `main.c` since the records were last
    // cleared. Empty means the object was reused -- which is the only
    // evidence that distinguishes "the fingerprint changed" from "the
    // fingerprint is ignored".
    let compiles_of_main = |records: &std::path::Path| -> Vec<Vec<String>> {
        recorded_argvs(records)
            .into_iter()
            .filter(|argv| argv.contains(&main_c))
            .collect()
    };
    let clear = |records: &std::path::Path| {
        fs::remove_dir_all(records).unwrap();
        fs::create_dir_all(records).unwrap();
    };

    let build = |records: &std::path::Path| -> RunLog {
        clear(records);
        harbour_run_env(&home, &app, &["build"], &[("CC", shim.to_str().unwrap())]).success()
    };

    // ---- `c_std = "99"`: strict C99, all the way to the running binary.
    fs::write(app.join("Harbour.toml"), manifest("c_std = \"99\"\n")).unwrap();
    let log = build(&records);
    let argvs = compiles_of_main(&records);
    assert_eq!(argvs.len(), 1, "expected one compile of main.c\n{log}");
    assert!(
        argvs[0].contains(&"-std=c99".to_string()),
        "`c_std = \"99\"` must put `-std=c99` on the real compiler argv, \
         not merely parse: {:?}\n{log}",
        argvs[0]
    );
    assert_eq!(
        run_built_exe(&app, "cstdapp").out(),
        "199901 strict",
        "the compiler has to have *honoured* the standard: the program \
         prints its own __STDC_VERSION__ and whether __STRICT_ANSI__ is \
         defined"
    );

    // clangd must be told the same dialect the build used, or it reports
    // diagnostics for a language the build never compiled.
    let cc_db = fs::read_to_string(app.join(".harbour/compile_commands.json")).unwrap();
    assert!(
        cc_db.contains("-std=c99"),
        "compile_commands.json must carry the same `-std=` the build used:\n{cc_db}"
    );

    // `harbour flags` exists to be authoritative, so it has to show it too.
    let reported = harbour_run(&home, &app, &["flags", "cstdapp", "--compile"]).success();
    let printed = reported_flags(&reported.stdout);
    let tail = argvs[0].len() - 3;
    assert_eq!(
        printed,
        argvs[0][1..tail].to_vec(),
        "`harbour flags` must print exactly the flags the compiler was \
         handed, in order.\n{reported}"
    );

    // ---- An unchanged rebuild must not recompile. Without this, the next
    // assertion would pass even if the fingerprint ignored `c_std` and
    // simply recompiled everything every time.
    let log = build(&records);
    assert!(
        compiles_of_main(&records).is_empty(),
        "nothing changed, so main.c must not be recompiled; otherwise the \
         `c_std` invalidation below proves nothing\n{log}"
    );

    // ---- Only `c_std` changes: c99 -> gnu99. Same ISO version, different
    // dialect, and the compiler must be run again to pick it up.
    fs::write(app.join("Harbour.toml"), manifest("c_std = \"gnu99\"\n")).unwrap();
    let log = build(&records);
    let argvs = compiles_of_main(&records);
    assert_eq!(
        argvs.len(),
        1,
        "changing `c_std` must invalidate the compile fingerprint, or the \
         stale object silently survives\n{log}"
    );
    assert!(
        argvs[0].contains(&"-std=gnu99".to_string()),
        "the GNU dialect must be requested as such: {:?}\n{log}",
        argvs[0]
    );
    assert_eq!(
        run_built_exe(&app, "cstdapp").out(),
        "199901 gnu",
        "gnu99 is C99 *without* __STRICT_ANSI__ -- that difference is the \
         whole reason the GNU forms are spellable"
    );

    // ---- Dropping the field again is also a change, and must also rebuild.
    fs::write(app.join("Harbour.toml"), manifest("")).unwrap();
    let log = build(&records);
    let argvs = compiles_of_main(&records);
    assert_eq!(
        argvs.len(),
        1,
        "removing `c_std` must invalidate the fingerprint too\n{log}"
    );
    assert!(
        !argvs[0].iter().any(|a| a.starts_with("-std=")),
        "with no `c_std` and no C++ in the graph, Harbour must not invent a \
         standard: {:?}\n{log}",
        argvs[0]
    );
}

/// Assembly in the same target as C must not be given `-std=`.
///
/// zstd, libuv and every crypto library lay their sources out this way: one
/// target, `.c` and `.S` side by side. `-std=` describes a C dialect; the
/// `.S` files go through the same driver but the flag is meaningless there,
/// and `GccToolchain::compile_command` skips it for `Language::Asm`. This is
/// the end-to-end check of that -- the unit test pins the argv, this pins
/// that a mixed target still builds and runs.
#[cfg(all(not(windows), not(target_env = "msvc")))]
#[test]
fn test_c_std_is_not_applied_to_assembly_in_a_mixed_target() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let (shim, records) = install_cc_recorder(tmp.path());

    let app = tmp.path().join("mixed");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(
        app.join("src/main.c"),
        "#include <stdio.h>\n\
         long asm_answer(void);\n\
         int main(void) { printf(\"%ld\\n\", asm_answer()); return 0; }\n",
    )
    .unwrap();

    // A leaf function returning 41 + 1, in the host's assembly dialect.
    let asm = if cfg!(target_arch = "aarch64") {
        ".text\n\
         .globl _asm_answer\n\
         .globl asm_answer\n\
         _asm_answer:\n\
         asm_answer:\n\
         \tmov x0, #42\n\
         \tret\n"
    } else {
        ".text\n\
         .globl _asm_answer\n\
         .globl asm_answer\n\
         _asm_answer:\n\
         asm_answer:\n\
         \tmovq $42, %rax\n\
         \tret\n"
    };
    fs::write(app.join("src/answer.S"), asm).unwrap();

    fs::write(
        app.join("Harbour.toml"),
        "[package]\n\
         name = \"mixed\"\n\
         version = \"0.1.0\"\n\
         \n\
         [targets.mixed]\n\
         kind = \"exe\"\n\
         sources = [\"src/main.c\", \"src/answer.S\"]\n\
         c_std = \"gnu11\"\n",
    )
    .unwrap();

    let log = harbour_run_env(&home, &app, &["build"], &[("CC", shim.to_str().unwrap())]).success();

    let asm_path = app
        .join("src/answer.S")
        .canonicalize()
        .unwrap()
        .display()
        .to_string();
    let main_path = app
        .join("src/main.c")
        .canonicalize()
        .unwrap()
        .display()
        .to_string();
    let argvs = recorded_argvs(&records);

    let asm_argv = argvs
        .iter()
        .find(|a| a.contains(&asm_path))
        .unwrap_or_else(|| panic!("no recorded compile of {asm_path}\n{log}"));
    assert!(
        !asm_argv.iter().any(|a| a.starts_with("-std=")),
        "assembly must not be handed a C standard: {asm_argv:?}"
    );

    let c_argv = argvs
        .iter()
        .find(|a| a.contains(&main_path))
        .unwrap_or_else(|| panic!("no recorded compile of {main_path}\n{log}"));
    assert!(
        c_argv.contains(&"-std=gnu11".to_string()),
        "the C source in the same target still gets it: {c_argv:?}"
    );

    assert_eq!(
        run_built_exe(&app, "mixed").out(),
        "42",
        "and the mixed target has to link and run"
    );
}

/// On MSVC, `c_std` uses the `/std:` switch `cl` has -- and says so when it
/// hasn't got one.
///
/// `cl` has `/std:c11` and `/std:c17` and nothing for C89, C99 or C23. The
/// unit tests in `msvc.rs` pin the argv from any host; this one only runs
/// where `cl` actually does, because the thing worth proving is that `cl`
/// *accepts* the switch and that a standard it cannot express does not turn
/// into a silently ignored setting. Two wrong MSVC expectations have been
/// committed in this repo from reasoning by analogy with gcc, so this is
/// deliberately verified on the `windows-latest` job rather than asserted.
#[cfg(target_env = "msvc")]
#[test]
fn test_c_std_on_msvc_uses_the_switch_cl_has_and_warns_about_the_rest() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let app = tmp.path().join("msvccstd");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(
        app.join("src/main.c"),
        "#include <stdio.h>\n\
         int main(void) { printf(\"ok\\n\"); return 0; }\n",
    )
    .unwrap();

    let manifest = |c_std: &str| {
        format!(
            "[package]\n\
             name = \"msvccstd\"\n\
             version = \"0.1.0\"\n\
             \n\
             [targets.msvccstd]\n\
             kind = \"exe\"\n\
             sources = [\"src/main.c\"]\n\
             c_std = \"{c_std}\"\n"
        )
    };

    // A standard `cl` has: the switch must be on the command line, `cl`
    // must accept it, and the program must run.
    fs::write(app.join("Harbour.toml"), manifest("11")).unwrap();
    let log = harbour_run(&home, &app, &["build"]).success();
    let cc_db = fs::read_to_string(app.join(".harbour/compile_commands.json")).unwrap();
    assert!(
        cc_db.contains("/std:c11"),
        "`c_std = \"11\"` must become `/std:c11` on MSVC:\n{cc_db}\n{log}"
    );
    assert_eq!(run_built_exe(&app, "msvccstd").out(), "ok");

    // A standard `cl` has not: no flag, no GCC spelling smuggled in, and a
    // warning that names the gap.
    fs::remove_dir_all(app.join(".harbour")).unwrap();
    fs::write(app.join("Harbour.toml"), manifest("99")).unwrap();
    let log = harbour_run(&home, &app, &["build"]).success();
    let cc_db = fs::read_to_string(app.join(".harbour/compile_commands.json")).unwrap();
    assert!(
        !cc_db.contains("/std:c") && !cc_db.contains("-std="),
        "`cl` has no C99 switch, so none may be emitted -- and certainly \
         not the gcc spelling:\n{cc_db}"
    );
    let narration = log.combined();
    assert!(
        narration.contains("c_std") && narration.contains("c99"),
        "a standard that cannot be honoured must be reported, not dropped \
         in silence\n{log}"
    );
    assert_eq!(run_built_exe(&app, "msvccstd").out(), "ok");
}

// Named profiles (issue #106)
//
// `[profile.NAME]` parsed for any name and only `debug`/`release` could ever
// be selected, because there was no `--profile` flag. The assertions here are
// on the **real argv the compiler was handed** and on where the artifact
// landed, because a named profile whose settings silently did not apply is
// an ordinary build that reports `Finished asan`.
/// A consumer with a named profile inheriting from `release`, plus a
/// path dependency, so "whose profile wins" is observable.
fn write_profile_project(root: &std::path::Path) {
    let dep = root.join("profdep");
    fs::create_dir_all(dep.join("include")).unwrap();
    fs::create_dir_all(dep.join("src")).unwrap();
    fs::write(
        dep.join("Harbour.toml"),
        r#"[package]
name = "profdep"
version = "0.1.0"

# Ignored: profiles come from the package being built, never from a
# dependency. A define here reaching the compile line is the defect.
[profile.debug]
cflags = ["-DDEP_PROFILE_LEAKED=1"]

[profile.zzdepprof]
inherits = "release"
cflags = ["-DDEP_NAMED_PROFILE_LEAKED=1"]

[targets.profdep]
kind = "staticlib"
sources = ["src/lib.c"]

[targets.profdep.surface.compile.public]
include_dirs = ["include"]
"#,
    )
    .unwrap();
    fs::write(
        dep.join("include/profdep.h"),
        "#ifndef PROFDEP_H\n#define PROFDEP_H\nint profdep_value(void);\n#endif\n",
    )
    .unwrap();
    fs::write(
        dep.join("src/lib.c"),
        "#include \"profdep.h\"\nint profdep_value(void) { return 7; }\n",
    )
    .unwrap();

    let app = root.join("profapp");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(
        app.join("Harbour.toml"),
        r#"[package]
name = "profapp"
version = "0.1.0"

[dependencies]
profdep = { path = "../profdep" }

[profile.release]
cflags = ["-DFROM_RELEASE=1"]

[profile.asan]
inherits = "release"
opt_level = "1"
cflags = ["-DFROM_ASAN=1"]

[targets.profapp]
kind = "exe"
sources = ["src/main.c"]
"#,
    )
    .unwrap();
    fs::write(
        app.join("src/main.c"),
        r#"#include <stdio.h>
#include "profdep.h"

int main(void) {
#ifdef DEP_PROFILE_LEAKED
    printf("dep-profile-leaked\n");
    return 0;
#endif
#ifdef DEP_NAMED_PROFILE_LEAKED
    printf("dep-named-profile-leaked\n");
    return 0;
#endif
    printf("%d\n", profdep_value());
    return 0;
}
"#,
    )
    .unwrap();
}

/// `--profile asan` selects the named profile: its own keys, its
/// `inherits` ancestor's keys, and its own output directory.
#[cfg(not(windows))]
#[test]
fn test_named_profile_reaches_the_compiler_and_its_own_output_directory() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    write_profile_project(tmp.path());
    let app = tmp.path().join("profapp");

    let asan_root = tmp.path().join("asan-rec");
    fs::create_dir_all(&asan_root).unwrap();
    let (shim, records) = install_cc_recorder(&asan_root);
    harbour_run_env(
        &home,
        &app,
        &["build", "--profile", "asan"],
        &[("CC", shim.to_str().unwrap())],
    )
    .success();

    // The artifact lands under the profile's own name, so two profiles never
    // share a fingerprint cache.
    assert!(
        built_exe_path_in(&app, "asan", "profapp").exists(),
        "`--profile asan` must build into the `asan` output directory.\n{:#?}",
        snapshot_tree(&target_dir(&app))
            .into_keys()
            .collect::<Vec<_>>()
    );

    let argv: Vec<String> = recorded_argvs(&records).into_iter().flatten().collect();
    assert!(!argv.is_empty(), "the shim recorded nothing");
    assert!(
        argv.iter().any(|a| a == "-DFROM_ASAN=1"),
        "the named profile's own `cflags` must reach the compiler.\nargv:\n{argv:#?}"
    );
    assert!(
        argv.iter().any(|a| a == "-DFROM_RELEASE=1"),
        "`inherits = \"release\"` must bring `[profile.release]`'s `cflags` \
         with it.\nargv:\n{argv:#?}"
    );
    assert!(
        argv.iter().any(|a| a == "-O1"),
        "the named profile's own `opt_level` must win over its ancestor's \
         `3`.\nargv:\n{argv:#?}"
    );
    assert!(
        !argv.iter().any(|a| a == "-O3"),
        "`opt_level = \"1\"` must replace the inherited `3`, not sit beside \
         it.\nargv:\n{argv:#?}"
    );

    // Whose profile wins: the root's. A dependency's `[profile.*]` -- named
    // or not -- must not reach any compile line, and the binary says so.
    assert!(
        !argv
            .iter()
            .any(|a| a.contains("DEP_PROFILE_LEAKED") || a.contains("DEP_NAMED_PROFILE_LEAKED")),
        "a dependency's profile must not reach the build.\nargv:\n{argv:#?}"
    );
    let out = Command::new(built_exe_path_in(&app, "asan", "profapp"))
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "7");

    // `--release` is a different profile in a different directory, and the
    // named profile's own flags must not follow it there.
    let rel_root = tmp.path().join("rel-rec");
    fs::create_dir_all(&rel_root).unwrap();
    let (shim, records) = install_cc_recorder(&rel_root);
    harbour_run_env(
        &home,
        &app,
        &["build", "--release"],
        &[("CC", shim.to_str().unwrap())],
    )
    .success();
    assert!(built_exe_path_in(&app, "release", "profapp").exists());
    let argv: Vec<String> = recorded_argvs(&records).into_iter().flatten().collect();
    assert!(
        argv.iter().any(|a| a == "-DFROM_RELEASE=1") && argv.iter().any(|a| a == "-O3"),
        "argv:\n{argv:#?}"
    );
    assert!(
        !argv.iter().any(|a| a == "-DFROM_ASAN=1"),
        "`[profile.asan]`'s own keys must not apply to `release`.\nargv:\n{argv:#?}"
    );
}

/// `--release` and `--profile` are the same setting, so asking for both is
/// refused rather than silently resolved by precedence.
#[test]
fn test_release_and_profile_cannot_both_be_given() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    write_profile_project(tmp.path());
    let app = tmp.path().join("profapp");

    let run = harbour_run(&home, &app, &["build", "--release", "--profile", "asan"]);
    assert!(
        !run.status.success(),
        "`--release` is `--profile release`; both at once must be an error\n{run}"
    );
    // Named, so this cannot pass merely because `--profile` is unrecognised.
    let message = run.combined();
    assert!(
        message.contains("cannot be used with") && message.contains("--profile"),
        "the error must be the conflict, not an unknown flag\n{run}"
    );

    // ... and each on its own is accepted.
    harbour_run(&home, &app, &["build", "--profile", "asan"]).success();
    harbour_run(&home, &app, &["build", "--release"]).success();
}

/// A profile name nothing declares is an error listing what exists, not a
/// silent debug build under a directory named after the typo.
#[test]
fn test_an_unknown_profile_name_is_an_error_listing_what_exists() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    write_profile_project(tmp.path());
    let app = tmp.path().join("profapp");

    let run = harbour_run(&home, &app, &["build", "--profile", "asam"]);
    assert!(
        !run.status.success(),
        "an undeclared profile must fail rather than build something\n{run}"
    );
    let message = run.combined();
    assert!(
        message.contains("no profile named `asam`"),
        "the diagnostic must name what was asked for\n{message}"
    );
    assert!(
        message.contains("`asan`") && message.contains("`release`"),
        "and list the profiles that exist\n{message}"
    );
    assert!(
        !built_exe_path_in(&app, "asam", "profapp").exists(),
        "nothing must have been built under the typo's name"
    );
}

/// A named profile must declare `inherits`, and the build says so rather
/// than guessing a base.
#[test]
fn test_a_named_profile_without_inherits_fails_the_build() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let app = tmp.path().join("noinherit");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(app.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();
    fs::write(
        app.join("Harbour.toml"),
        r#"[package]
name = "noinherit"
version = "0.1.0"

[profile.asan]
opt_level = "1"
sanitizers = ["address"]

[targets.noinherit]
kind = "exe"
sources = ["src/main.c"]
"#,
    )
    .unwrap();

    let run = harbour_run(&home, &app, &["build"]);
    assert!(!run.status.success(), "{run}");
    assert!(
        run.combined().contains("must set `inherits`"),
        "the diagnostic must say what is missing\n{run}"
    );
}
