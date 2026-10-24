//! Incremental rebuilds: what a second build reuses and what a change recompiles.
#![allow(unused_imports)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::prelude::*;
use predicates::prelude::*;
use tempfile::TempDir;

use harbour::test_support::fixtures::local_registry;

use crate::common::*;

/// File names of the members inside a static archive, parsed straight out
/// of the file.
///
/// GNU `ar` archives, BSD/macOS archives and MSVC `.lib` files are all
/// `!<arch>` archives, so one parser covers every platform without needing
/// `ar` or `lib.exe` on PATH.
///
/// Only the final path component is returned. GNU `ar` stores bare file
/// names, but MSVC's `lib.exe` stores each member under the path it was
/// given on the command line -- and Harbour passes absolute paths -- so
/// comparing raw member names would work on one toolchain and not the
/// other. The file name is the identity that matters anyway: it is what
/// `ar r` matches on, and therefore what decides whether a stale object
/// survives.
///
/// This inspects archive *contents* rather than which definition happened to
/// win symbol resolution. A stale member the linker did not pick this time is
/// still a bug -- that is precisely how the archive bug hid, surfacing only on
/// Windows and only intermittently, when MSVC detection flipped the object
/// extension and both `foo.o` and `foo.obj` sat in the archive.
fn archive_members(path: &std::path::Path) -> Vec<String> {
    let data =
        fs::read(path).unwrap_or_else(|e| panic!("cannot read archive {}: {e}", path.display()));
    assert!(
        data.starts_with(b"!<arch>\n"),
        "{} is not an `!<arch>` static archive (first bytes: {:?})",
        path.display(),
        &data[..data.len().min(16)]
    );

    let field = |bytes: &[u8]| String::from_utf8_lossy(bytes).trim().to_string();
    let base = |name: &str| {
        name.trim_end_matches('\0')
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(name)
            .to_string()
    };
    let mut members = Vec::new();
    let mut long_names: Vec<u8> = Vec::new();
    let mut pos = 8;
    while pos + 60 <= data.len() {
        let header = &data[pos..pos + 60];
        let raw_name = field(&header[0..16]);
        let size: usize = match field(&header[48..58]).parse() {
            Ok(n) => n,
            Err(_) => break,
        };
        let body = &data[pos + 60..(pos + 60 + size).min(data.len())];

        if raw_name == "//" {
            // GNU/MSVC long-name string table; always precedes its users.
            long_names = body.to_vec();
        } else if let Some(len) = raw_name
            .strip_prefix("#1/")
            .and_then(|n| n.parse::<usize>().ok())
        {
            // BSD/macOS: the name is the first `len` bytes of the body.
            let name = base(&field(&body[..len.min(body.len())]));
            if !name.starts_with("__.SYMDEF") {
                members.push(name);
            }
        } else if let Some(offset) = raw_name
            .strip_prefix('/')
            .and_then(|n| n.parse::<usize>().ok())
        {
            let tail = &long_names[offset.min(long_names.len())..];
            let end = tail
                .iter()
                .position(|b| *b == b'/' || *b == b'\n' || *b == 0)
                .unwrap_or(tail.len());
            members.push(base(&field(&tail[..end])));
        } else if raw_name != "/" && raw_name != "/SYM64/" && !raw_name.starts_with("__.SYMDEF") {
            // `/` is the symbol table on GNU and the first linker member on
            // MSVC; neither is a real member.
            members.push(base(raw_name.trim_end_matches('/')));
        }

        pos += 60 + size + (size % 2);
    }
    members.sort();
    members
}

/// The stems of an archive's members -- `one.o` and `one.obj` both become
/// `one`.
///
/// Tests assert on stems rather than on file names because the object
/// extension is not a stable function of the platform: MSVC detection on
/// Windows fails intermittently (`vcvarsall.bat failed: The batch file
/// cannot be found`), and when it does the extension flips from `.obj` to
/// `.o` on the same machine, in the same tree, between two builds. That
/// flakiness is the root cause of the stale-archive bug; a test that hard-
/// codes either extension is asserting on toolchain-detection luck.
fn archive_member_stems(path: &std::path::Path) -> Vec<String> {
    let mut stems: Vec<String> = archive_members(path)
        .iter()
        .map(|m| m.rsplit_once('.').map(|(s, _)| s).unwrap_or(m).to_string())
        .collect();
    stems.sort();
    stems
}

/// Assembly sources compile, get the C preprocessor (so `-I`/`-D` apply),
/// mix with C in one target, and participate in header-dependency
/// invalidation like any other source.
///
/// Gated to the two architectures Harbour's CI runs on, and off MSVC,
/// which assembles with a separate `ml64.exe`/`armasm64.exe` and is
/// rejected with a dedicated error instead.
#[test]
#[cfg(all(
    not(target_env = "msvc"),
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
fn test_assembly_source_builds_links_and_tracks_headers() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "asmapp"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("asmapp");

    // Apple prefixes C symbols with an underscore; ELF does not.
    let sym = if cfg!(target_vendor = "apple") {
        "_fast_add"
    } else {
        "fast_add"
    };

    #[cfg(target_arch = "aarch64")]
    let body = format!(
        "    .text\n    .globl {sym}\n    .align 2\n{sym}:\n\
         \x20   add w0, w0, w1\n    add w0, w0, #BIAS\n    ret\n"
    );
    #[cfg(target_arch = "x86_64")]
    let body = format!(
        "    .text\n    .globl {sym}\n{sym}:\n\
         \x20   movl %edi, %eax\n    addl %esi, %eax\n    addl $BIAS, %eax\n    ret\n"
    );

    // `.S` (capital) so the C preprocessor runs and resolves the include.
    fs::write(
        app_dir.join("src/fast_add.S"),
        format!("#include \"bias.h\"\n{body}"),
    )
    .unwrap();
    fs::write(app_dir.join("src/bias.h"), "#define BIAS 7\n").unwrap();
    fs::write(
        app_dir.join("src/main.c"),
        r#"#include <stdio.h>
int fast_add(int a, int b);

int main(void) {
    printf("%d\n", fast_add(20, 15));
    return 0;
}
"#,
    )
    .unwrap();

    let manifest_path = app_dir.join("Harbour.toml");
    let manifest = fs::read_to_string(&manifest_path).unwrap().replace(
        r#"sources = ["src/**/*.c"]"#,
        r#"sources = ["src/**/*.c", "src/**/*.S"]"#,
    );
    assert!(
        manifest.contains("*.S"),
        "manifest must opt the assembly source in"
    );
    fs::write(&manifest_path, manifest).unwrap();

    harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .success()
        .stderr(predicate::str::contains("Finished"));

    let exe = built_exe_path(&app_dir, "asmapp");
    let out = Command::new(&exe).output().unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "42",
        "20 + 15 + BIAS(7): a wrong answer means the preprocessor never ran \
         on the .S, so the include and define did not apply"
    );

    // A header included *by assembly* must invalidate that object.
    fs::write(app_dir.join("src/bias.h"), "#define BIAS 8\n").unwrap();
    harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .success();

    let out = Command::new(&exe).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "43",
        "changing a header included by the .S must recompile it"
    );
}

/// A static archive must be recreated, not updated in place.
///
/// `ar r` matches members by file name, so a member whose name is no longer
/// produced survives forever: renaming a source leaves the old object in the
/// archive, and the linker can resolve a symbol from that stale copy instead
/// of the current one.
///
/// This is what produced a wrong program on Windows. When MSVC detection
/// fails between two builds the object extension flips from `.obj` to `.o`,
/// so a freshly compiled object lands under a *new* member name, both copies
/// sit in the archive, and the stale one wins -- the library reported its
/// pre-change behaviour even though every file had just been recompiled.
/// Renaming a source reproduces it on any platform.
#[test]
fn test_archive_does_not_keep_objects_from_removed_sources() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let lib = tmp.path().join("valuelib");
    fs::create_dir_all(lib.join("src")).unwrap();
    fs::create_dir_all(lib.join("include")).unwrap();
    fs::write(lib.join("include/valuelib.h"), "int value(void);\n").unwrap();
    fs::write(lib.join("src/one.c"), "int value(void) { return 1; }\n").unwrap();
    fs::write(
        lib.join("Harbour.toml"),
        r#"[package]
name = "valuelib"
version = "0.1.0"

[targets.valuelib]
kind = "staticlib"
sources = ["src/**/*.c"]

[targets.valuelib.surface.compile.public]
include_dirs = ["include"]
"#,
    )
    .unwrap();

    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");
    harbour(&home)
        .args(["add", "valuelib", "--path", "../valuelib"])
        .current_dir(&app_dir)
        .assert()
        .success();
    fs::write(
        app_dir.join("src/main.c"),
        r#"#include <stdio.h>
#include "valuelib.h"

int main(void) {
    printf("%d\n", value());
    return 0;
}
"#,
    )
    .unwrap();

    build_ok(&home, &app_dir);
    assert_eq!(
        run_built_exe(&app_dir, "app").out(),
        "1",
        "sanity: the first build must link the original definition"
    );
    let archive = built_archive_path(&app_dir, "valuelib");
    assert!(
        archive_member_stems(&archive).contains(&"one".to_string()),
        "sanity: the first build's archive must contain an object for \
         `one.c` -- if this fails the member assertions below prove \
         nothing. Members: {:?}",
        archive_members(&archive)
    );

    // Same symbol, different file name and different answer. The old
    // object's member name is no longer produced by any source.
    fs::remove_file(lib.join("src/one.c")).unwrap();
    fs::write(lib.join("src/two.c"), "int value(void) { return 2; }\n").unwrap();

    build_ok(&home, &app_dir);

    // Assert on the archive's *contents* first, and on the program's
    // behaviour second. Which definition wins symbol resolution is not
    // something a test should depend on -- it is why this bug reproduced
    // only on Windows and only intermittently -- so the primary assertion
    // is that the stale member is not there at all.
    //
    // Compared by stem, not by file name: the object extension is not a
    // stable function of the platform. MSVC detection flakiness flips it
    // between `.obj` and `.o` on the same machine between two builds, which
    // is the very thing that produced this bug, so `one.o` would be the
    // wrong thing to look for.
    let stems = archive_member_stems(&archive);
    let members = archive_members(&archive);
    assert!(
        !stems.contains(&"one".to_string()),
        "the archive must contain only objects for sources that still \
         exist, but `one.c`'s object is still a member: {members:?}\n\
         hint: `ar r` matches members by name, so an archive that is \
         updated rather than recreated keeps objects forever"
    );
    assert_eq!(
        stems,
        vec!["two".to_string()],
        "the renamed source's object must be the archive's only member: {members:?}"
    );

    let out = run_built_exe(&app_dir, "app");
    assert_eq!(
        out.out(),
        "2",
        "`1` means a stale member won symbol resolution; archive members: {members:?}"
    );
}

/// A staticlib whose single function returns `value`, plus a public header
/// exposing that value as a macro so tests can invalidate either the
/// dependency's *implementation* or its *interface*.
fn write_answer_lib(dir: &std::path::Path, value: i32, macro_value: i32) {
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::create_dir_all(dir.join("include")).unwrap();
    fs::write(
        dir.join("include/answerlib.h"),
        format!("#define ANSWER_BONUS {macro_value}\nint answer(void);\n"),
    )
    .unwrap();
    fs::write(
        dir.join("src/answer.c"),
        format!("int answer(void) {{ return {value}; }}\n"),
    )
    .unwrap();
    fs::write(
        dir.join("Harbour.toml"),
        r#"[package]
name = "answerlib"
version = "0.1.0"

[targets.answerlib]
kind = "staticlib"
sources = ["src/**/*.c"]

[targets.answerlib.surface.compile.public]
include_dirs = ["include"]
"#,
    )
    .unwrap();
}

/// An app that prints `answer() + ANSWER_BONUS`, so its output is sensitive
/// to both halves of the dependency.
fn write_answer_app(home: &std::path::Path, tmp: &TempDir) -> PathBuf {
    harbour(home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");
    harbour(home)
        .args(["add", "answerlib", "--path", "../answerlib"])
        .current_dir(&app_dir)
        .assert()
        .success();
    fs::write(
        app_dir.join("src/main.c"),
        r#"#include <stdio.h>
#include "answerlib.h"

int main(void) {
    printf("%d\n", answer() + ANSWER_BONUS);
    return 0;
}
"#,
    )
    .unwrap();
    app_dir
}

/// Building a dependency graph twice with nothing changed must reuse every
/// artifact.
///
/// Nothing in this suite asserted anything about incremental freshness
/// before -- `grep -c "up to date"` over it returned 0 -- so a fingerprint
/// that always reported "dirty" and rebuilt the world on every invocation
/// would have been completely invisible, as would a `clean` that failed to
/// remove its cache. It also pins the "build twice" habit itself: a whole
/// class of bug (sources a `prebuild` step generates, which globs expanded
/// at plan time could not see on a clean build until #63) only exists on one
/// of the two builds.
#[test]
fn test_second_build_of_a_dependency_graph_reuses_every_artifact() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    write_answer_lib(&tmp.path().join("answerlib"), 40, 2);
    let app_dir = write_answer_app(&home, &tmp);

    let builds = build_twice(&home, &app_dir);
    assert_eq!(
        run_built_exe(&app_dir, "app").out(),
        "42",
        "sanity: the graph must actually link and run before freshness \
         means anything\n{}",
        builds.clean
    );
    builds.assert_incremental_is_a_no_op();
}

/// Rewrite `answerlib`'s manifest with an `[...surface.abi] toggles` list.
///
/// Everything else is byte-identical to [`write_answer_lib`]'s manifest, so
/// a rebuild after calling this sees exactly one changed input: the ABI
/// declaration.
fn set_answer_lib_abi_toggles(dir: &std::path::Path, toggles: &[&str]) {
    let rendered = toggles
        .iter()
        .map(|t| format!("\"{t}\""))
        .collect::<Vec<_>>()
        .join(", ");
    fs::write(
        dir.join("Harbour.toml"),
        format!(
            r#"[package]
name = "answerlib"
version = "0.1.0"

[targets.answerlib]
kind = "staticlib"
sources = ["src/**/*.c"]

[targets.answerlib.surface.compile.public]
include_dirs = ["include"]

[targets.answerlib.surface.abi]
toggles = [{rendered}]
"#
        ),
    )
    .unwrap();
}

/// Changing a library's declared ABI toggles must re-produce the library.
///
/// `surface.abi.toggles` names the axes an author says their ABI depends on
/// (`pic`, `visibility`, `crt`, `stdlib`). It deliberately emits no flags, so
/// the *only* thing that can honour the declaration is the cache key -- and
/// before this was wired, it reached `ResolvedSurface.abi` and stopped dead:
/// `AbiIdentity::with_surface` had no non-test caller, so the ABI fingerprint
/// varied with nothing but the target triple, the compiler and the target
/// kind. Editing `toggles` produced `All N file(s) up to date` and left every
/// artifact's mtime untouched, which was verified against the unfixed binary
/// before this test was written.
///
/// The second half of the assertion matters as much as the first. A cache key
/// that invalidates too much is its own bug, so the objects must be *reused*:
/// toggles change no compile flag, so nothing may recompile.
#[test]
fn test_changing_abi_toggles_relinks_the_library_but_recompiles_nothing() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let lib_dir = tmp.path().join("answerlib");
    write_answer_lib(&lib_dir, 40, 2);
    set_answer_lib_abi_toggles(&lib_dir, &["pic"]);
    let app_dir = write_answer_app(&home, &tmp);

    let clean = build_ok(&home, &app_dir);
    assert_eq!(
        run_built_exe(&app_dir, "app").out(),
        "42",
        "sanity: the graph must link and run before freshness means \
         anything\n{clean}"
    );

    // The only edit: one more toggle.
    set_answer_lib_abi_toggles(&lib_dir, &["pic", "visibility"]);
    let (log, diff) = rebuild_and_diff(&home, &app_dir);

    // `answerlib.` rather than `answerlib`: the object files live under
    // `.../answerlib/src/` and in an `answerlib-0.1.0` directory, so the bare
    // name matches them too. The archive is the only artifact whose *file
    // name* is `answerlib` followed by an extension -- `libanswerlib.a` on
    // Unix, `answerlib.lib` under MSVC -- so the trailing dot separates the
    // two without hardcoding either spelling or a path separator.
    diff.assert_touched(
        "answerlib.",
        "the library's declared ABI changed, so its archive must be \
         re-produced rather than served from the link cache",
    );

    // `answer.o` is a prefix of `answer.obj`, so this reads the same under
    // MSVC.
    diff.assert_untouched(
        "answer.o",
        "ABI toggles emit no compiler flag, so no source may be recompiled \
         -- a cache key that invalidates more than it must is also a bug",
    );
    diff.assert_untouched(
        "main.o",
        "the app's own sources are untouched by a dependency's ABI \
         declaration",
    );

    assert_eq!(
        run_built_exe(&app_dir, "app").out(),
        "42",
        "the program must still work after the ABI-driven relink\n{log}\n{diff}"
    );
}

/// Changing the compiler between two builds of the same tree must recompile
/// everything -- and the toolchain change must be visible in the fingerprint
/// database.
///
/// Two things are covered here. The first is the invariant itself: a
/// compiler change must invalidate every object, since an object built by
/// another compiler cannot be reused. The second is the mechanism
/// [`Rebuild::assert_reused_everything`] leans on to tell a fingerprinting
/// bug apart from a toolchain change -- if Harbour ever stopped recording
/// the toolchain in its fingerprints, that diagnosis would silently become
/// wrong, so it is asserted rather than assumed.
///
/// Unix-only, and skipped if the second compiler is absent: it needs two
/// compilers that really exist. `CC` is how Harbour's own toolchain
/// detection is overridden (`detect.rs`).
#[test]
#[cfg(unix)]
fn test_changing_the_compiler_between_builds_recompiles_everything() {
    let probe = Command::new("gcc").arg("--version").output();
    if !probe.map(|o| o.status.success()).unwrap_or(false) {
        eprintln!("skipping: no `gcc` on PATH to switch to");
        return;
    }

    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    write_answer_lib(&tmp.path().join("answerlib"), 40, 2);
    let app_dir = write_answer_app(&home, &tmp);

    build_ok(&home, &app_dir);
    let before = recorded_toolchain_hashes(&app_dir);
    assert!(
        !before.is_empty(),
        "the fingerprint database must record a toolchain hash; the \
         conditional freshness assertion is built on it"
    );

    let (rebuild, diff) = rebuild_and_diff_env(&home, &app_dir, &[("CC", "gcc")]);
    let after = recorded_toolchain_hashes(&app_dir);

    assert_ne!(
        before, after,
        "compiling with a different `CC` must change the recorded toolchain \
         fingerprint\n{rebuild}"
    );
    diff.assert_touched(
        "answer",
        "the dependency's object was produced by a different compiler, so it \
         must be recompiled",
    );
    diff.assert_touched(
        "main",
        "the app's object was produced by a different compiler, so it must \
         be recompiled",
    );
    assert_eq!(
        run_built_exe(&app_dir, "app").out(),
        "42",
        "the program must behave the same after being rebuilt with another \
         compiler\n{rebuild}\n{diff}"
    );
}

/// Changing a dependency must recompile exactly what depends on the part
/// that changed, and the consumer must end up running the new code.
///
/// Two failure modes are asserted separately, because they need different
/// fixes and an assertion on the program's output alone cannot tell them
/// apart -- the ambiguity that made the first two attempts at diagnosing
/// the Windows archive bug useless:
///
/// - the dependency's object was never recompiled (a fingerprint that
///   failed to invalidate), versus
/// - it was recompiled and the consumer was never relinked against the new
///   archive.
///
/// The precision cuts the other way too: changing only the dependency's
/// implementation must *not* recompile the consumer's objects, so a
/// fingerprint that invalidates the world on any change fails here.
#[test]
fn test_changing_a_dependency_recompiles_the_right_things() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let lib = tmp.path().join("answerlib");
    write_answer_lib(&lib, 40, 2);
    let app_dir = write_answer_app(&home, &tmp);

    build_ok(&home, &app_dir);
    assert_eq!(run_built_exe(&app_dir, "app").out(), "42", "sanity");

    // Phase 1: the dependency's *implementation* changes. Its object and
    // archive must be redone and the app relinked, but the app's own
    // translation unit does not include anything that changed.
    fs::write(
        lib.join("src/answer.c"),
        "int answer(void) { return 100; }\n",
    )
    .unwrap();

    let (rebuild, diff) = rebuild_and_diff(&home, &app_dir);
    diff.assert_touched(
        "answer",
        "the dependency's source changed, so its object must be recompiled",
    );
    diff.assert_touched(
        "answerlib",
        "the recompiled object must be re-archived; an archive left \
         untouched still holds the old member",
    );
    diff.assert_touched(
        "bin/",
        "the archive changed, so the executable must be relinked -- \
         `recompiled but never relinked` produces a stale program from a \
         successful build",
    );
    diff.assert_untouched(
        "obj/app/",
        "nothing the app's own translation unit includes changed, so \
         recompiling it means the fingerprint is invalidating too much",
    );
    assert_eq!(
        run_built_exe(&app_dir, "app").out(),
        "102",
        "the relinked program must run the new code\n{rebuild}\n{diff}"
    );

    // Phase 2: the dependency's *header* changes. Now the consumer's own
    // object must be recompiled, which only happens if header dependencies
    // are tracked across package boundaries.
    fs::write(
        lib.join("include/answerlib.h"),
        "#define ANSWER_BONUS 900\nint answer(void);\n",
    )
    .unwrap();

    let (rebuild, diff) = rebuild_and_diff(&home, &app_dir);
    diff.assert_touched(
        "obj/app/",
        "the app includes the dependency's public header, so a change to it \
         must recompile the app's object; a build system that only tracks \
         source mtimes silently keeps the old macro value",
    );
    assert_eq!(
        run_built_exe(&app_dir, "app").out(),
        "1000",
        "a header-only change must reach the binary\n{rebuild}\n{diff}"
    );
}

/// Two targets that compile the same source with different flags each keep
/// their own object *and* their own cache entry.
///
/// The compile cache was keyed by the source file. Two targets compiling
/// `shared.c` under different defines overwrote one another's entry, so each
/// looked stale to the other and one file was recompiled on every build, for
/// ever, with nothing to say why. It never showed while each target had its own
/// output directory; sharing one (a library shared with a host tool) is what
/// exposed it, but a plain package with two programs over one source hits it
/// too, which is what this builds.
#[test]
fn test_two_targets_compiling_one_source_do_not_recompile_each_other_forever() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let root = tmp.path().join("twoexe");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Harbour.toml"),
        "[package]\nname = \"twoexe\"\nversion = \"0.1.0\"\n\n\
         [targets.a]\nkind = \"exe\"\nsources = [\"src/a.c\", \"src/shared.c\"]\n\
         [targets.a.surface.compile.private]\ndefines = [\"WHO=1\"]\n\n\
         [targets.b]\nkind = \"exe\"\nsources = [\"src/b.c\", \"src/shared.c\"]\n\
         [targets.b.surface.compile.private]\ndefines = [\"WHO=2\"]\n",
    )
    .unwrap();
    fs::write(root.join("src/shared.c"), "int who(void) { return WHO; }\n").unwrap();
    fs::write(
        root.join("src/a.c"),
        "int who(void);\nint main(void) { return who() - 1; }\n",
    )
    .unwrap();
    fs::write(
        root.join("src/b.c"),
        "int who(void);\nint main(void) { return who() - 2; }\n",
    )
    .unwrap();

    harbour_run(&home, &root, &["build"]).success();

    // Each program must have been built with *its* define, not the other's.
    // (`run_built_exe` requires exit 0, and `who() - N` is 0 only for its own N.)
    run_built_exe(&root, "a");
    run_built_exe(&root, "b");

    // Twice, since the failure alternates: one build records the first target's
    // fingerprint, the next the second's.
    for round in 1..=2 {
        let run = harbour_run(&home, &root, &["build"]);
        assert!(run.status.success(), "{run}");
        let log = run.to_string();
        assert!(
            log.contains("file(s) up to date") && !log.contains("Compiling"),
            "build {round} with nothing changed recompiled a file\n{run}"
        );
    }
}
