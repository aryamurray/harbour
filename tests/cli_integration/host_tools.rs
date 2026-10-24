//! Host targets and the generators that run them.
#![allow(unused_imports)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::prelude::*;
use predicates::prelude::*;
use tempfile::TempDir;

use harbour::test_support::fixtures::local_registry;

use crate::common::*;

/// A package where a generator runs a program the *same package* builds.
///
/// `gen` is a host program over the static library `core`; it prints a header
/// whose number comes from `core`, so the header is only right if `core` was
/// compiled and linked into `gen` and `gen` actually ran during the build.
/// `app` includes that header and links `core` too, as CPython's interpreter
/// and its freeze tool share a core library.
fn write_host_tool_package(root: &Path, step_extra: &str) {
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/core.h"), "int core_value(void);\n").unwrap();
    fs::write(
        root.join("src/core.c"),
        "int core_value(void) { return 41; }\n",
    )
    .unwrap();
    fs::write(
        root.join("src/gen.c"),
        "#include <stdio.h>\n#include \"core.h\"\n\
         int main(void) { printf(\"#define GENERATED %d\\n\", core_value() + 1); return 0; }\n",
    )
    .unwrap();
    fs::write(
        root.join("src/app.c"),
        "#include <stdio.h>\n#include \"generated/value.h\"\n#include \"core.h\"\n\
         int main(void) { printf(\"%d %d\\n\", GENERATED, core_value()); return 0; }\n",
    )
    .unwrap();
    fs::write(
        root.join("run_gen.sh"),
        "#!/bin/sh\nset -e\nmkdir -p generated\n\"$HARBOUR_TOOL_GEN\" > generated/value.h\n",
    )
    .unwrap();
    fs::write(
        root.join("Harbour.toml"),
        format!(
            "[package]\nname = \"hosted\"\nversion = \"0.1.0\"\n\n\
             [targets.core]\nkind = \"staticlib\"\nsources = [\"src/core.c\"]\n\n\
             [targets.core.surface.compile.public]\ninclude_dirs = [\"src\"]\n\n\
             [targets.gen]\nkind = \"exe\"\nhost = true\nsources = [\"src/gen.c\"]\n\n\
             [targets.gen.deps]\ncore = {{ local = true }}\n\n\
             [targets.app]\nkind = \"exe\"\nsources = [\"src/app.c\"]\n\n\
             [targets.app.surface.compile.private]\ninclude_dirs = [\".\"]\n\n\
             [targets.app.deps]\ncore = {{ local = true }}\n\n\
             [[targets.app.prebuild]]\nprogram = \"sh\"\nargs = [\"run_gen.sh\"]\n\
             tools = [\"gen\"]\noutputs = [\"generated/value.h\"]\n{step_extra}"
        ),
    )
    .unwrap();
}

#[test]
#[cfg(unix)]
fn test_a_generator_runs_a_host_program_built_from_its_own_package() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let root = tmp.path().join("hosted");
    write_host_tool_package(&root, "");

    harbour_run(&home, &root, &["build"]).success();

    let header = fs::read_to_string(root.join("generated/value.h")).unwrap();
    assert_eq!(
        header.trim(),
        "#define GENERATED 42",
        "the generator must have run `gen`"
    );
    let output = Command::new(built_exe_path(&root, "app")).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "42 41");
}

/// A cross build builds the tool for the machine running the build.
///
/// The generator *runs* `gen`, and the build only succeeds if that worked, so
/// a tool built for the target (a binary this machine cannot execute, or one
/// compiled with the target's configuration) fails here. The artifact check
/// pins where it went: the host tree, not the cross tree. Needs a toolchain for
/// the other architecture, so skipped without one, as the other cross tests are.
#[test]
#[cfg(unix)]
fn test_a_cross_build_builds_the_host_tool_for_the_host() {
    let target = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "x86_64-apple-darwin",
        ("macos", _) => "arm64-apple-darwin",
        ("linux", "x86_64") => "aarch64-unknown-linux-gnu",
        ("linux", "aarch64") => "x86_64-unknown-linux-gnu",
        (os, arch) => {
            eprintln!("skipping: no other-architecture target for {os}/{arch}");
            return;
        }
    };
    if let Err(e) = harbour::builder::toolchain::detect_toolchain(Some(
        &harbour::core::target::TargetTriple::parse(target),
    )) {
        eprintln!("skipping: no toolchain for {target}: {e}");
        return;
    }

    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let root = tmp.path().join("hosted");
    write_host_tool_package(&root, "");
    // The cross-built program only needs to compile; it cannot run here.
    let manifest = fs::read_to_string(root.join("Harbour.toml")).unwrap();
    fs::write(
        root.join("Harbour.toml"),
        manifest.replace(
            "[targets.app]\nkind = \"exe\"",
            "[targets.app]\nkind = \"staticlib\"",
        ),
    )
    .unwrap();

    harbour(&home)
        .current_dir(&root)
        .args(["build", "--target-triple", target])
        .assert()
        .success();

    assert_eq!(
        fs::read_to_string(root.join("generated/value.h"))
            .unwrap()
            .trim(),
        "#define GENERATED 42",
        "the generator must have run a tool this machine can execute"
    );
    let tools: Vec<PathBuf> = walk_files(&root.join(".harbour"))
        .into_iter()
        .filter(|p| {
            p.file_name().is_some_and(|n| n == "gen")
                && p.components().any(|c| c.as_os_str() == "host-tools")
        })
        .collect();
    assert_eq!(tools.len(), 1, "expected one host-built `gen`: {tools:?}");
    assert!(
        !tools[0]
            .to_string_lossy()
            .contains(&harbour::core::target::TargetTriple::parse(target).canonical()),
        "the tool belongs to the host tree, not the cross tree: {}",
        tools[0].display()
    );
}

/// A host target is not part of a build for the target, so naming one with
/// `--target` is refused instead of producing a host binary where a target
/// binary belongs.
#[test]
#[cfg(unix)]
fn test_selecting_a_host_target_directly_is_refused() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let root = tmp.path().join("hosted");
    write_host_tool_package(&root, "");

    let run = harbour_run(&home, &root, &["build", "--target", "gen"]);
    assert!(!run.status.success());
    assert!(
        run.to_string().contains("host target"),
        "expected the refusal to say why\n{run}"
    );
}

/// `host` and `tools` are checked together when the manifest loads.
#[test]
#[cfg(unix)]
fn test_host_and_tools_mistakes_are_refused_with_the_names_involved() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let cases: [(&str, &str, &str); 3] = [
        // (replace, with, expected in the error)
        (
            "tools = [\"gen\"]",
            "tools = [\"missing\"]",
            "no target of that name",
        ),
        (
            "[targets.gen]\nkind = \"exe\"\nhost = true",
            "[targets.gen]\nkind = \"exe\"",
            "not a host target",
        ),
        (
            "[targets.gen]\nkind = \"exe\"\nhost = true",
            "[targets.gen]\nkind = \"staticlib\"\nhost = true",
            "not an executable",
        ),
    ];
    for (i, (from, to, expected)) in cases.iter().enumerate() {
        let root = tmp.path().join(format!("hosted{i}"));
        write_host_tool_package(&root, "");
        let manifest = fs::read_to_string(root.join("Harbour.toml")).unwrap();
        fs::write(root.join("Harbour.toml"), manifest.replace(from, to)).unwrap();

        let run = harbour_run(&home, &root, &["build"]);
        assert!(!run.status.success(), "case {i} must be refused");
        assert!(
            run.to_string().contains(expected),
            "case {i}: expected `{expected}`\n{run}"
        );
    }
}

/// A tool whose own generator needs that same tool has no valid build order,
/// and the error says so with the chain instead of recursing.
#[test]
#[cfg(unix)]
fn test_a_host_tool_that_needs_itself_is_a_cycle() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let root = tmp.path().join("hosted");
    write_host_tool_package(&root, "");
    let manifest = fs::read_to_string(root.join("Harbour.toml")).unwrap();
    fs::write(
        root.join("Harbour.toml"),
        manifest.replace(
            "[targets.gen.deps]\ncore = { local = true }\n",
            "[targets.gen.deps]\ncore = { local = true }\n\n\
             [[targets.gen.prebuild]]\nprogram = \"true\"\ntools = [\"gen\"]\n",
        ),
    )
    .unwrap();

    let run = harbour_run(&home, &root, &["build"]);
    assert!(!run.status.success());
    assert!(
        run.to_string().contains("cycle between host tools"),
        "expected the cycle to be named\n{run}"
    );
}

/// A tool that changes re-runs a generator that declares `inputs`, even though
/// no declared input moved, and an unchanged tool does not.
///
/// The tool binary's content is in the generator's fingerprint. Without it a
/// step with `inputs` would be skipped after its tool was rebuilt from changed
/// source, leaving a header produced by the old program.
#[test]
#[cfg(unix)]
fn test_a_changed_host_tool_reruns_a_generator_that_declares_inputs() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let root = tmp.path().join("hosted");
    write_host_tool_package(&root, "inputs = [\"run_gen.sh\"]\n");
    fs::write(
        root.join("run_gen.sh"),
        "#!/bin/sh\nset -e\necho run >> runs.log\nmkdir -p generated\n\
         \"$HARBOUR_TOOL_GEN\" > generated/value.h\n",
    )
    .unwrap();
    let runs = |root: &Path| {
        fs::read_to_string(root.join("runs.log"))
            .map(|s| s.lines().count())
            .unwrap_or(0)
    };

    harbour_run(&home, &root, &["build"]).success();
    assert_eq!(runs(&root), 1);
    harbour_run(&home, &root, &["build"]).success();
    assert_eq!(
        runs(&root),
        1,
        "unchanged tool, unchanged inputs: the step is skipped"
    );

    fs::write(
        root.join("src/gen.c"),
        "#include <stdio.h>\n#include \"core.h\"\n\
         int main(void) { printf(\"#define GENERATED %d\\n\", core_value() + 2); return 0; }\n",
    )
    .unwrap();
    harbour_run(&home, &root, &["build"]).success();
    assert_eq!(runs(&root), 2, "a rebuilt tool must re-run the generator");
    assert_eq!(
        fs::read_to_string(root.join("generated/value.h"))
            .unwrap()
            .trim(),
        "#define GENERATED 43"
    );
}

/// A host target's `when` blocks, and those of the siblings it links, are
/// evaluated against the **host**, even when the build is for another
/// architecture.
///
/// This is why a host target gets a context of its own instead of only a
/// different compiler path: CPython's `pyconfig.h`, LuaJIT's
/// `-DLUAJIT_TARGET=...` and the kernel's `ARCH` each describe a machine, and a
/// tool configured for the *target* is a host program that is wrong or cannot
/// run. `core` defines `ARCH_MARK` only when the arch matches the host; the
/// tool reports it. In a cross build the target's arch is the other one, so a
/// tool evaluated against the target would report 0.
#[test]
#[cfg(unix)]
fn test_a_host_tool_sees_the_hosts_platform_in_a_cross_build() {
    let (target, host_arch) = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => ("x86_64-apple-darwin", "aarch64"),
        ("macos", _) => ("arm64-apple-darwin", "x86_64"),
        ("linux", "x86_64") => ("aarch64-unknown-linux-gnu", "x86_64"),
        ("linux", "aarch64") => ("x86_64-unknown-linux-gnu", "aarch64"),
        (os, arch) => {
            eprintln!("skipping: no other-architecture target for {os}/{arch}");
            return;
        }
    };
    if let Err(e) = harbour::builder::toolchain::detect_toolchain(Some(
        &harbour::core::target::TargetTriple::parse(target),
    )) {
        eprintln!("skipping: no toolchain for {target}: {e}");
        return;
    }

    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let root = tmp.path().join("hosted");
    write_host_tool_package(&root, "");

    fs::write(
        root.join("src/core.c"),
        "int core_value(void) {\n#ifdef ARCH_MARK\n    return 41;\n#else\n    return 0;\n#endif\n}\n",
    )
    .unwrap();
    let manifest = fs::read_to_string(root.join("Harbour.toml")).unwrap();
    fs::write(
        root.join("Harbour.toml"),
        manifest
            .replace(
                "[targets.app]\nkind = \"exe\"",
                "[targets.app]\nkind = \"staticlib\"",
            )
            .replace(
                "[targets.gen]",
                &format!(
                    "[[targets.core.when]]\narch = \"{host_arch}\"\n\
                     defines = [\"ARCH_MARK\"]\n\n\
                     [targets.gen]"
                ),
            ),
    )
    .unwrap();

    harbour(&home)
        .current_dir(&root)
        .args(["build", "--target-triple", target])
        .assert()
        .success();

    assert_eq!(
        fs::read_to_string(root.join("generated/value.h"))
            .unwrap()
            .trim(),
        "#define GENERATED 42",
        "the tool must have been configured for the host ({host_arch}), not for {target}"
    );
}

/// A host tool can see the host's platform and the build's at once, and its
/// generators are told what the program is for.
///
/// LuaJIT's `buildvm` is compiled for the build machine and configured for the
/// target; CPython's tools are configured for the host. `arch` is the host's
/// inside a host tool and `target_arch` is the build's, so both exist in one
/// manifest. `core` sums two marks, one per spelling, and the tool prints the
/// sum: a tool that read both conditions against the host would lose the second,
/// and one that read both against the target would lose the first.
#[test]
#[cfg(unix)]
fn test_a_host_tool_sees_both_the_host_and_the_target_and_its_generator_is_told_the_target() {
    let (target, host_arch, target_arch) = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => ("x86_64-apple-darwin", "aarch64", "x86_64"),
        ("macos", _) => ("arm64-apple-darwin", "x86_64", "aarch64"),
        ("linux", "x86_64") => ("aarch64-unknown-linux-gnu", "x86_64", "aarch64"),
        ("linux", "aarch64") => ("x86_64-unknown-linux-gnu", "aarch64", "x86_64"),
        (os, arch) => {
            eprintln!("skipping: no other-architecture target for {os}/{arch}");
            return;
        }
    };
    if let Err(e) = harbour::builder::toolchain::detect_toolchain(Some(
        &harbour::core::target::TargetTriple::parse(target),
    )) {
        eprintln!("skipping: no toolchain for {target}: {e}");
        return;
    }

    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let root = tmp.path().join("hosted");
    write_host_tool_package(&root, "");

    // 20 for the host's `arch`, 21 for the build's `target_arch`.
    fs::write(
        root.join("src/core.c"),
        "int core_value(void) {\n    int v = 0;\n#ifdef HOST_MARK\n    v += 20;\n#endif\n\
         #ifdef TARGET_MARK\n    v += 21;\n#endif\n    return v;\n}\n",
    )
    .unwrap();
    let dump = write_env_dumping_generator(&root, "").replace("targets.app", "targets.gen");
    let manifest = fs::read_to_string(root.join("Harbour.toml")).unwrap();
    fs::write(
        root.join("Harbour.toml"),
        manifest
            .replace(
                "[targets.app]\nkind = \"exe\"",
                "[targets.app]\nkind = \"staticlib\"",
            )
            .replace(
                "[targets.gen]\nkind",
                &format!(
                    "[[targets.core.when]]\narch = \"{host_arch}\"\ndefines = [\"HOST_MARK\"]\n\n\
                     [[targets.core.when]]\ntarget_arch = \"{target_arch}\"\n\
                     defines = [\"TARGET_MARK\"]\n\n{dump}\n[targets.gen]\nkind"
                ),
            ),
    )
    .unwrap();

    harbour(&home)
        .current_dir(&root)
        .args(["build", "--target-triple", target])
        .assert()
        .success();

    assert_eq!(
        fs::read_to_string(root.join("generated/value.h"))
            .unwrap()
            .trim(),
        "#define GENERATED 42",
        "the tool must see host `arch` ({host_arch}) *and* build `target_arch` ({target_arch})"
    );

    // The tool's own generator produces files for the program, so it is told
    // the build's platform, and `HARBOUR_HOST_*` still names this machine.
    let env = fs::read_to_string(root.join("generated/env.txt")).unwrap();
    assert_eq!(
        env_dump_get(&env, "HARBOUR_TARGET_TRIPLE").as_deref(),
        Some(target)
    );
    assert_eq!(
        env_dump_get(&env, "HARBOUR_TARGET_ARCH").as_deref(),
        Some(target_arch),
        "a host tool's generator is told what the program is for\n{env}"
    );
    assert_eq!(
        env_dump_get(&env, "HARBOUR_CROSS_COMPILING").as_deref(),
        Some("1")
    );
    assert_ne!(
        env_dump_get(&env, "HARBOUR_HOST_TRIPLE"),
        env_dump_get(&env, "HARBOUR_TARGET_TRIPLE"),
        "host and target must differ in a cross build"
    );
}

/// On a native build, a library a host tool links is the very one the program
/// links, compiled once.
///
/// `gen` (a host tool) and `app` both link `core`. The host *is* the target, so
/// a second compile of `core` for the tool is pure waste: same toolchain, same
/// flags, same sources. Counted from the compiler invocations themselves, not
/// from object timestamps, so it cannot be satisfied by two compiles that
/// happen to write one file. A second build compiles nothing.
#[test]
#[cfg(unix)]
fn test_a_native_build_compiles_a_library_shared_with_a_host_tool_once() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let root = tmp.path().join("hosted");
    write_host_tool_package(&root, "");

    let records_dir = tmp.path().join("records");
    fs::create_dir_all(&records_dir).unwrap();
    // One recorder for both builds: the compiler's path is part of the toolchain
    // fingerprint, so a different script on the second build would (correctly)
    // rebuild everything.
    let (shim, records) = install_cc_recorder(&records_dir);
    let compiles = |records: &Path| {
        recorded_argvs(records)
            .iter()
            .filter(|argv| argv.iter().any(|a| a == "-c"))
            .count()
    };
    let compiles_of_core = |records: &Path| {
        recorded_argvs(records)
            .iter()
            .filter(|argv| argv.iter().any(|a| a == "-c"))
            .filter(|argv| argv.iter().any(|a| a.ends_with("src/core.c")))
            .count()
    };

    harbour_run_env(&home, &root, &["build"], &[("CC", shim.to_str().unwrap())]).success();
    assert_eq!(
        compiles_of_core(&records),
        1,
        "`core` must be compiled once and shared by the tool and the program"
    );
    let app = Command::new(built_exe_path(&root, "app")).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&app.stdout).trim(), "42 41");
    let after_first = compiles(&records);

    harbour_run_env(&home, &root, &["build"], &[("CC", shim.to_str().unwrap())]).success();
    assert_eq!(
        compiles(&records),
        after_first,
        "a build with nothing changed must compile nothing"
    );
}
