//! Cross-compiling, freestanding and bare-metal builds, link inputs, and the target label.
#![allow(unused_imports)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::prelude::*;
use predicates::prelude::*;
use tempfile::TempDir;

use harbour::test_support::fixtures::local_registry;

use crate::common::*;

/// A real bare-metal cross build with no cross toolchain installed: the host
/// clang, `-target aarch64-none-elf`, and an archiver that survived
/// validation.
///
/// The assertion is on the *artifact*, not on the exit status: the bug this
/// route invites is a build that reports success while producing an archive
/// with no members at all (macOS `ar` drops ELF members and still exits 0),
/// or objects quietly built for the host. So the archive is opened and its
/// ELF header read: `e_machine` must be `EM_AARCH64`.
///
/// Skipped rather than failed when the host cannot do this at all -- no
/// clang, a clang without the AArch64 backend, or no archiver that keeps ELF
/// members (a stock macOS install has no `llvm-ar`). Those are properties of
/// the machine, not regressions.
#[test]
fn cross_builds_a_static_lib_with_host_clang_and_target_flag() {
    use harbour::builder::toolchain::{probe_host_clang, HostClangProbe};
    use harbour::core::target::TargetTriple;

    let triple = "aarch64-none-elf";
    if !matches!(
        probe_host_clang(&TargetTriple::parse(triple)),
        HostClangProbe::Ready { .. }
    ) {
        eprintln!("skipping: this host's clang cannot build for {triple}");
        return;
    }

    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let root = tmp.path().join("bare");

    harbour(&home)
        .current_dir(tmp.path())
        .args(["new", "--lib", "bare"])
        .assert()
        .success();

    // No `#include`: host clang has no sysroot for a bare-metal target, and
    // supplying one (or `-ffreestanding`/`-nostdlib`) is a build-flag
    // concern handled separately from discovery.
    fs::write(
        root.join("src/lib.c"),
        "int bare_add(int a, int b) { return a + b; }\n",
    )
    .unwrap();
    fs::write(
        root.join("include/bare/bare.h"),
        "#ifndef BARE_H\n#define BARE_H\nint bare_add(int a, int b);\n#endif\n",
    )
    .unwrap();

    harbour(&home)
        .current_dir(&root)
        .args(["build", "--target-triple", triple])
        .assert()
        .success();

    // Cross builds get their own output tree, keyed on the canonical triple.
    let archive = root
        .join(".harbour/target")
        .join(TargetTriple::parse(triple).canonical())
        .join("debug/lib/libbare.a");
    assert!(
        archive.exists(),
        "expected a cross-built archive at {}",
        archive.display()
    );

    let bytes = fs::read(&archive).unwrap();
    let elf = bytes
        .windows(4)
        .position(|w| w == b"\x7fELF")
        .unwrap_or_else(|| {
            panic!(
                "archive contains no ELF member at all ({} bytes) -- the \
                 archiver dropped the object while reporting success",
                bytes.len()
            )
        });
    // ELF header: e_machine is a little-endian u16 at offset 18. 0xB7 is
    // EM_AARCH64; a host build on x86_64 would read 0x3E, and on an Apple
    // host there would be no ELF magic to find in the first place.
    let e_machine = u16::from_le_bytes([bytes[elf + 18], bytes[elf + 19]]);
    assert_eq!(
        e_machine, 0xB7,
        "archived object is not AArch64 (e_machine {e_machine:#x})"
    );
}

/// `-ffreestanding` has to reach the *compiler*, and the only honest proof
/// is a compiled artifact that behaves differently. `__STDC_HOSTED__` is
/// exactly that: the C standard has a freestanding implementation define it
/// as `0` and a hosted one as `1`, so the library's own return value
/// distinguishes "the flag was passed" from "the flag was declared,
/// deduplicated and reported but never handed to `cc`".
///
/// A static library is deliberately the subject: it has a compile step and
/// no link step, so this isolates the compile half from `-nostdlib`, and it
/// pins the decision that `freestanding` is accepted on `staticlib` (where
/// it changes how the library compiles) while `linker_script`/`entry` are
/// not (nothing would ever read them).
/// Gated off MSVC, which rejects all three keys up front (its equivalents
/// are unwired and it has no linker-script concept); the rejection itself
/// is covered by
/// `test_freestanding_keys_are_rejected_under_msvc`.
#[cfg(not(target_env = "msvc"))]
#[test]
fn test_freestanding_staticlib_is_compiled_without_a_hosted_libc() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let lib = tmp.path().join("barelib");
    fs::create_dir_all(lib.join("src")).unwrap();
    fs::create_dir_all(lib.join("include")).unwrap();
    fs::write(lib.join("include/barelib.h"), "int hosted_level(void);\n").unwrap();
    fs::write(
        lib.join("src/lib.c"),
        "#include \"barelib.h\"\nint hosted_level(void) { return __STDC_HOSTED__; }\n",
    )
    .unwrap();
    fs::write(
        lib.join("Harbour.toml"),
        r#"[package]
name = "barelib"
version = "0.1.0"
requires = "freestanding"

[targets.barelib]
kind = "staticlib"
sources = ["src/**/*.c"]
freestanding = true

[targets.barelib.surface.compile.public]
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
        .args(["add", "barelib", "--path", "../barelib"])
        .current_dir(&app_dir)
        .assert()
        .success();
    fs::write(
        app_dir.join("src/main.c"),
        "#include <stdio.h>\n#include \"barelib.h\"\n\n\
         int main(void) { printf(\"%d %d\\n\", hosted_level(), __STDC_HOSTED__); return 0; }\n",
    )
    .unwrap();

    harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .success();

    let exe = built_exe_path(&app_dir, "app");
    let out = Command::new(&exe).output().unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "0 1",
        "the library must be compiled -ffreestanding (__STDC_HOSTED__ == 0) \
         while the hosted app that consumes it is not: `freestanding` is a \
         per-target build mode, not a graph-wide one"
    );
}

/// The link line a freestanding target produces must be anchored to the
/// package root, and `harbour flags`/`harbour linkplan` must both report
/// exactly what the linker will get.
///
/// Run from a *subdirectory* on purpose. `linker_script` is resolved against
/// the package root, and the failure mode being guarded against is a
/// relative path silently resolved against the process working directory --
/// which happens to be right for the root package and wrong the moment the
/// package is a dependency. That has already been fixed twice here, for
/// `include_dirs` in a `when` block and for a recipe's `source_dir`.
/// Gated off MSVC, which rejects all three keys up front (its equivalents
/// are unwired and it has no linker-script concept); the rejection itself
/// is covered by
/// `test_freestanding_keys_are_rejected_under_msvc`.
#[cfg(not(target_env = "msvc"))]
#[test]
fn test_freestanding_link_line_is_package_rooted_and_reported_consistently() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "payload"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let dir = tmp.path().join("payload");
    fs::create_dir_all(dir.join("boot")).unwrap();
    fs::write(
        dir.join("boot/layout.ld"),
        "ENTRY(_start)\nSECTIONS { . = 0x40000000; .text : { *(.text) } }\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/main.c"),
        "void _start(void) { for (;;) { } }\n",
    )
    .unwrap();
    fs::write(
        dir.join("Harbour.toml"),
        r#"[package]
name = "payload"
version = "0.1.0"
requires = "freestanding"

[targets.payload]
kind = "exe"
sources = ["src/**/*.c"]
freestanding = true
linker_script = "boot/layout.ld"
entry = "_start"
"#,
    )
    .unwrap();

    // Not canonicalized on purpose: temp directories are symlinked on macOS
    // (`/var` -> `/private/var`), so the exact prefix is not the point --
    // *which package directory the tail hangs off* is.
    //
    // Separators are flattened because the reported path legitimately mixes
    // them on Windows: `Path::join` appends the platform separator and
    // leaves the one already inside `boot/layout.ld` alone, so the flag
    // reads `-Wl,-T,C:\...\payload\boot/layout.ld`. The property under test
    // is the anchoring, not the spelling.
    let flat = |s: &str| s.replace('\\', "/");
    let wanted = "payload/boot/layout.ld";
    let cwd_relative = "payload/src/boot";

    // `src/` is not the package root: a cwd-relative resolution would
    // produce `<pkg>/src/boot/layout.ld`, or nothing at all.
    for cmd in ["flags", "linkplan"] {
        let out = harbour(&home)
            .args([cmd, "payload"])
            .current_dir(dir.join("src"))
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let stdout = flat(&String::from_utf8_lossy(&out));

        for expected in ["-nostdlib", "-Wl,--entry=_start", "-Wl,-T,", wanted] {
            assert!(
                stdout.contains(expected),
                "`harbour {cmd}` must report `{expected}`; got:\n{stdout}"
            );
        }
        assert!(
            !stdout.contains(cwd_relative),
            "the linker script must not be resolved against the working \
             directory; got:\n{stdout}"
        );
    }
}

/// A dependency's relative `linker_script` must be looked for inside *that
/// dependency*, not wherever `harbour` was run.
///
/// The script here exists only in the dependent's root, so a cwd-relative
/// resolution would find it and the build would proceed with the wrong
/// file. Correct behaviour is to fail, naming the dependency's own root.
/// Checked through the error message rather than a completed link because
/// no host linker can finish a freestanding link (see [`bare_metal_cc`]).
/// Gated off MSVC, which rejects all three keys up front (its equivalents
/// are unwired and it has no linker-script concept); the rejection itself
/// is covered by
/// `test_freestanding_keys_are_rejected_under_msvc`.
#[cfg(not(target_env = "msvc"))]
#[test]
fn test_a_dependencys_linker_script_is_looked_for_in_that_dependency() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let dep = tmp.path().join("payload");
    fs::create_dir_all(dep.join("src")).unwrap();
    fs::write(
        dep.join("src/start.c"),
        "void _start(void) { for (;;) { } }\n",
    )
    .unwrap();
    fs::write(
        dep.join("Harbour.toml"),
        r#"[package]
name = "payload"
version = "0.1.0"
requires = "freestanding"

[targets.payload]
kind = "exe"
sources = ["src/**/*.c"]
freestanding = true
linker_script = "layout.ld"
"#,
    )
    .unwrap();

    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");
    // Present in the *dependent*, absent in the dependency that names it.
    fs::write(app_dir.join("layout.ld"), "ENTRY(_start)\n").unwrap();
    harbour(&home)
        .args(["add", "payload", "--path", "../payload"])
        .current_dir(&app_dir)
        .assert()
        .success();

    harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .failure()
        .stderr(predicate::str::contains("layout.ld").and(predicate::str::contains("payload")));
}

/// The one test that performs a real freestanding link, and the only place
/// the flags are proven to be *accepted* rather than merely emitted.
///
/// Skipped unless a bare-metal cross driver is installed, because there is
/// no way to fake it: the host linker on macOS rejects both `-nostdlib` and
/// `-T`, so a version of this test that "passed" everywhere would be
/// testing nothing.
#[test]
fn test_freestanding_image_links_with_a_bare_metal_toolchain() {
    let Some((triple, cc)) = bare_metal_cc() else {
        eprintln!(
            "skipping: no bare-metal cross toolchain installed \
             (x86_64-elf-gcc / aarch64-elf-gcc / arm-none-eabi-gcc / \
             riscv64-unknown-elf-gcc); the freestanding link is NOT verified"
        );
        return;
    };
    eprintln!("running the freestanding link against {cc} for {triple}");

    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "payload"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let dir = tmp.path().join("payload");
    fs::write(
        dir.join("layout.ld"),
        "ENTRY(_start)\nSECTIONS { . = 0x100000; .text : { *(.text*) } \
         .rodata : { *(.rodata*) } .data : { *(.data*) } .bss : { *(.bss*) } }\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/main.c"),
        "void _start(void) { volatile int spin = __STDC_HOSTED__; while (spin == 0) { } }\n",
    )
    .unwrap();
    fs::write(
        dir.join("Harbour.toml"),
        r#"[package]
name = "payload"
version = "0.1.0"
requires = "freestanding"

[targets.payload]
kind = "exe"
sources = ["src/**/*.c"]
freestanding = true
linker_script = "layout.ld"
entry = "_start"
"#,
    )
    .unwrap();

    harbour(&home)
        .args(["build", "--target-triple", triple])
        .current_dir(&dir)
        .assert()
        .success();
}

/// The counterpart to the three tests above: on MSVC these keys must be
/// *refused*, not quietly dropped.
///
/// Worth a test of its own rather than leaving MSVC as a hole in the
/// coverage. `-ffreestanding`/`-nostdlib`/`-Wl,-T,` are GCC-driver
/// spellings; `cl`/`link.exe` have neither those nor any linker-script
/// concept, and their nearest equivalents (`/NODEFAULTLIB`, `/ENTRY:`) are
/// not wired. Passing the flags through would fail deep inside the compiler,
/// and dropping them would produce a hosted binary from a manifest asking
/// for a freestanding one.
#[cfg(target_env = "msvc")]
#[test]
fn test_freestanding_keys_are_rejected_under_msvc() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "payload"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let dir = tmp.path().join("payload");
    fs::write(dir.join("layout.ld"), "ENTRY(_start)\n").unwrap();
    fs::write(
        dir.join("src/main.c"),
        "void _start(void) { for (;;) { } }\n",
    )
    .unwrap();
    fs::write(
        dir.join("Harbour.toml"),
        r#"[package]
name = "payload"
version = "0.1.0"
requires = "freestanding"

[targets.payload]
kind = "exe"
sources = ["src/**/*.c"]
freestanding = true
linker_script = "layout.ld"
entry = "_start"
"#,
    )
    .unwrap();

    harbour(&home)
        .args(["build"])
        .current_dir(&dir)
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("freestanding")
                .and(predicate::str::contains("GCC/Clang"))
                .and(predicate::str::contains("linker_script")),
        );
}

/// A `prebuild` generator may emit the linker script itself, and the
/// plan-time existence check must see it.
///
/// This is a direct interaction with the prebuild-ordering change: since
/// generators now run *during planning*, before a target's sources are
/// resolved, they also run before the linker-script check further down the
/// same loop. Under the previous ordering -- generators ran at the start of
/// `NativeBuilder::execute`, i.e. after the whole plan was built -- this
/// check would have rejected a generated script that had not been written
/// yet. Templating a script with memory sizes is ordinary bare-metal
/// practice, so the combination is worth pinning rather than rediscovering.
#[cfg(not(target_env = "msvc"))]
#[test]
fn test_a_generated_linker_script_is_visible_to_the_plan_time_check() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "payload"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let dir = tmp.path().join("payload");
    fs::write(
        dir.join("src/main.c"),
        "void _start(void) { for (;;) { } }\n",
    )
    .unwrap();

    // The script is *not* in the checkout: only the generator can produce it.
    let prebuild = if cfg!(windows) {
        fs::write(
            dir.join("gen.cmd"),
            "@echo off\r\n>layout.ld echo ENTRY^(_start^)\r\n",
        )
        .unwrap();
        "[[targets.payload.prebuild]]\n\
         program = \"cmd\"\n\
         args = [\"/C\", \"gen.cmd\"]\n\
         outputs = [\"layout.ld\"]\n"
    } else {
        fs::write(
            dir.join("gen.sh"),
            "#!/bin/sh\necho 'ENTRY(_start)' > layout.ld\n",
        )
        .unwrap();
        "[[targets.payload.prebuild]]\n\
         program = \"sh\"\n\
         args = [\"gen.sh\"]\n\
         outputs = [\"layout.ld\"]\n"
    };

    fs::write(
        dir.join("Harbour.toml"),
        format!(
            r#"[package]
name = "payload"
version = "0.1.0"
requires = "freestanding"

[targets.payload]
kind = "exe"
sources = ["src/**/*.c"]
freestanding = true
linker_script = "layout.ld"

{prebuild}"#
        ),
    )
    .unwrap();

    assert!(
        !dir.join("layout.ld").exists(),
        "the script must not exist before the generator runs, or this test \
         proves nothing"
    );

    // `linkplan` builds the plan without linking, which is exactly the phase
    // under test -- and the only phase reachable without a bare-metal linker.
    harbour(&home)
        .args(["linkplan", "payload"])
        .current_dir(&dir)
        .assert()
        .success()
        .stdout(predicate::str::contains("-Wl,-T,"));

    assert!(
        dir.join("layout.ld").exists(),
        "the generator should have produced the script during planning"
    );
}

/// A target-level freestanding flag and a `surface.when` block's private
/// ldflags must both reach the link line, and be attributed separately.
///
/// These are two different mechanisms landing in the same sorted, deduped
/// `ldflags` list, so it is worth pinning that neither displaces the other.
/// The combination is also the documented escape hatch for Apple and other
/// hosts whose default linker cannot do a freestanding link: `-fuse-ld=lld`
/// belongs in a conditional private link surface, while `-nostdlib` comes
/// from the target key. Note this only became testable once private
/// requirements in a `when` block were actually applied -- they used to
/// parse and do nothing.
#[cfg(not(target_env = "msvc"))]
#[test]
fn test_conditional_private_ldflags_compose_with_target_level_freestanding() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "payload"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let dir = tmp.path().join("payload");
    fs::write(dir.join("layout.ld"), "ENTRY(_start)\n").unwrap();
    fs::write(
        dir.join("src/main.c"),
        "void _start(void) { for (;;) { } }\n",
    )
    .unwrap();

    // Condition on the host's own os so the block matches wherever this runs.
    let os = if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "linux"
    };
    fs::write(
        dir.join("Harbour.toml"),
        format!(
            r#"[package]
name = "payload"
version = "0.1.0"
requires = "freestanding"

[targets.payload]
kind = "exe"
sources = ["src/**/*.c"]
freestanding = true
linker_script = "layout.ld"

[[targets.payload.surface.when]]
os = "{os}"
[targets.payload.surface.when."link.private"]
ldflags = ["-fuse-ld=lld"]
"#
        ),
    )
    .unwrap();

    let out = harbour(&home)
        .args(["flags", "payload", "--link"])
        .current_dir(&dir)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8_lossy(&out).to_string();

    for expected in ["-nostdlib", "-Wl,-T,", "-fuse-ld=lld"] {
        assert!(
            stdout.contains(expected),
            "`{expected}` must reach the link line; got:\n{stdout}"
        );
    }
    // Attribution must distinguish the two mechanisms, so that `harbour
    // flags` stays a usable answer to "where did this flag come from".
    assert!(
        stdout.contains("target config"),
        "the target keys must be attributed to the target, got:\n{stdout}"
    );
    assert!(
        stdout.contains("surface.link.private"),
        "the `when` block's ldflags must be attributed to the surface, got:\n{stdout}"
    );

    // And the real link line agrees with what `flags` just reported.
    harbour(&home)
        .args(["linkplan", "payload"])
        .current_dir(&dir)
        .assert()
        .success()
        .stdout(
            predicate::str::contains("-nostdlib")
                .and(predicate::str::contains("-fuse-ld=lld"))
                .and(predicate::str::contains("-Wl,-T,")),
        );
}

/// A cross toolchain that can actually produce a freestanding ELF image, if
/// one happens to be installed.
///
/// The host toolchain is not a substitute. Apple's `ld` has no `-T` and
/// refuses `-nostdlib` links outright ("dynamic executables or dylibs must
/// link with libSystem.dylib"), so on macOS there is no way to complete a
/// freestanding link at all; on Linux the host GCC can, which is why this
/// looks for a bare-metal driver rather than assuming.
fn bare_metal_cc() -> Option<(&'static str, &'static str)> {
    for (triple, cc) in [
        ("x86_64-unknown-none", "x86_64-elf-gcc"),
        ("aarch64-unknown-none", "aarch64-elf-gcc"),
        ("arm-none-eabi", "arm-none-eabi-gcc"),
        ("riscv64-unknown-none-elf", "riscv64-unknown-elf-gcc"),
    ] {
        // Probed by running it rather than by looking it up on PATH: the
        // question is whether a working driver exists, and `which` is not
        // in this test crate's dependency graph.
        let usable = Command::new(cc)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        if usable {
            return Some((triple, cc));
        }
    }
    None
}

/// A framework declared on a dependency's public link surface must appear on
/// the link line the linker actually receives.
///
/// `surface.link.public.frameworks` was parsed, propagated across the
/// dependency graph, deduplicated, and printed by `harbour flags` and by the
/// top half of `harbour linkplan` -- while `LinkStep`/`LinkInput` had no
/// field for it, so the linker never saw it. Every existing test passed:
/// nothing linked a framework, and the surface-level output agreed with
/// itself.
///
/// The two halves of `linkplan` are what make this checkable on every
/// platform. The first walks the surface; the "Link line" section is
/// rendered from the `LinkStep` the builder executes. Asserting on the
/// second is asserting on what gets passed. The framework name is
/// deliberately fictional -- linking it would fail, and this is about
/// whether the input is *carried*, not whether macOS has it.
#[test]
fn test_declared_framework_reaches_the_link_line() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let lib = tmp.path().join("flib");
    fs::create_dir_all(lib.join("src")).unwrap();
    fs::create_dir_all(lib.join("include")).unwrap();
    fs::write(lib.join("include/flib.h"), "int flib_value(void);\n").unwrap();
    fs::write(
        lib.join("src/flib.c"),
        "int flib_value(void) { return 1; }\n",
    )
    .unwrap();
    fs::write(
        lib.join("Harbour.toml"),
        r#"[package]
name = "flib"
version = "0.1.0"

[targets.flib]
kind = "staticlib"
sources = ["src/**/*.c"]

[targets.flib.surface.compile.public]
include_dirs = ["include"]

[targets.flib.surface.link.public]
frameworks = ["HarbourTestFramework"]
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
        .args(["add", "flib", "--path", "../flib"])
        .current_dir(&app_dir)
        .assert()
        .success();

    let plan = harbour_run(&home, &app_dir, &["linkplan", "app"]).success();
    let stdout = plan.stdout.clone();
    let (surface_section, link_line) = stdout
        .split_once("Link line (what the linker receives")
        .unwrap_or_else(|| panic!("linkplan printed no link line section\n{plan}"));

    assert!(
        surface_section.contains("-framework HarbourTestFramework"),
        "sanity: the resolved surface must carry the framework, otherwise \
         the assertion below is testing nothing\n{plan}"
    );
    assert_eq!(
        link_line.matches("-framework HarbourTestFramework").count(),
        1,
        "a framework declared on a dependency's public link surface must be \
         passed to the linker exactly once. Appearing in the surface listing \
         but not on the link line is the exact shape of the bug this covers: \
         parsed, propagated, printed -- and dropped before the link.\n{plan}"
    );

    // `harbour flags` reads the surface, so it must agree with the link line
    // rather than being the only place the framework shows up.
    let flags = harbour_run(&home, &app_dir, &["flags", "app"]).success();
    assert!(
        flags.stdout.contains("HarbourTestFramework"),
        "`harbour flags` and the link line must not disagree\n{flags}"
    );
}

/// A relative `kind = "path"` library resolves against the package that
/// *declared* it, not against the process working directory.
///
/// `add_compile_requirements` takes a `root` and anchors `include_dirs` to
/// it; `add_link_requirements` took no root at all, so a relative archive
/// path reached the linker verbatim and resolved against the root package's
/// directory. `MANIFEST.md` spends a paragraph warning about exactly this
/// hazard for `-I`.
///
/// The test is built so that the unanchored behaviour *succeeds* and links
/// the wrong file: both packages carry a `vendor/` archive of the same name,
/// returning different values, so the binary's own output says which one the
/// linker picked. A test that only asserted "the build fails without the fix"
/// would miss the failure mode that matters -- a root package that happens to
/// have a same-named archive gets a silently wrong link.
#[test]
fn a_relative_path_library_anchors_to_the_package_that_declared_it() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    // Two throwaway staticlib packages, built with Harbour, whose archives
    // are then vendored by hand. Using Harbour to produce them keeps the
    // test off `cc`/`ar` invocation details.
    let archive = |name: &str, answer: i32| -> PathBuf {
        let dir = tmp.path().join(format!("src-{name}"));
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(
            dir.join("Harbour.toml"),
            format!(
                "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n\n\
                 [targets.{name}]\nkind = \"staticlib\"\nsources = [\"src/v.c\"]\n"
            ),
        )
        .unwrap();
        fs::write(
            dir.join("src/v.c"),
            format!("int vendored_answer(void) {{ return {answer}; }}\n"),
        )
        .unwrap();
        harbour(&home)
            .args(["build"])
            .current_dir(&dir)
            .assert()
            .success();
        built_archive_path(&dir, name)
    };
    let real = archive("real", 42);
    let decoy = archive("decoy", 99);

    // The dependency: declares the vendored archive by a path relative to
    // its own root, the same way it declares `include_dirs`.
    let lib_dir = tmp.path().join("lib");
    fs::create_dir_all(lib_dir.join("src")).unwrap();
    fs::create_dir_all(lib_dir.join("include")).unwrap();
    fs::create_dir_all(lib_dir.join("vendor")).unwrap();
    fs::copy(&real, lib_dir.join("vendor/libvend.a")).unwrap();
    fs::write(
        lib_dir.join("Harbour.toml"),
        r#"[package]
name = "mylib"
version = "0.1.0"

[targets.mylib]
kind = "staticlib"
sources = ["src/l.c"]
public_headers = ["include/**/*.h"]

[targets.mylib.surface.compile.public]
include_dirs = ["include"]

[targets.mylib.surface.link.public]
libs = [{ kind = "path", path = "vendor/libvend.a" }]
"#,
    )
    .unwrap();
    fs::write(lib_dir.join("include/l.h"), "int l(void);\n").unwrap();
    fs::write(
        lib_dir.join("src/l.c"),
        "int vendored_answer(void);\nint l(void) { return vendored_answer(); }\n",
    )
    .unwrap();

    // The root package, with a same-named archive of its own at the same
    // relative path. This is what makes the unanchored bug silent.
    let app_dir = tmp.path().join("app");
    fs::create_dir_all(app_dir.join("src")).unwrap();
    fs::create_dir_all(app_dir.join("vendor")).unwrap();
    fs::copy(&decoy, app_dir.join("vendor/libvend.a")).unwrap();
    fs::write(
        app_dir.join("Harbour.toml"),
        r#"[package]
name = "app"
version = "0.1.0"

[dependencies]
mylib = { path = "../lib" }

[targets.app]
kind = "exe"
sources = ["src/m.c"]
"#,
    )
    .unwrap();
    fs::write(
        app_dir.join("src/m.c"),
        "#include <stdio.h>\n#include <l.h>\nint main(void) { printf(\"%d\\n\", l()); return 0; }\n",
    )
    .unwrap();

    harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .success();

    assert_eq!(
        run_built_exe(&app_dir, "app").out(),
        "42",
        "`vendor/libvend.a` is declared in `mylib`'s manifest, so it must \
         resolve inside `mylib`'s tree. `99` means the link resolved it \
         against the root package's directory instead and quietly used the \
         root's same-named archive"
    );

    // `harbour flags` must report the same path the link used, or the
    // inspection command sends you looking in the wrong tree.
    let flags = harbour(&home)
        .args(["flags", "app"])
        .current_dir(&app_dir)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let flags = String::from_utf8_lossy(&flags).to_string();

    // Assert the *property* -- absolute, and ending in the declared relative
    // path -- rather than matching the path string. Two things defeat string
    // comparison on Windows: the anchored path keeps the manifest's own
    // separator inside it (`...\\lib\\vendor/libvend.a`, which Windows accepts),
    // and the CI runner hands the test an 8.3 short temp dir (`RUNNER~1`)
    // while Harbour normalizes the package root to the long form
    // (`runneradmin`). Both spellings name the same file; neither is a bug.
    let line = flags
        .lines()
        .find(|l| l.contains("libvend.a"))
        .unwrap_or_else(|| panic!("`harbour flags` never mentions the library:\n{flags}"));
    let token = line.split("# from:").next().unwrap_or(line).trim();
    let token_path = Path::new(token);
    assert!(
        token_path.is_absolute(),
        "`harbour flags` must name the anchored path, not the relative one: \
         got `{token}`\n{flags}"
    );
    assert!(
        token_path.ends_with(Path::new("vendor").join("libvend.a")),
        "the anchored path must still end in the declared relative path: \
         got `{token}`\n{flags}"
    );
}

// `arch` is the literal triple component (#138)
//
// The same Debian armhf compiler is `arch = "arm"` through one triple and
// `arch = "armv7"` through another, and a manifest keyed on one silently
// does not apply to the other -- with no warning, because an unmatched
// `when` block is normal and expected. openssl caught that one step before
// it produced a 64-bit `bn_conf.h` on a 32-bit target.
/// A spelling in the *same* ISA family as this host's architecture, but not
/// the same architecture.
///
/// Panics rather than skipping on an architecture it has no entry for: a
/// test that quietly does nothing on a new platform is a test that stopped
/// checking, and the fix is one line here.
fn sibling_arch_spelling() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "aarch64_be",
        "x86_64" => "x86_64h",
        "x86" => "i686",
        "arm" => "armv7",
        "powerpc64" => "powerpc64le",
        other => panic!(
            "no sibling architecture spelling recorded for `{other}`; add one \
             to `sibling_arch_spelling` (it must be a different spelling in \
             the same ISA family)"
        ),
    }
}

#[test]
fn a_when_block_for_a_sibling_arch_spelling_is_reported() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");

    let sibling = sibling_arch_spelling();
    fs::write(
        app_dir.join("Harbour.toml"),
        format!(
            "[package]\n\
             name = \"app\"\n\
             version = \"0.1.0\"\n\
             \n\
             [targets.app]\n\
             kind = \"bin\"\n\
             sources = [\"src/**/*.c\"]\n\
             \n\
             [[targets.app.when]]\n\
             arch = \"{sibling}\"\n\
             defines = [\"NEVER_MATCHES\"]\n"
        ),
    )
    .unwrap();
    fs::write(app_dir.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();

    // The build must still succeed: this is an advisory, not an error. A
    // package legitimately may have no block for the current architecture.
    let log = build_ok(&home, &app_dir);
    let text = log.combined();
    assert!(
        text.contains("same architecture family spelled differently"),
        "a `when` block naming a sibling spelling of this architecture \
         ({sibling} vs {}) must be reported, since it contributes nothing \
         and looks like it was meant to\n{log}",
        std::env::consts::ARCH
    );
    assert!(
        text.contains(sibling),
        "the advisory must name the spelling that did not match\n{log}"
    );
}

#[test]
fn a_when_block_for_an_unrelated_arch_is_not_reported() {
    // The normal case, and the reason the advisory is narrow. openssl has
    // aarch64 and x86_64 assembly and nothing for 32-bit ARM; that build is
    // *supposed* to match nothing and compile the portable baseline. A
    // warning there would fire on every correct manifest and be tuned out,
    // which is how a real warning stops being read.
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");

    // `mips` is in no family this project's CI hosts belong to.
    fs::write(
        app_dir.join("Harbour.toml"),
        "[package]\n\
         name = \"app\"\n\
         version = \"0.1.0\"\n\
         \n\
         [targets.app]\n\
         kind = \"bin\"\n\
         sources = [\"src/**/*.c\"]\n\
         \n\
         [[targets.app.when]]\n\
         arch = \"mips\"\n\
         defines = [\"NEVER_MATCHES\"]\n",
    )
    .unwrap();
    fs::write(app_dir.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();

    let log = build_ok(&home, &app_dir);
    assert!(
        !log.combined().contains("same architecture family"),
        "an unrelated architecture block must stay silent\n{log}"
    );
}

/// The other architecture of this host's OS, as a triple, if this machine can
/// build for it (macOS always can; Linux needs the distribution's cross
/// compiler). `None` means the caller should skip, as the other cross tests do.
fn cross_target_for_this_host() -> Option<&'static str> {
    let target = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "x86_64-apple-darwin",
        ("macos", _) => "arm64-apple-darwin",
        ("linux", "x86_64") => "aarch64-unknown-linux-gnu",
        ("linux", "aarch64") => "x86_64-unknown-linux-gnu",
        (os, arch) => {
            eprintln!("skipping: no other-architecture target for {os}/{arch}");
            return None;
        }
    };
    match harbour::builder::toolchain::detect_toolchain(Some(
        &harbour::core::target::TargetTriple::parse(target),
    )) {
        Ok(_) => Some(target),
        Err(e) => {
            eprintln!("skipping: no toolchain for {target}: {e}");
            None
        }
    }
}

/// A one-file library, ready to build.
fn write_label_project(root: &Path) {
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/lib.c"), "int answer(void) { return 42; }\n").unwrap();
    fs::write(
        root.join("Harbour.toml"),
        "[package]\nname = \"labelled\"\nversion = \"0.1.0\"\n\n\
         [targets.labelled]\nkind = \"staticlib\"\nsources = [\"src/lib.c\"]\n",
    )
    .unwrap();
}

/// `harbour build`'s `Finished` line names the target the build was *for*, from
/// the same resolution the build used -- the flag, else `toolchain.target`, else
/// the host -- instead of from the flag alone.
///
/// With `toolchain.target` set a plain `harbour build` cross-compiles (it writes
/// an archive for the other architecture), but the line said `[native]`; only
/// `--target-triple` made it name the target. A label that can disagree with what
/// was built is worse than none.
#[test]
#[cfg(unix)]
fn test_the_finished_line_names_the_target_from_toolchain_config() {
    let Some(target) = cross_target_for_this_host() else {
        return;
    };
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let root = tmp.path().join("labelled");
    write_label_project(&root);

    // A host build says `native`, as it always has.
    let native = harbour_run(&home, &root, &["build"]);
    assert!(native.status.success(), "{native}");
    assert!(native.to_string().contains("[native]"), "{native}");

    harbour(&home)
        .current_dir(&root)
        .args(["toolchain", "override", "--target", target])
        .assert()
        .success();

    let configured = harbour_run(&home, &root, &["build"]);
    assert!(configured.status.success(), "{configured}");
    let text = configured.to_string();
    assert!(
        text.contains(&format!("[{target}]")) && !text.contains("[native]"),
        "a build for {target} configured in toolchain.target must say so\n{text}"
    );

    // The flag wins over the config and is shown as typed, as before.
    let flagged = harbour_run(&home, &root, &["build", "--target-triple", target]);
    assert!(flagged.status.success(), "{flagged}");
    assert!(
        flagged.to_string().contains(&format!("[{target}]")),
        "{flagged}"
    );
}

/// The same fact in machine-readable form: `build-started` carries the target the
/// build is for. It was a hardcoded `"native"` -- even with `--target-triple` --
/// so a tool reading the stream could not tell a cross build from a host one.
#[test]
#[cfg(unix)]
fn test_the_json_build_started_event_carries_the_effective_target() {
    let Some(target) = cross_target_for_this_host() else {
        return;
    };
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let root = tmp.path().join("labelled");
    write_label_project(&root);

    let started_target = |run: &RunLog| -> String {
        run.to_string()
            .lines()
            .find(|l| l.contains("\"reason\":\"build-started\""))
            .map(|l| {
                let v: serde_json::Value = serde_json::from_str(l.trim()).unwrap();
                v["target_triple"].as_str().unwrap().to_string()
            })
            .unwrap_or_else(|| panic!("no build-started event in\n{run}"))
    };

    let native = harbour_run(&home, &root, &["build", "--message-format", "json"]);
    assert_eq!(started_target(&native), "native");

    let flagged = harbour_run(
        &home,
        &root,
        &[
            "build",
            "--message-format",
            "json",
            "--target-triple",
            target,
        ],
    );
    assert_eq!(
        started_target(&flagged),
        target,
        "the flag must reach the event"
    );

    harbour(&home)
        .current_dir(&root)
        .args(["toolchain", "override", "--target", target])
        .assert()
        .success();
    let configured = harbour_run(&home, &root, &["build", "--message-format", "json"]);
    assert_eq!(started_target(&configured), target, "so must the config");
}

/// The backend's cross-compilation check follows the target the build is for, not
/// only the `--target-triple` flag. `custom` cannot cross-compile: with the target
/// set in config the check was skipped, so the build went ahead for a target the
/// backend had just said it cannot do.
#[test]
#[cfg(unix)]
fn test_a_backend_that_cannot_cross_compile_is_refused_for_a_configured_target() {
    let Some(target) = cross_target_for_this_host() else {
        return;
    };
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let root = tmp.path().join("labelled");
    write_label_project(&root);

    harbour(&home)
        .current_dir(&root)
        .args(["toolchain", "override", "--target", target])
        .assert()
        .success();

    let run = harbour_run(&home, &root, &["build", "--backend", "custom"]);
    assert!(!run.status.success(), "{run}");
    assert!(
        run.to_string()
            .contains("does not support cross-compilation"),
        "the refusal must name the reason\n{run}"
    );
}
