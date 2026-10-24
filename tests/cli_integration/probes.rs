//! Native configure-style probes: answers, caches, generated headers, MSVC probes.
#![allow(unused_imports)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::prelude::*;
use predicates::prelude::*;
use tempfile::TempDir;

use harbour::test_support::fixtures::local_registry;

use crate::common::*;

/// The manifest source for the probe fixture used by the tests below.
///
/// The header list is chosen so the answers are *known independently*: one
/// header exists on every hosted platform, and one exists nowhere. A probe
/// subsystem whose fixture answers are all `yes` would be indistinguishable
/// from a constant, which is why the negative case is in the fixture rather
/// than in a separate test.
#[cfg(not(windows))]
const PROBE_FIXTURE_MANIFEST: &str = r#"[package]
name = "probed"
version = "0.1.0"

[targets.probed]
kind = "exe"
sources = ["src/main.c"]

[targets.probed.probes]
check_headers = ["stdio.h", "definitely/not/a/real/header.h"]
check_sizeof = ["int", "long", "short", "size_t", "void *"]
"#;

/// A program that is *only* compilable if every probed size agrees with what
/// the compiler itself says, and which prints what the preprocessor saw.
///
/// The compile-time assertions are the point. "Harbour built something and
/// exited 0" has repeatedly meant "wrong output, exit 0" in this repo, so the
/// fixture is written so that a wrong probe answer is a **build failure**
/// rather than a wrong number on stdout.
#[cfg(not(windows))]
const PROBE_FIXTURE_SOURCE: &str = r#"#include <stdio.h>
#include <stddef.h>
#define SAME(a, b) { char c[((a) == (b)) ? 1 : -1]; (void) c; }
int main(void) {
    SAME(sizeof(int), SIZEOF_INT)
    SAME(sizeof(long), SIZEOF_LONG)
    SAME(sizeof(short), SIZEOF_SHORT)
    SAME(sizeof(size_t), SIZEOF_SIZE_T)
    SAME(sizeof(void *), SIZEOF_VOID_P)
#ifdef HAVE_STDIO_H
    printf("stdio=yes\n");
#else
    printf("stdio=no\n");
#endif
#ifdef HAVE_DEFINITELY_NOT_A_REAL_HEADER_H
    printf("bogus=yes\n");
#else
    printf("bogus=no\n");
#endif
    printf("long=%d\n", SIZEOF_LONG);
    return 0;
}
"#;

#[cfg(not(windows))]
fn write_probe_fixture(dir: &std::path::Path) {
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(dir.join("Harbour.toml"), PROBE_FIXTURE_MANIFEST).unwrap();
    fs::write(dir.join("src/main.c"), PROBE_FIXTURE_SOURCE).unwrap();
}

/// Every recorded argv containing a probe snippet.
#[cfg(not(windows))]
fn probe_compile_count(records: &std::path::Path) -> usize {
    recorded_argvs(records)
        .iter()
        .filter(|a| a.iter().any(|x| x.contains("probe.c")))
        .count()
}

#[cfg(not(windows))]
fn clear_records(records: &std::path::Path) {
    for entry in fs::read_dir(records).unwrap() {
        fs::remove_file(entry.unwrap().path()).unwrap();
    }
}

/// The probe defines on the recorded compile of the package's own source, in
/// the order the compiler received them.
#[cfg(not(windows))]
fn recorded_probe_defines(records: &std::path::Path) -> Vec<String> {
    let argvs = recorded_argvs(records);
    let real = argvs
        .iter()
        .find(|a| a.iter().any(|x| x.ends_with("main.c")))
        .unwrap_or_else(|| panic!("no compile of main.c was recorded; argvs: {argvs:#?}"));
    real.iter()
        .filter(|a| a.starts_with("-DHAVE_") || a.starts_with("-DSIZEOF_"))
        .cloned()
        .collect()
}

/// Probe answers must reach the *real* compile command, not just a data
/// structure.
///
/// This is the test that distinguishes a working probe subsystem from the
/// failure mode this repo keeps producing: nine-plus features have existed as
/// well-typed, fully-parsed code that never reached the compiler, and the
/// crate's root `pub mod`s suppress `dead_code` so nothing warns. The witness
/// here is the argv the compiler was actually handed, captured by a recording
/// `CC` shim, plus the behaviour of the produced binary.
#[test]
#[cfg(not(windows))]
fn probe_answers_reach_the_real_compile_command() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let (shim, records) = install_cc_recorder(tmp.path());
    let app = tmp.path().join("probed");
    write_probe_fixture(&app);

    harbour_run_env(&home, &app, &["build"], &[("CC", shim.to_str().unwrap())]).success();

    let probe_defines = recorded_probe_defines(&records);

    assert!(
        probe_defines.contains(&"-DHAVE_STDIO_H=1".to_string()),
        "a header that exists must reach the compiler as a define. \
         probe defines on the real compile line: {probe_defines:?}"
    );

    // The inverse, and the more important half: a false answer must emit
    // *nothing*. `-DHAVE_X=0` would satisfy the `#ifdef HAVE_X` that every
    // real config header is tested with, inverting the answer.
    assert!(
        !probe_defines
            .iter()
            .any(|a| a.contains("DEFINITELY_NOT_A_REAL_HEADER")),
        "a header that does not exist must emit no define at all, not \
         `=0`: {probe_defines:?}"
    );

    // Order is declaration order, not sorted. `ac859c1` removed a sort of
    // the flag list because C flags are last-wins; re-introducing one here
    // for probe defines would be the same defect in a new place.
    let names: Vec<String> = probe_defines
        .iter()
        .map(|d| {
            d.trim_start_matches("-D")
                .split('=')
                .next()
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(
        names,
        vec![
            "HAVE_STDIO_H",
            "SIZEOF_INT",
            "SIZEOF_LONG",
            "SIZEOF_SHORT",
            "SIZEOF_SIZE_T",
            "SIZEOF_VOID_P",
        ],
        "probe defines must reach the compiler in declaration order -- \
         `check_headers` before `check_sizeof`, each in list order"
    );

    // The binary is the independent witness: it reports what the
    // preprocessor saw, so the claim does not rest on the argv capture.
    let out = run_built_exe(&app, "probed");
    assert!(
        out.out().contains("stdio=yes"),
        "the built program must see HAVE_STDIO_H: {}",
        out.out()
    );
    assert!(
        out.out().contains("bogus=no"),
        "the built program must not see a define for a header that does not \
         exist: {}",
        out.out()
    );
}

/// A `sizeof` probe must produce the size the compiler itself reports, for
/// the target being built for, without running anything.
///
/// The fixture's `SAME(...)` macros are compile-time assertions, so a wrong
/// answer fails the build. That is deliberate: the alternative -- printing
/// the number and comparing strings -- cannot distinguish "the probe is
/// wrong" from "the program printed something else".
#[test]
#[cfg(not(windows))]
fn sizeof_probes_agree_with_the_compiler_without_running_a_program() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let app = tmp.path().join("probed");
    write_probe_fixture(&app);

    // If any probed size disagreed with `sizeof` as the compiler computes
    // it, this build would fail on a negative array bound.
    build_ok(&home, &app);

    let out = run_built_exe(&app, "probed");
    let long_size: usize = out
        .out()
        .lines()
        .find_map(|l| l.strip_prefix("long="))
        .expect("the fixture prints the probed sizeof(long)")
        .parse()
        .expect("a number");
    // Compared against Rust's own view of C `long` rather than a literal
    // `8`, so this stays correct on a 32-bit target -- where the answer must
    // be 4, and where the vendored-config placeholder this replaces would
    // have asserted 8 because it was keyed on the OS alone.
    assert_eq!(
        long_size,
        std::mem::size_of::<std::os::raw::c_long>(),
        "the probed sizeof(long) must match what this target's C `long` \
         actually is"
    );
}

/// Probe answers must be cached against the toolchain, and the cache must be
/// discarded wholesale when the toolchain changes.
///
/// A stale probe answer is worse than a stale object file: it is a *wrong
/// `#define`*, so the package compiles as though it were running on a
/// different machine. This repo has already shipped one cache-keying defect
/// (`e860627`), and the probe cache reuses `ToolchainFingerprint::hash()`
/// precisely so that there is only one definition of "the toolchain
/// changed".
#[test]
#[cfg(not(windows))]
fn the_probe_cache_survives_a_rebuild_and_dies_with_the_toolchain() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let app = tmp.path().join("probed");
    write_probe_fixture(&app);

    // Two shims that behave identically but live at different paths, so the
    // only thing that changes between builds is the toolchain identity.
    let (shim_a, records) = install_cc_recorder(tmp.path());
    let shim_b = tmp.path().join("cc-recorder-b");
    fs::copy(&shim_a, &shim_b).unwrap();
    let mut perms = fs::metadata(&shim_b).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    fs::set_permissions(&shim_b, perms).unwrap();

    harbour_run_env(&home, &app, &["build"], &[("CC", shim_a.to_str().unwrap())]).success();
    let cold = probe_compile_count(&records);
    assert!(
        cold > 0,
        "a cold build must actually run probe compiles; recorded none"
    );

    clear_records(&records);
    harbour_run_env(&home, &app, &["build"], &[("CC", shim_a.to_str().unwrap())]).success();
    assert_eq!(
        probe_compile_count(&records),
        0,
        "a warm rebuild with an unchanged toolchain must answer every probe \
         from the cache and spawn no compiler at all"
    );

    clear_records(&records);
    harbour_run_env(&home, &app, &["build"], &[("CC", shim_b.to_str().unwrap())]).success();
    assert_eq!(
        probe_compile_count(&records),
        cold,
        "changing the compiler must discard the probe cache wholesale and \
         re-measure every probe -- a probe answer that survives a toolchain \
         change is a wrong `#define`, not a stale object"
    );
}

/// A probe answer that changes must recompile the objects that depend on it.
///
/// The chain under test is long and every link has been a bug in some build
/// system: the compile surface changes, so the probe cache is discarded, so
/// the probe is re-measured, so the answer flips, so the define set changes,
/// so the compile fingerprint changes, so the object is rebuilt, so the
/// program behaves differently. Asserting on the program's *behaviour* is
/// what makes this a real test rather than a check that a hash changed.
#[test]
#[cfg(not(windows))]
fn a_changed_probe_answer_rebuilds_and_changes_what_the_program_does() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let app = tmp.path().join("probed");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::create_dir_all(app.join("vendored/made/up")).unwrap();

    let manifest = |extra_include: &str| {
        format!(
            "[package]\n\
             name = \"probed\"\n\
             version = \"0.1.0\"\n\
             \n\
             [targets.probed]\n\
             kind = \"exe\"\n\
             sources = [\"src/main.c\"]\n\
             {extra_include}\n\
             [targets.probed.probes]\n\
             check_headers = [\"made/up/header.h\"]\n"
        )
    };

    fs::write(
        app.join("src/main.c"),
        "#include <stdio.h>\n\
         int main(void) {\n\
         #ifdef HAVE_MADE_UP_HEADER_H\n\
         \x20   printf(\"found\\n\");\n\
         #else\n\
         \x20   printf(\"missing\\n\");\n\
         #endif\n\
         \x20   return 0;\n\
         }\n",
    )
    .unwrap();
    fs::write(
        app.join("vendored/made/up/header.h"),
        "/* a header that exists only when the include path reaches it */\n",
    )
    .unwrap();

    // Build 1: the header is on disk but not on the include path, so the
    // probe must answer no.
    fs::write(app.join("Harbour.toml"), manifest("")).unwrap();
    build_ok(&home, &app);
    assert_eq!(
        run_built_exe(&app, "probed").out(),
        "missing",
        "a header outside the include path must probe as absent -- if this \
         says `found`, the probe is reading the source tree rather than \
         asking the compiler"
    );

    // Build 2: the same header, now reachable. Nothing about the probe
    // declaration changed; only the surface it is asked against.
    fs::write(
        app.join("Harbour.toml"),
        manifest("\n[targets.probed.private]\ninclude_dirs = [\"vendored\"]\n"),
    )
    .unwrap();
    build_ok(&home, &app);
    assert_eq!(
        run_built_exe(&app, "probed").out(),
        "found",
        "the include path changed, so the probe must be re-measured, the \
         define must change, and the object must be recompiled. `missing` \
         here means a stale probe answer survived into a rebuilt binary"
    );
}

/// The same manifest and toolchain must produce the same probe defines, in
/// the same order, every time.
///
/// Not a hypothetical concern: a measured 18 distinct link orders across 40
/// clean runs of one manifest was fixed days ago, caused by `HashMap`
/// iteration reaching build output. Probe answers reach build output as
/// defines, so this is the same exposure in a new subsystem.
#[test]
#[cfg(not(windows))]
fn probe_defines_are_byte_identical_across_clean_builds() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let (shim, records) = install_cc_recorder(tmp.path());
    let app = tmp.path().join("probed");
    write_probe_fixture(&app);

    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..6 {
        fs::remove_dir_all(app.join(".harbour")).ok();
        clear_records(&records);
        harbour_run_env(&home, &app, &["build"], &[("CC", shim.to_str().unwrap())]).success();
        seen.insert(recorded_probe_defines(&records).join(" "));
    }
    assert_eq!(
        seen.len(),
        1,
        "6 clean builds of one manifest produced {} different probe define \
         lists:\n{:#?}",
        seen.len(),
        seen
    );
}

/// `harbour flags` must list the probe defines the build actually uses, in
/// the same order, and must attribute them to a *probe* rather than to a
/// manifest table nobody wrote.
///
/// This command is documented as authoritative about what the compiler
/// receives, and §2.4 of the 2026-09-07 audit is four separate instances of
/// it not being -- each one a second implementation of "what flags does this
/// file get" that had drifted from the first. A probe define reaching the
/// compiler but not this listing would be the fifth, and it was: the first
/// version of the probe subsystem measured probes inside `BuildPlan`, so
/// `harbour flags` printed three profile flags and nothing else.
#[test]
#[cfg(not(windows))]
fn harbour_flags_lists_the_probe_defines_the_build_uses() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let (shim, records) = install_cc_recorder(tmp.path());
    let app = tmp.path().join("probed");
    write_probe_fixture(&app);

    harbour_run_env(&home, &app, &["build"], &[("CC", shim.to_str().unwrap())]).success();
    let from_compiler = recorded_probe_defines(&records);
    assert!(
        !from_compiler.is_empty(),
        "the compiler must have received probe defines for this test to mean \
         anything"
    );

    let reported = harbour_run(&home, &app, &["flags", "probed", "--compile"]).success();
    let from_flags: Vec<String> = reported_flags(reported.out())
        .into_iter()
        .filter(|f| f.starts_with("-DHAVE_") || f.starts_with("-DSIZEOF_"))
        .collect();

    assert_eq!(
        from_flags, from_compiler,
        "`harbour flags` must print exactly the probe defines the compiler \
         received, in the same order.\nflags said: {from_flags:?}\ncc got:    \
         {from_compiler:?}"
    );

    // And the attribution must say these were measured, not declared. A
    // reader who goes looking for `-DSIZEOF_LONG=8` in a `surface` table
    // will not find it, and needs to know that changing the toolchain can
    // change the value.
    assert!(
        reported.out().contains("(probe)"),
        "a probe-derived define must be attributed to `probe`, not to a \
         manifest table that does not contain it:\n{}",
        reported.out()
    );
}

/// Probe answers are private to the target that declared them, and do **not**
/// reach a dependent. This pins the behaviour Harbour actually has.
///
/// A characterization test, not an aspiration. `visibility = "public"` was
/// implemented in the first draft of this subsystem: it parsed, it was
/// branched on in `BuildPlan`, and its defines were folded into the
/// `AbiSurfaceKey` so a consumer would relink. It did not work. A dependent's
/// compile surface is folded from each dependency's *declared*
/// `surface.compile.public` (`surface_resolver.rs`, the `dep_resolved
/// .compile_public` arm), and a probe answer exists in no manifest -- so the
/// consumer here failed to compile on an undefined `SIZEOF_LONG` while the
/// field looked, from the library's side, like it worked.
///
/// That is the 2026-09-07 audit's §2.7 shape exactly, so the field was
/// removed rather than shipped. This test exists so that the day someone
/// implements propagation, it fails and says so, instead of the manifest
/// reference quietly becoming wrong.
#[test]
#[cfg(not(windows))]
fn probe_answers_do_not_reach_a_dependent() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let lib = tmp.path().join("problib");
    fs::create_dir_all(lib.join("src")).unwrap();
    fs::create_dir_all(lib.join("include")).unwrap();
    fs::write(
        lib.join("Harbour.toml"),
        "[package]\n\
         name = \"problib\"\n\
         version = \"0.1.0\"\n\
         \n\
         [targets.problib]\n\
         kind = \"staticlib\"\n\
         sources = [\"src/lib.c\"]\n\
         public_headers = [\"include/problib.h\"]\n\
         \n\
         [targets.problib.public]\n\
         include_dirs = [\"include\"]\n\
         \n\
         [targets.problib.probes]\n\
         check_headers = [\"stdio.h\"]\n\
         check_sizeof = [\"long\"]\n",
    )
    .unwrap();
    fs::write(
        lib.join("include/problib.h"),
        "int problib_size_of_long(void);\n",
    )
    .unwrap();
    // The library itself does see its own answers -- that is the half that
    // works, and this file would not compile without it.
    fs::write(
        lib.join("src/lib.c"),
        "#ifndef HAVE_STDIO_H\n\
         #error \"the declaring target must see its own probe answers\"\n\
         #endif\n\
         int problib_size_of_long(void) { return SIZEOF_LONG; }\n",
    )
    .unwrap();

    let app = tmp.path().join("probapp");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(
        app.join("Harbour.toml"),
        "[package]\n\
         name = \"probapp\"\n\
         version = \"0.1.0\"\n\
         \n\
         [dependencies]\n\
         problib = { path = \"../problib\" }\n\
         \n\
         [targets.probapp]\n\
         kind = \"exe\"\n\
         sources = [\"src/main.c\"]\n\
         \n\
         [targets.probapp.deps]\n\
         problib = \"problib\"\n",
    )
    .unwrap();
    fs::write(
        app.join("src/main.c"),
        "#include <stdio.h>\n\
         #include \"problib.h\"\n\
         int main(void) {\n\
         #ifdef HAVE_STDIO_H\n\
         \x20   printf(\"consumer_sees_probe=1\\n\");\n\
         #else\n\
         \x20   printf(\"consumer_sees_probe=0\\n\");\n\
         #endif\n\
         \x20   printf(\"lib_long=%d\\n\", problib_size_of_long());\n\
         \x20   return 0;\n\
         }\n",
    )
    .unwrap();

    build_ok(&home, &app);
    let out = run_built_exe(&app, "probapp");

    assert!(
        out.out().contains("consumer_sees_probe=0"),
        "probe answers are private to the declaring target. If the consumer \
         now sees them, propagation has been implemented -- update \
         MANIFEST.md's probe section and this test together:\n{}",
        out.out()
    );
    // The library's own answer is real and travels in the archive, which is
    // what makes "private" the right word rather than "broken".
    let long_size: usize = out
        .out()
        .lines()
        .find_map(|l| l.strip_prefix("lib_long="))
        .expect("the library reports its probed sizeof(long)")
        .parse()
        .expect("a number");
    assert_eq!(
        long_size,
        std::mem::size_of::<std::os::raw::c_long>(),
        "the library was compiled with its own probe answer, and the value \
         must be the real one:\n{}",
        out.out()
    );
}

/// Probes are answered in the dialect the package is compiled in.
///
/// `answer_for_target` builds its `CompileInput` from the same
/// `Toolchain::compile_command` the real build uses, so when `c_std` grew a
/// field there, that site had to say something. `c_std: None` would have
/// compiled, and would have meant probes measured in the compiler's default
/// dialect while the package is compiled in its own — a `config.h`
/// describing a translation unit the package is not going to have.
///
/// That is not hypothetical. Measured, on two libcs:
///
/// * glibc (gcc 13): `sizeof(u_int)` answers under the default dialect and
///   under `-std=gnu99`, and the type **does not exist** under `-std=c99`
///   or `-std=c11`. `__STRICT_ANSI__` stops `features.h` defining
///   `_DEFAULT_SOURCE`, and `__USE_MISC` goes with it. `gnu99` versus `c99`
///   — the exact pair — is a different set of declarations.
/// * Apple clang: BSD types survive `-std=c99` there, but `max_align_t` is
///   visible only from `-std=c11` up, so the dialect still decides the
///   answer, by a different mechanism.
///
/// This test uses a header of its own rather than either of those, so it
/// asserts the wiring on every toolchain including MSVC, and so it cannot
/// start failing because a libc reorganised its feature macros. `89` and
/// `11` are the two values every backend can express: `cl` has `/std:c11`
/// and defines `__STDC_VERSION__` under it, and emits nothing for `89`.
///
/// The second half is the cache: both builds run in the same tree with no
/// `.harbour` removed in between, so an answer that survived the `c_std`
/// change would be a stale cache hit. `surface_key` covers the dialect for
/// exactly this reason.
#[test]
fn test_probes_are_measured_in_the_packages_own_c_dialect() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let app = tmp.path().join("probedialect");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::create_dir_all(app.join("include")).unwrap();
    fs::write(app.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();

    // A header that exists on disk but only *compiles* from C99 onward.
    // The probe question "does `#include <needs_c99.h>` compile" therefore
    // has a different answer per dialect, on every toolchain, without
    // depending on any libc's feature macros.
    fs::write(
        app.join("include/needs_c99.h"),
        "#if !defined(__STDC_VERSION__) || __STDC_VERSION__ < 199901L\n\
         #error \"this header requires C99\"\n\
         #endif\n\
         int needs_c99(void);\n",
    )
    .unwrap();

    let manifest = |c_std: &str| {
        format!(
            "[package]\n\
             name = \"probedialect\"\n\
             version = \"0.1.0\"\n\
             \n\
             [targets.probedialect]\n\
             kind = \"exe\"\n\
             sources = [\"src/main.c\"]\n\
             c_std = \"{c_std}\"\n\
             \n\
             [targets.probedialect.private]\n\
             include_dirs = [\"include\"]\n\
             \n\
             [targets.probedialect.probes]\n\
             check_headers = [\"needs_c99.h\"]\n"
        )
    };

    let define_present = |app: &std::path::Path| -> bool {
        fs::read_to_string(app.join(".harbour/compile_commands.json"))
            .unwrap()
            .contains("HAVE_NEEDS_C99_H")
    };

    // C89: the probe must be compiled as C89 and answer "no", so no define.
    fs::write(app.join("Harbour.toml"), manifest("89")).unwrap();
    let run = harbour_run(&home, &app, &["build"]).success();
    assert!(
        !define_present(&app),
        "the probe must be answered under the target's own `c_std = \"89\"`; \
         a `no` here is the header refusing to compile as C89\n{run}"
    );

    // C11, same tree, cache warm from the run above: the answer must be
    // re-measured, not served.
    fs::write(app.join("Harbour.toml"), manifest("11")).unwrap();
    let run = harbour_run(&home, &app, &["build"]).success();
    assert!(
        define_present(&app),
        "under `c_std = \"11\"` the same probe must answer `yes` -- and the \
         previous dialect's answer must not have been served from the probe \
         cache\n{run}"
    );

    // And back again, to pin that the invalidation is not one-directional
    // (a cache keyed on \"anything changed\" would pass the step above).
    fs::write(app.join("Harbour.toml"), manifest("89")).unwrap();
    let run = harbour_run(&home, &app, &["build"]).success();
    assert!(
        !define_present(&app),
        "going back to C89 must re-measure too\n{run}"
    );

    assert_eq!(
        run_built_exe(&app, "probedialect").status.code(),
        Some(0),
        "and the package still builds and runs"
    );
}

/// A `symbol` probe must **link**, not merely compile.
///
/// That is the entire reason the kind exists separately from `header`: a
/// header declaring something the libc does not provide is the classic
/// `configure` trap, and a compile-only check answers `yes` to every one of
/// them. The witness here is a symbol that cannot possibly exist answering
/// `no` while real libc functions answer `yes`, both reported by the built
/// program rather than by a flag list.
#[test]
#[cfg(not(windows))]
fn symbol_probes_link_and_distinguish_real_functions_from_invented_ones() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let app = tmp.path().join("symbols");
    fs::create_dir_all(app.join("src")).unwrap();

    fs::write(
        app.join("Harbour.toml"),
        "[package]\n\
         name = \"symbols\"\n\
         version = \"0.1.0\"\n\
         \n\
         [targets.symbols]\n\
         kind = \"exe\"\n\
         sources = [\"src/main.c\"]\n\
         \n\
         [targets.symbols.probes]\n\
         check_symbols = [\n\
         \x20 \"strerror_r\",\n\
         \x20 \"gettimeofday\",\n\
         \x20 \"definitely_not_a_real_function_anywhere\",\n\
         ]\n",
    )
    .unwrap();
    fs::write(
        app.join("src/main.c"),
        "#include <stdio.h>\n\
         int main(void) {\n\
         #ifdef HAVE_STRERROR_R\n\
         \x20   printf(\"strerror_r=yes\\n\");\n\
         #else\n\
         \x20   printf(\"strerror_r=no\\n\");\n\
         #endif\n\
         #ifdef HAVE_GETTIMEOFDAY\n\
         \x20   printf(\"gettimeofday=yes\\n\");\n\
         #else\n\
         \x20   printf(\"gettimeofday=no\\n\");\n\
         #endif\n\
         #ifdef HAVE_DEFINITELY_NOT_A_REAL_FUNCTION_ANYWHERE\n\
         \x20   printf(\"bogus=yes\\n\");\n\
         #else\n\
         \x20   printf(\"bogus=no\\n\");\n\
         #endif\n\
         \x20   return 0;\n\
         }\n",
    )
    .unwrap();

    build_ok(&home, &app);
    let out = run_built_exe(&app, "symbols");

    assert!(
        out.out().contains("strerror_r=yes"),
        "a real libc function must probe as present:\n{}",
        out.out()
    );
    assert!(
        out.out().contains("gettimeofday=yes"),
        "a real libc function must probe as present:\n{}",
        out.out()
    );
    // The load-bearing half. A probe that answered `yes` to everything --
    // which is what a broken link step, or a compile-only check against a
    // fallback declaration that is never resolved, would produce -- fails
    // here and only here.
    assert!(
        out.out().contains("bogus=no"),
        "a symbol that exists nowhere must probe as absent. `yes` here means \
         the probe is not actually linking, so every `HAVE_<function>` is a \
         lie:\n{}",
        out.out()
    );
}

/// A symbol that is a *macro* rather than a function must still probe as
/// present.
///
/// Not hypothetical: on macOS `htonl` is a macro and `<arpa/inet.h>`
/// declares no function of that name at all, so `&htonl` does not compile.
/// A probe that only took the address answers `no` on every Mac for
/// something the package can call perfectly well. Verified by deleting the
/// `#if defined` branch from `symbol_snippet` and watching this flip.
///
/// Asserted on both platforms rather than guarded by `cfg(target_os)`,
/// because the answer must be `yes` either way -- macro on one, ordinary
/// function on the other, usable on both. A test that only ran where the
/// macro case occurs would pass on Linux for the wrong reason.
#[test]
#[cfg(not(windows))]
fn a_symbol_that_is_a_macro_probes_as_present() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let app = tmp.path().join("macrosym");
    fs::create_dir_all(app.join("src")).unwrap();

    fs::write(
        app.join("Harbour.toml"),
        "[package]\n\
         name = \"macrosym\"\n\
         version = \"0.1.0\"\n\
         \n\
         [targets.macrosym]\n\
         kind = \"exe\"\n\
         sources = [\"src/main.c\"]\n\
         \n\
         [targets.macrosym.probes.named.HAVE_HTONL]\n\
         symbol = \"htonl\"\n\
         prelude = [\"arpa/inet.h\"]\n",
    )
    .unwrap();
    fs::write(
        app.join("src/main.c"),
        "#include <stdio.h>\n\
         #include <arpa/inet.h>\n\
         int main(void) {\n\
         #ifdef HAVE_HTONL\n\
         \x20   printf(\"htonl=yes %u\\n\", (unsigned) htonl(1u));\n\
         #else\n\
         \x20   printf(\"htonl=no\\n\");\n\
         #endif\n\
         \x20   return 0;\n\
         }\n",
    )
    .unwrap();

    build_ok(&home, &app);
    let out = run_built_exe(&app, "macrosym");
    assert!(
        out.out().contains("htonl=yes"),
        "`htonl` is callable on every platform Harbour supports -- as a macro \
         on some and a function on others -- so it must probe as present. \
         `no` means the macro case is unhandled:\n{}",
        out.out()
    );
}

/// `libs` puts the library on the probe's link line, which is what subsumes
/// `AC_CHECK_LIB` rather than needing a separate probe kind.
///
/// The assertion is deliberately weak in one direction and strong in the
/// other. `cbrt` must be found *with* `-lm` on every platform; whether it is
/// also found without depends on the libc (glibc 2.34+ merged libm into
/// libc, and Apple never separated them), so asserting `no` there would be
/// asserting a property of the machine rather than of Harbour.
#[test]
#[cfg(not(windows))]
fn libs_reaches_the_probe_link_line() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let app = tmp.path().join("libsym");
    fs::create_dir_all(app.join("src")).unwrap();

    fs::write(
        app.join("Harbour.toml"),
        "[package]\n\
         name = \"libsym\"\n\
         version = \"0.1.0\"\n\
         \n\
         [targets.libsym]\n\
         kind = \"exe\"\n\
         sources = [\"src/main.c\"]\n\
         \n\
         [targets.libsym.public]\n\
         system_libs = [\"m\"]\n\
         \n\
         [targets.libsym.probes.named.HAVE_CBRT_IN_LIBM]\n\
         symbol = \"cbrt\"\n\
         prelude = [\"math.h\"]\n\
         libs = [\"m\"]\n\
         \n\
         [targets.libsym.probes.named.HAVE_NOTHING_IN_LIBM]\n\
         symbol = \"definitely_not_in_libm_either\"\n\
         libs = [\"m\"]\n",
    )
    .unwrap();
    fs::write(
        app.join("src/main.c"),
        "#include <stdio.h>\n\
         #include <math.h>\n\
         int main(void) {\n\
         #ifdef HAVE_CBRT_IN_LIBM\n\
         \x20   printf(\"cbrt=yes %.0f\\n\", cbrt(27.0));\n\
         #else\n\
         \x20   printf(\"cbrt=no\\n\");\n\
         #endif\n\
         #ifdef HAVE_NOTHING_IN_LIBM\n\
         \x20   printf(\"nothing=yes\\n\");\n\
         #else\n\
         \x20   printf(\"nothing=no\\n\");\n\
         #endif\n\
         \x20   return 0;\n\
         }\n",
    )
    .unwrap();

    build_ok(&home, &app);
    let out = run_built_exe(&app, "libsym");
    assert!(
        out.out().contains("cbrt=yes 3"),
        "`cbrt` must be found with `-lm`, and the built program must be able \
         to call it:\n{}",
        out.out()
    );
    // Without this, "libs made the link succeed" is indistinguishable from
    // "the link always succeeds".
    assert!(
        out.out().contains("nothing=no"),
        "adding `libs` must not make every symbol resolve:\n{}",
        out.out()
    );
}

/// A `symbol` probe's answer depends on its `prelude`, and both answers are
/// correct.
///
/// `fdatasync` on macOS links but is not declared in `<unistd.h>`. With no
/// prelude the fallback declaration finds it; with `unistd.h` the compile
/// fails. Those are different questions -- "can I call this if I declare it
/// myself" versus "can I call this the way the header offers it" -- which is
/// why `prelude` is part of the probe's cache key rather than an
/// implementation detail.
///
/// The test asserts the *mechanism* (the two are asked independently and
/// cached separately) rather than a specific platform's answers, since which
/// way they differ is a property of the libc.
#[test]
#[cfg(not(windows))]
fn the_prelude_is_part_of_the_question_a_symbol_probe_asks() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let (shim, records) = install_cc_recorder(tmp.path());
    let app = tmp.path().join("preludesym");
    fs::create_dir_all(app.join("src")).unwrap();

    fs::write(
        app.join("Harbour.toml"),
        "[package]\n\
         name = \"preludesym\"\n\
         version = \"0.1.0\"\n\
         \n\
         [targets.preludesym]\n\
         kind = \"exe\"\n\
         sources = [\"src/main.c\"]\n\
         \n\
         [targets.preludesym.probes.named.HAVE_BARE]\n\
         symbol = \"strerror_r\"\n\
         \n\
         [targets.preludesym.probes.named.HAVE_VIA_HEADER]\n\
         symbol = \"strerror_r\"\n\
         prelude = [\"string.h\"]\n",
    )
    .unwrap();
    fs::write(app.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();

    harbour_run_env(&home, &app, &["build"], &[("CC", shim.to_str().unwrap())]).success();

    // Two probes for one symbol must have been measured separately -- a
    // shared cache entry keyed on the symbol alone would answer the second
    // from the first, and the two are not the same question.
    let argvs = recorded_argvs(&records);
    let snippets: Vec<&Vec<String>> = argvs
        .iter()
        .filter(|a| a.iter().any(|x| x.contains("probe.c")))
        .collect();
    assert!(
        snippets.len() >= 2,
        "both `strerror_r` probes must be measured; only {} probe compile(s) \
         were recorded, so one was served from the other's cache entry",
        snippets.len()
    );

    // And the snippets must actually differ: one includes <string.h>, the
    // other declares the symbol itself.
    let dirs: std::collections::BTreeSet<String> = snippets
        .iter()
        .filter_map(|a| a.iter().find(|x| x.contains("probe.c")))
        .cloned()
        .collect();
    assert!(
        dirs.len() >= 2,
        "each probe must get its own directory, or parallel probes race over \
         one path: {dirs:?}"
    );
}

/// `libs` on a kind that does not link is refused, not ignored.
#[test]
fn libs_on_a_non_linking_probe_kind_is_an_error() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let app = tmp.path().join("badlibs");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(app.join("src/main.c"), "int main(void){return 0;}\n").unwrap();
    fs::write(
        app.join("Harbour.toml"),
        "[package]\n\
         name = \"badlibs\"\n\
         version = \"0.1.0\"\n\
         \n\
         [targets.badlibs]\n\
         kind = \"exe\"\n\
         sources = [\"src/main.c\"]\n\
         \n\
         [targets.badlibs.probes.named.HAVE_POLL_H]\n\
         header = \"poll.h\"\n\
         libs = [\"m\"]\n",
    )
    .unwrap();

    let log = harbour_run(&home, &app, &["build"]);
    assert!(
        !log.status.success(),
        "a `libs` key on a non-linking probe kind must fail the build:\n{log}"
    );
    assert!(
        log.combined().contains("only a `symbol` probe uses"),
        "a `header` probe has no link line, so `libs` must be refused rather \
         than parsed and dropped:\n{}",
        log.combined()
    );
}

/// Write a target whose probe answers go into a generated header rather than
/// onto the command line.
#[cfg(not(windows))]
fn write_header_fixture(dir: &std::path::Path, disable_value: u32) {
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join("Harbour.toml"),
        format!(
            "[package]\n\
             name = \"hdr\"\n\
             version = \"0.1.0\"\n\
             \n\
             [targets.hdr]\n\
             kind = \"exe\"\n\
             sources = [\"src/main.c\"]\n\
             \n\
             [targets.hdr.probes]\n\
             emit = {{ header = \"gen_config.h\" }}\n\
             defines = [\"GEN_DISABLE_FTP={disable_value}\", \"GEN_OS=\\\"harbour\\\"\", \"GEN_FLAG\"]\n\
             check_headers = [\"stdio.h\", \"definitely/not/real.h\"]\n\
             check_symbols = [\"poll\", \"definitely_not_a_real_function\"]\n\
             check_sizeof = [\"long\", \"void *\"]\n"
        ),
    )
    .unwrap();
    // The package includes the header *by name*, which is the case a flag
    // list cannot serve and the whole reason this mode exists.
    fs::write(
        dir.join("src/main.c"),
        "#include <stdio.h>\n\
         #include \"gen_config.h\"\n\
         int main(void) {\n\
         \x20   printf(\"ftp=%d\\n\", GEN_DISABLE_FTP);\n\
         \x20   printf(\"os=%s\\n\", GEN_OS);\n\
         #ifdef GEN_FLAG\n\
         \x20   printf(\"flag=yes\\n\");\n\
         #endif\n\
         #ifdef HAVE_STDIO_H\n\
         \x20   printf(\"stdio=yes\\n\");\n\
         #else\n\
         \x20   printf(\"stdio=no\\n\");\n\
         #endif\n\
         #ifdef HAVE_DEFINITELY_NOT_REAL_H\n\
         \x20   printf(\"bogus_header=yes\\n\");\n\
         #else\n\
         \x20   printf(\"bogus_header=no\\n\");\n\
         #endif\n\
         #ifdef HAVE_POLL\n\
         \x20   printf(\"poll=yes\\n\");\n\
         #else\n\
         \x20   printf(\"poll=no\\n\");\n\
         #endif\n\
         #ifdef HAVE_DEFINITELY_NOT_A_REAL_FUNCTION\n\
         \x20   printf(\"bogus_symbol=yes\\n\");\n\
         #else\n\
         \x20   printf(\"bogus_symbol=no\\n\");\n\
         #endif\n\
         \x20   printf(\"long=%d ptr=%d\\n\", SIZEOF_LONG, SIZEOF_VOID_P);\n\
         \x20   return 0;\n\
         }\n",
    )
    .unwrap();
}

/// The generated header's path, which is fixed by `probe_include_dir`.
#[cfg(not(windows))]
fn generated_header_path(dir: &std::path::Path) -> PathBuf {
    dir.join(".harbour/target/debug/probe/hdr-0.1.0/hdr/include/gen_config.h")
}

/// `emit = { header = "..." }` must write a header the package can
/// `#include` by name, carrying both measured answers and declared literals.
///
/// This is the mode curl and openssl need and the reason it exists: 253
/// config lines are not expressible as `-D` flags, and those packages
/// `#include` a config header *by name*, so no arrangement of flags serves
/// them.
///
/// The witness is the built program, which reports what its preprocessor
/// saw. Nothing here arrives as a `-D` -- asserted separately below.
#[test]
#[cfg(not(windows))]
fn a_generated_config_header_carries_answers_and_literals() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let app = tmp.path().join("hdr");
    write_header_fixture(&app, 1);

    build_ok(&home, &app);
    let header = fs::read_to_string(generated_header_path(&app))
        .expect("the generated header must exist where `probe_include_dir` says");

    // Literals, in declaration order, before the measured answers.
    assert!(header.contains("#define GEN_DISABLE_FTP 1"), "{header}");
    assert!(header.contains("#define GEN_OS \"harbour\""), "{header}");
    // A value-less literal becomes `1`, matching how `-DFOO` behaves.
    assert!(header.contains("#define GEN_FLAG 1"), "{header}");

    // Measured answers.
    assert!(header.contains("#define HAVE_STDIO_H 1"), "{header}");
    assert!(header.contains("#define HAVE_POLL 1"), "{header}");
    assert!(header.contains("#define SIZEOF_LONG "), "{header}");

    // A false answer is a commented-out `#undef`, which is what autoconf and
    // CMake produce. It is not decoration: it records that the question was
    // *asked and answered no*, which is what distinguishes this file from a
    // header that forgot something. A reader diffing it against a vendored
    // `curl_config.h` sees the same shape.
    assert!(
        header.contains("/* #undef HAVE_DEFINITELY_NOT_REAL_H */"),
        "a false answer must be recorded as a commented `#undef`, not \
         omitted:\n{header}"
    );
    assert!(
        header.contains("/* #undef HAVE_DEFINITELY_NOT_A_REAL_FUNCTION */"),
        "{header}"
    );
    // And never as `=0`, which `#ifdef` would accept.
    assert!(
        !header.contains("#define HAVE_DEFINITELY_NOT_REAL_H 0"),
        "{header}"
    );

    // The include guard is prefixed. `GEN_CONFIG_H` is a guard a package's
    // own vendored copy may already define, and a header whose guard is
    // already defined expands to nothing -- a build that fails on missing
    // macros with no mention of this file.
    assert!(
        header.contains("#ifndef HARBOUR_PROBE_GEN_CONFIG_H"),
        "{header}"
    );

    let out = run_built_exe(&app, "hdr");
    for expected in [
        "ftp=1",
        "os=harbour",
        "flag=yes",
        "stdio=yes",
        "bogus_header=no",
        "poll=yes",
        "bogus_symbol=no",
    ] {
        assert!(
            out.out().contains(expected),
            "the built program must see `{expected}` through the generated \
             header:\n{}",
            out.out()
        );
    }
}

/// When a header is emitted, the answers must **not** also arrive as `-D`
/// flags.
///
/// Two sources for one fact is the defect shape this whole subsystem has
/// been audited against. The `contribution` function is the single place
/// `emit` is interpreted precisely so this cannot happen; this is the test
/// that says so.
#[test]
#[cfg(not(windows))]
fn emitting_a_header_puts_nothing_on_the_command_line() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let (shim, records) = install_cc_recorder(tmp.path());
    let app = tmp.path().join("hdr");
    write_header_fixture(&app, 1);

    harbour_run_env(&home, &app, &["build"], &[("CC", shim.to_str().unwrap())]).success();

    let argvs = recorded_argvs(&records);
    let real = argvs
        .iter()
        .find(|a| a.iter().any(|x| x.ends_with("main.c")))
        .expect("a compile of main.c");

    let probe_defines: Vec<&String> = real
        .iter()
        .filter(|a| {
            a.starts_with("-DHAVE_") || a.starts_with("-DSIZEOF_") || a.starts_with("-DGEN_")
        })
        .collect();
    assert!(
        probe_defines.is_empty(),
        "with `emit = {{ header = ... }}` the answers live in the file; \
         putting them on the command line as well would be two sources for \
         one fact: {probe_defines:?}"
    );

    // What must be there instead is the `-I`, and it must be **first**: `-I`
    // is first-match-wins, so a package that still vendors a `config.h` of
    // the same name has to get the generated one. That is the migration path
    // off the vendored file.
    let includes: Vec<&String> = real.iter().filter(|a| a.starts_with("-I")).collect();
    let first = includes.first().unwrap_or_else(|| {
        panic!("the generated header's dir must be on the include path: {real:?}")
    });
    assert!(
        first.contains("probe") && first.contains("include"),
        "the generated header's directory must come first on the include \
         path, so it wins over a vendored copy of the same name. Got: \
         {includes:?}"
    );
}

/// The generated header's *content* must be a compile fingerprint input.
///
/// `probes.defines` is the isolating lever: those literals go only into the
/// header, never onto the command line, and the include directory's path
/// does not change. So changing one changes the file's bytes and nothing
/// else. If the object did not recompile, a changed probe answer could leave
/// a stale object -- and the design document listed this as *inferred*, on
/// the grounds that `collect_header_deps` is a textual scanner rather than a
/// preprocessor.
///
/// Worth recording how this was first mis-measured: hand-editing the
/// generated header and rebuilding proves nothing, because the build
/// regenerates it during planning, before any fingerprint is taken. The
/// edit is gone before anything hashes it.
#[test]
#[cfg(not(windows))]
fn changing_the_generated_header_recompiles_what_includes_it() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let app = tmp.path().join("hdr");

    write_header_fixture(&app, 1);
    build_ok(&home, &app);
    assert!(run_built_exe(&app, "hdr").out().contains("ftp=1"));

    // Only the header's bytes change.
    write_header_fixture(&app, 7);
    build_ok(&home, &app);
    assert!(
        run_built_exe(&app, "hdr").out().contains("ftp=7"),
        "the generated header changed, so the object must be recompiled. \
         `ftp=1` here means the header's content is not a fingerprint input \
         and a changed answer can survive into a rebuilt binary:\n{}",
        run_built_exe(&app, "hdr").out()
    );
}

/// The generated header must be byte-identical across clean builds.
///
/// It is a compile fingerprint input, so a line that moved between runs
/// would recompile every translation unit that includes it on every build,
/// forever. For curl that is 196 sources. This is also where `HashMap`
/// iteration would show up, and a measured 18 distinct link orders across 40
/// clean runs of one manifest was fixed only days ago.
#[test]
#[cfg(not(windows))]
fn the_generated_header_is_byte_identical_across_clean_builds() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let app = tmp.path().join("hdr");
    write_header_fixture(&app, 1);

    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..6 {
        fs::remove_dir_all(app.join(".harbour")).ok();
        build_ok(&home, &app);
        seen.insert(fs::read_to_string(generated_header_path(&app)).expect("header"));
    }
    assert_eq!(
        seen.len(),
        1,
        "6 clean builds produced {} different generated headers:\n{:#?}",
        seen.len(),
        seen
    );
}

/// `probes.defines` without a generated header is refused.
#[test]
fn literal_probe_defines_require_a_generated_header() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let app = tmp.path().join("baddefs");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(app.join("src/main.c"), "int main(void){return 0;}\n").unwrap();
    fs::write(
        app.join("Harbour.toml"),
        "[package]\n\
         name = \"baddefs\"\n\
         version = \"0.1.0\"\n\
         \n\
         [targets.baddefs]\n\
         kind = \"exe\"\n\
         sources = [\"src/main.c\"]\n\
         \n\
         [targets.baddefs.probes]\n\
         defines = [\"FOO=1\"]\n\
         check_headers = [\"stdio.h\"]\n",
    )
    .unwrap();

    let log = harbour_run(&home, &app, &["build"]);
    assert!(!log.status.success(), "must fail:\n{log}");
    assert!(
        log.combined().contains("requires"),
        "without a header these are just compile defines, which \
         `[targets.X.private]` already spells -- offering a second spelling \
         invites the reader to look for a difference:\n{}",
        log.combined()
    );
}

/// Probe answers are cached against declarations, **not** against the
/// contents of the filesystem. This pins the behaviour Harbour actually has.
///
/// A characterization test, in the same spirit as
/// `probe_answers_do_not_reach_a_dependent`: it asserts the real behaviour
/// *and* makes the boundary legible, so the limitation is discovered by
/// reading a test rather than by debugging a wrong `#define`.
///
/// The cache key is the toolchain fingerprint, a hash of the pre-probe
/// compile surface, and each probe's own spec. None of those mentions
/// filesystem content, and **none of them can**: the input to a *negative*
/// answer is the absence of a file, so there is no finite set of paths to
/// watch for invalidation. The only alternative to declaration-keyed caching
/// is re-running every probe on every build -- 199 compiler spawns for curl.
/// CMake's `CMakeCache.txt` and autoconf's `config.cache` make the same
/// trade.
///
/// The consequence: installing a system header, or changing SDKs without
/// changing the compiler version, leaves the previous answer in place.
/// `harbour clean --probes` is the way out, and it exists so the way out is
/// not `clean --all`.
#[test]
#[cfg(not(windows))]
fn probe_answers_are_cached_against_declarations_not_the_filesystem() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let app = tmp.path().join("fscache");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::create_dir_all(app.join("vendored")).unwrap();

    // `vendored` is on the include path from the start, so the manifest never
    // changes and neither does the surface key. The only thing that changes
    // is whether a file exists inside a directory already being searched.
    fs::write(
        app.join("Harbour.toml"),
        "[package]\n\
         name = \"fscache\"\n\
         version = \"0.1.0\"\n\
         \n\
         [targets.fscache]\n\
         kind = \"exe\"\n\
         sources = [\"src/main.c\"]\n\
         \n\
         [targets.fscache.private]\n\
         include_dirs = [\"vendored\"]\n\
         \n\
         [targets.fscache.probes]\n\
         check_headers = [\"appears_later.h\"]\n",
    )
    .unwrap();
    fs::write(
        app.join("src/main.c"),
        "#include <stdio.h>\n\
         int main(void) {\n\
         #ifdef HAVE_APPEARS_LATER_H\n\
         \x20   printf(\"answer=yes\\n\");\n\
         #else\n\
         \x20   printf(\"answer=no\\n\");\n\
         #endif\n\
         \x20   return 0;\n\
         }\n",
    )
    .unwrap();

    let answer = |home: &std::path::Path| -> String {
        build_ok(home, &app);
        run_built_exe(&app, "fscache").out().to_string()
    };

    assert_eq!(answer(&home), "answer=no", "the header does not exist yet");

    // It appears, on a path already being searched.
    fs::write(app.join("vendored/appears_later.h"), "/* now here */\n").unwrap();
    assert_eq!(
        answer(&home),
        "answer=no",
        "documented limitation: the probe cache is keyed on declarations, so \
         a file appearing on an unchanged include path does not invalidate \
         it. If this now says `yes`, filesystem-sensitive invalidation has \
         been implemented -- update MANIFEST.md, the design document and this \
         test together"
    );

    // The way out, which must not cost the compiled objects.
    harbour_run(&home, &app, &["clean", "--probes"]).success();
    assert_eq!(
        answer(&home),
        "answer=yes",
        "`clean --probes` must re-measure"
    );

    // And the same in the other direction: a stale `yes` is the more
    // dangerous one, because the package compiles code for a feature that is
    // no longer there.
    fs::remove_file(app.join("vendored/appears_later.h")).unwrap();
    assert_eq!(
        answer(&home),
        "answer=yes",
        "the same limitation in reverse: a header disappearing leaves a stale \
         `yes`"
    );
    harbour_run(&home, &app, &["clean", "--probes"]).success();
    assert_eq!(answer(&home), "answer=no");
}

/// `clean --probes` must re-measure probes and keep compiled objects.
///
/// That is the entire reason it exists rather than telling people to run
/// `clean`. "My probe answer is wrong and the only fix is a full rebuild" is
/// a bad failure mode for a package author iterating on a shim.
#[test]
#[cfg(not(windows))]
fn clean_probes_keeps_compiled_objects() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let app = tmp.path().join("keepobj");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(
        app.join("Harbour.toml"),
        "[package]\n\
         name = \"keepobj\"\n\
         version = \"0.1.0\"\n\
         \n\
         [targets.keepobj]\n\
         kind = \"exe\"\n\
         sources = [\"src/main.c\"]\n\
         \n\
         [targets.keepobj.probes]\n\
         check_headers = [\"stdio.h\"]\n",
    )
    .unwrap();
    fs::write(app.join("src/main.c"), "int main(void){return 0;}\n").unwrap();

    build_ok(&home, &app);
    let obj = find_one(&app, "main.o");
    let before = fs::metadata(&obj).unwrap().modified().unwrap();

    let log = harbour_run(&home, &app, &["clean", "--probes"]).success();
    assert!(
        log.combined().contains("probe cache"),
        "the command must say what it removed:\n{}",
        log.combined()
    );
    assert!(
        obj.exists(),
        "`clean --probes` must keep compiled objects; it is not `clean` with \
         extra steps"
    );

    // The next build re-measures the probe and reuses the object.
    let rebuild = harbour_run(&home, &app, &["build"]).success();
    assert!(
        rebuild.combined().contains("up to date"),
        "the object must be reused after `clean --probes`:\n{}",
        rebuild.combined()
    );
    assert_eq!(
        fs::metadata(&obj).unwrap().modified().unwrap(),
        before,
        "the object must not have been rewritten"
    );
}

/// The package's own object file, found by name under the build tree.
#[cfg(not(windows))]
fn find_one(root: &std::path::Path, name: &str) -> PathBuf {
    fn walk(dir: &std::path::Path, name: &str, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                // Skip the probe scratch tree: it contains its own
                // `probe.o`, and matching that instead would make this
                // assert the opposite of what it means to.
                if p.file_name().is_some_and(|n| n == "probe") {
                    continue;
                }
                walk(&p, name, out);
            } else if p.file_name().is_some_and(|n| n == name) {
                out.push(p);
            }
        }
    }
    let mut found = Vec::new();
    walk(root, name, &mut found);
    found.sort();
    found
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("no `{name}` under {}", root.display()))
}

/// A shim that records its argv and succeeds, standing in for `cl.exe`,
/// `lib.exe` and `link.exe` at once.
#[cfg(unix)]
fn install_msvc_recorder(tmp: &std::path::Path) -> (PathBuf, PathBuf) {
    let records = tmp.join("msvc-argv");
    fs::create_dir_all(&records).unwrap();
    let shim = tmp.join("fake-cl");
    fs::write(
        &shim,
        format!(
            "#!/bin/sh\n\
             out=\"{}/$$.$(od -An -N2 -tu2 /dev/urandom | tr -d ' ')\"\n\
             printf '%s\\n' \"$@\" > \"$out\"\n\
             exit 0\n",
            records.display()
        ),
    )
    .unwrap();
    let mut perms = fs::metadata(&shim).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    fs::set_permissions(&shim, perms).unwrap();
    (shim, records)
}

/// What Harbour hands `cl.exe` and `link.exe` when answering probes.
///
/// Every probe answers `Present` here, because the shim exits 0 no matter
/// what -- the answers are meaningless and are not asserted on. The argv is
/// the whole point.
#[cfg(unix)]
#[test]
fn msvc_probe_command_lines_are_msvc_shaped_and_libm_does_not_exist() {
    use harbour::builder::probe::{run_probes, ProbeEnv};
    use harbour::builder::toolchain::MsvcToolchain;
    use harbour::core::probe::{ProbeKind, ProbeSet};
    use harbour::core::target::{CStandard, CStandardSpec};

    let tmp = temp_dir();
    let (shim, records) = install_msvc_recorder(tmp.path());
    let toolchain = MsvcToolchain::new(shim.clone(), shim.clone(), shim.clone());

    let mut set = ProbeSet::default();
    set.probes.insert(
        "HAVE_WINDOWS_H".to_string(),
        ProbeKind::Header {
            header: "windows.h".to_string(),
            prelude: vec![],
        },
    );
    set.probes.insert(
        "SIZEOF_TIME_T".to_string(),
        ProbeKind::Sizeof {
            ty: "time_t".to_string(),
            prelude: vec![],
        },
    );
    // The entry this test exists for: a manifest written for Unix says
    // `libs = ["m"]`, because that is where `sqrt` lives on every Unix.
    set.probes.insert(
        "HAVE_SQRT".to_string(),
        ProbeKind::Symbol {
            symbol: "sqrt".to_string(),
            prelude: vec!["math.h".to_string()],
            libs: vec!["m".to_string()],
        },
    );

    let scratch = tmp.path().join("scratch");
    let env = ProbeEnv {
        toolchain: &toolchain,
        include_dirs: vec![PathBuf::from("/inc/one")],
        defines: vec![("_WIN32_WINNT".to_string(), Some("0x0601".to_string()))],
        target_cflags: vec![],
        target_ldflags: vec![],
        c_std: Some(CStandardSpec::iso(CStandard::C11)),
        scratch: scratch.clone(),
        toolchain_key: "fake-msvc".to_string(),
    };

    run_probes(&env, &set, "spike/msvc").expect("the shim always exits 0");

    let argvs = recorded_argvs(&records);
    assert!(!argvs.is_empty(), "no command was recorded at all");

    // --- the compile line ---
    let compile = argvs
        .iter()
        .find(|a| a.iter().any(|x| x.ends_with("probe.c")))
        .expect("a probe compile must have been recorded");

    assert_eq!(
        compile[0], "/nologo",
        "MSVC probe compiles must be `cl`-shaped: {compile:?}"
    );
    assert!(
        compile.contains(&"/c".to_string()),
        "a probe compile must not link: {compile:?}"
    );
    assert!(
        compile.iter().any(|a| a.starts_with("/Fo")),
        "MSVC names its object with `/Fo`, not `-o`: {compile:?}"
    );
    assert!(
        compile.iter().any(|a| a.ends_with("probe.obj")),
        "the probe object must use MSVC's `.obj` extension: {compile:?}"
    );
    assert!(
        compile.contains(&"/I/inc/one".to_string()),
        "the surface's include dirs must reach the probe as `/I`: {compile:?}"
    );
    assert!(
        compile.contains(&"/D_WIN32_WINNT=0x0601".to_string()),
        "the surface's defines must reach the probe as `/D`: {compile:?}"
    );
    assert!(
        compile.contains(&"/std:c11".to_string()),
        "a target pinning `c_std = \"11\"` must have its probes measured in \
         that dialect: {compile:?}"
    );
    assert!(
        !compile.iter().any(|a| a.starts_with("-")),
        "nothing GCC-shaped may reach `cl`: {compile:?}"
    );

    // --- the link line, and the bugs ---
    let link = argvs
        .iter()
        .find(|a| a.iter().any(|x| x == "m.lib"))
        .expect("the symbol probe must have produced a link");

    // Found by running this, not by reading: every extension accessor in
    // this codebase is dotless (`"exe"`, `"obj"`, `"lib"`), and
    // `TargetKind::output_filename` joins them with an explicit `.`.
    // `src/builder/probe.rs` writes `format!("probe{}", exe_extension())`
    // for the executable while correctly writing `format!("probe.{}",
    // object_extension())` for the object -- so the MSVC probe executable is
    // named `probeexe`. Harmless today, because a probe only reads the exit
    // code and never opens the file; recorded here so the next reader does
    // not have to rediscover it.
    assert!(
        link.iter().any(|a| a.contains("probeexe")),
        "documenting the dotless-extension defect in src/builder/probe.rs; \
         if this assertion starts failing the bug has been fixed and this \
         test should be inverted: {link:?}"
    );
    // The concrete defect this spike was asked about. `m.lib` does not exist in any MSVC
    // installation -- the math functions are in the CRT, which `cl`'s
    // embedded `/DEFAULTLIB` directives already pull in. `link.exe` answers
    // a missing library with LNK1104 and a non-zero exit, which
    // `src/builder/probe.rs` reads as "the symbol is absent". So every
    // `libs = ["m"]` symbol probe answers `no` on Windows for a function
    // that is right there.
    assert!(
        link.contains(&"m.lib".to_string()),
        "the mapping under test: `libs = [\"m\"]` becomes `m.lib`: {link:?}"
    );
    assert!(
        !link.iter().any(|a| a == "-lm"),
        "the GCC spelling must not reach `link.exe`: {link:?}"
    );

    // --- the snippets, as `cl` would see them ---
    let mut sizeof_src = String::new();
    let mut stack = vec![scratch.clone()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.file_name().is_some_and(|n| n == "probe.c") {
                let text = fs::read_to_string(&p).unwrap();
                if text.contains("sizeof(time_t)") {
                    sizeof_src = text;
                }
            }
        }
    }
    assert!(
        sizeof_src.contains("#ifdef __has_include"),
        "the `sizeof` preamble must be `__has_include`-guarded: {sizeof_src}"
    );
    assert!(
        sizeof_src.contains("? 1 : -1"),
        "the `sizeof` mechanism is a negative array bound: {sizeof_src}"
    );
}

/// Build a package whose probes emit a header, and return that header's text.
///
/// Panics with the build log if the build failed -- which is itself part of
/// the claim, because a `sizeof` probe that cannot see its type is a hard
/// error rather than a wrong answer, and the probe baseline check compiles
/// *and links* an empty program before anything else runs.
#[cfg(target_env = "msvc")]
fn probe_header_for(
    tmp: &std::path::Path,
    home: &std::path::Path,
    label: &str,
    probes: &str,
) -> String {
    let dir = tmp.join(label);
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join("Harbour.toml"),
        format!(
            "[package]\nname = \"probed\"\nversion = \"0.1.0\"\n\n\
             [targets.probed]\nkind = \"exe\"\nsources = [\"src/main.c\"]\n\n\
             [targets.probed.probes]\nemit = {{ header = \"probe_config.h\" }}\n{probes}\n"
        ),
    )
    .unwrap();
    fs::write(
        dir.join("src/main.c"),
        "#include \"probe_config.h\"\nint main(void) { return 0; }\n",
    )
    .unwrap();

    harbour_run(home, &dir, &["build"]).success();

    let mut found = Vec::new();
    fn walk(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.file_name().is_some_and(|n| n == "probe_config.h") {
                out.push(p);
            }
        }
    }
    walk(&dir, &mut found);
    found.sort();
    let path = found
        .first()
        .unwrap_or_else(|| panic!("the build succeeded but wrote no generated header"));
    fs::read_to_string(path).unwrap()
}

/// The `header`, `sizeof` and `symbol` probe kinds, answered by a real MSVC
/// toolchain.
///
/// **Not every kind, and the doc comment said "all three probe kinds" while
/// six existed.** `type` and `constant` are unexercised on Windows, and so
/// was `flag` for as long as it existed -- which mattered, because `flag`
/// was the one kind with MSVC-specific behaviour (`/WX`, to make `D9002`
/// fatal) and that behaviour was never observed on a Windows host. The
/// `flag` kind has since been removed; `type` and `constant` remain
/// unmeasured here and that is a known gap rather than a covered case.
///
/// The answers here are all independently known, which is the property that
/// makes the test mean something: a probe subsystem returning a constant, or
/// one whose every answer is `no` because the toolchain is broken, fails
/// this.
///
/// Worth stating what each group establishes, because each was an open
/// question before this ran:
///
/// - `HAVE_*` for headers: `Toolchain::compile_command` is backend-agnostic
///   and `cl /c` reports a missing include with a non-zero exit, so the
///   `header` kind needs nothing MSVC-specific.
/// - `SIZEOF_TIME_T`: only reachable through the `__has_include`-guarded
///   preamble in `sizeof_preamble`. If `cl` did not honour
///   `#ifdef __has_include` the type would be invisible and the probe would
///   fail the build with `SizeOutOfRange`, so a number here is the proof
///   that the preamble fires. `SIZEOF_OFF_T` does the same for
///   `<sys/types.h>`, which exists in the UCRT.
/// - `SIZEOF_LONG 4`: MSVC's `long` is 32-bit on 64-bit Windows. A probe
///   subsystem that had quietly inherited a Unix answer would say 8.
/// - `HAVE_MALLOC` / `HAVE_POLL`: the `symbol` kind is the only one that
///   links, so these establish that `link.exe` resolves a CRT symbol from
///   the `/DEFAULTLIB` directives `cl` embeds in the object, with no
///   library named on the link line at all -- and that a symbol Windows
///   does not have answers `no` rather than erroring.
#[cfg(target_env = "msvc")]
#[test]
fn msvc_answers_every_probe_kind_correctly() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let header = probe_header_for(
        tmp.path(),
        &home,
        "msvc-probes",
        "check_headers = [\"stdio.h\", \"windows.h\", \"sys/types.h\", \
         \"unistd.h\", \"sys/socket.h\"]\n\
         check_sizeof = [\"int\", \"long\", \"size_t\", \"void *\", \"time_t\", \"off_t\"]\n\
         check_symbols = [\"printf\", \"malloc\", \"poll\", \"strerror_r\"]",
    );

    for present in [
        "#define HAVE_STDIO_H 1",
        "#define HAVE_WINDOWS_H 1",
        "#define HAVE_SYS_TYPES_H 1",
        // 4, not 8: MSVC keeps `long` 32-bit on 64-bit Windows.
        "#define SIZEOF_LONG 4",
        "#define SIZEOF_INT 4",
        "#define SIZEOF_SIZE_T 8",
        "#define SIZEOF_VOID_P 8",
        // The `__has_include` preamble fired. See the doc comment.
        "#define SIZEOF_TIME_T 8",
        "#define SIZEOF_OFF_T 4",
        // The `symbol` kind links, and `link.exe` found the CRT with no
        // library named on the command line.
        "#define HAVE_PRINTF 1",
        "#define HAVE_MALLOC 1",
    ] {
        assert!(
            header.contains(present),
            "expected `{present}` in the MSVC-generated config header:\n{header}"
        );
    }

    for absent in [
        // Genuinely not on Windows. A subsystem answering `yes` here would
        // configure a package for a POSIX platform and then fail to compile.
        "HAVE_UNISTD_H",
        "HAVE_SYS_SOCKET_H",
        "HAVE_POLL",
        "HAVE_STRERROR_R",
    ] {
        assert!(
            header.contains(&format!("/* #undef {absent} */")),
            "`{absent}` must be recorded as asked-and-answered-no:\n{header}"
        );
        assert!(
            !header.contains(&format!("#define {absent}")),
            "`{absent}` must not be defined at all:\n{header}"
        );
    }
}

/// **A known defect, demonstrated rather than fixed.** `#[ignore]`d on
/// purpose: it asserts the *wrong* behaviour, so leaving it live would block
/// the fix it exists to describe.
///
/// `libs = ["m"]` is how every Unix manifest asks for the math library, and
/// `MsvcToolchain::link_exe_command` renders a `libs` entry as `<name>.lib`.
/// There is no `m.lib` in any MSVC installation -- the math functions are in
/// the CRT, which `cl`'s embedded `/DEFAULTLIB` directives already pull in --
/// so `link.exe` fails on a missing input file and
/// `src/builder/probe.rs::compile_and_link` reads that as "the symbol is
/// absent".
///
/// Measured on `windows-latest` with MSVC 14.51: `HAVE_SQRT` is
/// `/* #undef */` with `libs = ["m"]` and `1` without it. The build
/// *succeeds* either way, so the failure is silent -- a package told there
/// is no `sqrt` on a platform that has one.
///
/// Run it with `cargo test -- --ignored msvc_libs_entry` on a Windows host.
#[cfg(target_env = "msvc")]
#[test]
#[ignore = "documents a defect; asserts the wrong answer on purpose"]
fn msvc_libs_entry_named_for_unix_makes_a_symbol_probe_answer_no() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let with_libm = probe_header_for(
        tmp.path(),
        &home,
        "msvc-libm",
        "[targets.probed.probes.named.HAVE_SQRT]\n\
         symbol = \"sqrt\"\nprelude = [\"math.h\"]\nlibs = [\"m\"]",
    );
    let without = probe_header_for(
        tmp.path(),
        &home,
        "msvc-no-libm",
        "[targets.probed.probes.named.HAVE_SQRT]\n\
         symbol = \"sqrt\"\nprelude = [\"math.h\"]",
    );

    assert!(
        without.contains("#define HAVE_SQRT 1"),
        "`sqrt` resolves from the CRT with nothing on the link line, so the \
         probe must answer yes:\n{without}"
    );
    assert!(
        with_libm.contains("/* #undef HAVE_SQRT */"),
        "the defect: adding the Unix library name makes the same question \
         answer no. If this assertion fails, the library-name mapping has \
         been fixed and this test should be deleted:\n{with_libm}"
    );

    // And the mapping is not wrong in general -- a `libs` entry whose name
    // already is the Windows one works. So this is a name-translation
    // problem, not a reason to redesign the link step.
    let ws2 = probe_header_for(
        tmp.path(),
        &home,
        "msvc-ws2",
        "[targets.probed.probes.named.HAVE_HTONL]\n\
         symbol = \"htonl\"\nprelude = [\"winsock2.h\"]\nlibs = [\"ws2_32\"]",
    );
    assert!(
        ws2.contains("#define HAVE_HTONL 1"),
        "`libs = [\"ws2_32\"]` -> `ws2_32.lib` is correct and must keep \
         working:\n{ws2}"
    );
}

// ---------------------------------------------------------------------------
// `type` and `constant` probe kinds.
//
// Appended at the end of the file, as everything else here is: a split of
// this module is deliberately deferred until the concurrent work settles,
// because interleaved appends conflict less than a restructure does.
//
// Every fixture below is chosen so the *right answer is known independently*
// and so that at least one answer in each kind is `no`. A probe subsystem
// whose fixtures all answer `yes` is indistinguishable from a constant, and
// the specific degradation each kind can suffer -- a `member` that stops
// reaching the snippet, a `constant` that becomes a compile-only symbol
// check -- is what these pin.
// ---------------------------------------------------------------------------
/// A fixture exercising both kinds, with a known `yes` and a known `no` in
/// each.
///
/// `struct timeval` exists on every hosted platform and
/// `struct harbour_no_such_struct` exists nowhere. `O_NONBLOCK` is defined
/// by `<fcntl.h>` everywhere POSIX; `HARBOUR_NO_SUCH_CONSTANT` is defined
/// nowhere. `poll` is a function, so it answers `yes` as a `symbol` and
/// `no` as a `constant`, which is the pair that keeps `constant` from
/// degrading into a compile-only symbol check.
#[cfg(not(windows))]
const KINDS_FIXTURE_MANIFEST: &str = r#"[package]
name = "kinds"
version = "0.1.0"

[targets.kinds]
kind = "exe"
sources = ["src/main.c"]

[targets.kinds.probes.named.HAVE_STRUCT_TIMEVAL]
type = "struct timeval"
prelude = ["sys/time.h", "time.h"]

[targets.kinds.probes.named.HAVE_NO_SUCH_STRUCT]
type = "struct harbour_no_such_struct"
prelude = ["stddef.h"]

[targets.kinds.probes.named.HAVE_TIMEVAL_TV_SEC]
type = "struct timeval"
member = "tv_sec"
prelude = ["sys/time.h", "time.h"]

[targets.kinds.probes.named.HAVE_TIMEVAL_NO_SUCH_FIELD]
type = "struct timeval"
member = "harbour_no_such_field"
prelude = ["sys/time.h", "time.h"]

[targets.kinds.probes.named.HAVE_FCNTL_O_NONBLOCK]
constant = "O_NONBLOCK"
prelude = ["fcntl.h"]

[targets.kinds.probes.named.HAVE_NO_SUCH_CONSTANT]
constant = "HARBOUR_NO_SUCH_CONSTANT"
prelude = ["stddef.h"]

[targets.kinds.probes.named.HAVE_POLL_AS_A_CONSTANT]
constant = "poll"
prelude = ["poll.h"]

[targets.kinds.probes.named.HAVE_POLL_AS_A_SYMBOL]
symbol = "poll"
prelude = ["poll.h"]
"#;

/// The consumer. Every claim is a *compile-time* assertion, so a wrong probe
/// answer is a build failure rather than a different line on stdout.
///
/// `HAVE_TIMEVAL_TV_SEC` and `HAVE_TIMEVAL_NO_SUCH_FIELD` are the pair that
/// matters: they are the same type with two different members, so they can
/// only differ if `member` reaches the generated snippet. If it stopped
/// doing so, both would answer `yes` and this file would not compile.
#[cfg(not(windows))]
const KINDS_FIXTURE_SOURCE: &str = r#"#include <stdio.h>

#ifndef HAVE_STRUCT_TIMEVAL
#error "a type that exists on every hosted platform answered no"
#endif
#ifdef HAVE_NO_SUCH_STRUCT
#error "a type that exists nowhere answered yes"
#endif
#ifndef HAVE_TIMEVAL_TV_SEC
#error "struct timeval has tv_sec; the member probe answered no"
#endif
#ifdef HAVE_TIMEVAL_NO_SUCH_FIELD
#error "struct timeval has no such field, so `member` is not reaching the snippet"
#endif
#ifndef HAVE_FCNTL_O_NONBLOCK
#error "O_NONBLOCK is defined by <fcntl.h> on every POSIX platform"
#endif
#ifdef HAVE_NO_SUCH_CONSTANT
#error "a constant that exists nowhere answered yes"
#endif
#ifdef HAVE_POLL_AS_A_CONSTANT
#error "poll is a function, not an integer constant: the constant kind has degraded to a compile-only symbol check"
#endif
#ifndef HAVE_POLL_AS_A_SYMBOL
#error "poll is a symbol that links on every POSIX platform"
#endif

int main(void) {
    printf("timeval=yes\n");
    printf("member-present=yes\n");
    printf("member-absent=no\n");
    printf("o_nonblock=yes\n");
    printf("poll-as-constant=no\n");
    return 0;
}
"#;

#[cfg(not(windows))]
fn write_kinds_fixture(dir: &std::path::Path) {
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(dir.join("Harbour.toml"), KINDS_FIXTURE_MANIFEST).unwrap();
    fs::write(dir.join("src/main.c"), KINDS_FIXTURE_SOURCE).unwrap();
}

/// Both kinds, answered by the real toolchain, with the answers reaching
/// the real compile command.
///
/// This is deliberately one test over one fixture rather than three, because
/// the interesting assertions are the *pairs* -- a type with and without a
/// member, a name asked as a constant and as a symbol -- and splitting them
/// would let one half pass while the other was never built.
#[test]
#[cfg(not(windows))]
fn the_type_and_constant_kinds_answer_correctly_and_reach_the_real_compile_command() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let (shim, records) = install_cc_recorder(tmp.path());
    let app = tmp.path().join("kinds");
    write_kinds_fixture(&app);

    // The `#error`s in the fixture mean a wrong answer is a build failure,
    // so `.success()` is itself an assertion about eight probe answers.
    harbour_run_env(&home, &app, &["build"], &[("CC", shim.to_str().unwrap())]).success();

    let probe_defines = recorded_probe_defines(&records);
    let names: Vec<String> = probe_defines
        .iter()
        .map(|d| {
            d.trim_start_matches("-D")
                .split('=')
                .next()
                .unwrap()
                .to_string()
        })
        .collect();

    // The true answers, in the named entries' manifest order. Every false
    // answer contributes *nothing* -- not `=0`, which `#ifdef` would accept.
    assert_eq!(
        names,
        vec![
            "HAVE_STRUCT_TIMEVAL",
            "HAVE_TIMEVAL_TV_SEC",
            "HAVE_FCNTL_O_NONBLOCK",
            "HAVE_POLL_AS_A_SYMBOL",
        ],
        "the defines the compiler actually received, in order; got \
         {probe_defines:?}"
    );

    // The built program is the independent witness: it reports what the
    // preprocessor saw, so the claim does not rest only on the argv capture.
    let out = run_built_exe(&app, "kinds");
    for line in [
        "timeval=yes",
        "member-present=yes",
        "member-absent=no",
        "o_nonblock=yes",
        "poll-as-constant=no",
    ] {
        assert!(
            out.out().contains(line),
            "the built program must report `{line}`: {}",
            out.out()
        );
    }
}
