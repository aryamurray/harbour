//! Pre-build generators: what they are told, `inputs` skipping, generated sources.
#![allow(unused_imports)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::prelude::*;
use predicates::prelude::*;
use tempfile::TempDir;

use harbour::test_support::fixtures::local_registry;

use crate::common::*;

/// Write a code generator script into `dir` that emits `generated/table.h`
/// (declaring `generated_answer`) and `generated/table.c` (defining it as
/// `value`), and return the `[[targets.app.prebuild]]` block that invokes it.
///
/// The generated symbol is deliberately a variable rather than a function:
/// the script body then contains no parentheses or braces, which keeps the
/// `cmd.exe` and POSIX `sh` versions equivalent without quoting games.
fn write_answer_generator(dir: &std::path::Path, value: i32) -> String {
    if cfg!(windows) {
        fs::write(
            dir.join("gen.cmd"),
            format!(
                "@echo off\r\n\
                 if not exist generated mkdir generated\r\n\
                 >generated\\table.h echo extern int generated_answer;\r\n\
                 >generated\\table.c echo int generated_answer = {value};\r\n"
            ),
        )
        .unwrap();
        "[[targets.app.prebuild]]\n\
         program = \"cmd\"\n\
         args = [\"/C\", \"gen.cmd\"]\n\
         outputs = [\"generated/table.c\", \"generated/table.h\"]\n"
            .to_string()
    } else {
        fs::write(
            dir.join("gen.sh"),
            format!(
                "#!/bin/sh\n\
                 mkdir -p generated\n\
                 echo 'extern int generated_answer;' > generated/table.h\n\
                 echo 'int generated_answer = {value};' > generated/table.c\n"
            ),
        )
        .unwrap();
        "[[targets.app.prebuild]]\n\
         program = \"sh\"\n\
         args = [\"gen.sh\"]\n\
         outputs = [\"generated/table.c\", \"generated/table.h\"]\n"
            .to_string()
    }
}

/// Lay out an `app` package whose `[[targets.app.prebuild]]` generator emits
/// a source that the target compiles, with `sources` written as `sources_toml`.
fn write_codegen_app(app_dir: &std::path::Path, sources_toml: &str, value: i32) {
    let prebuild = write_answer_generator(app_dir, value);
    fs::write(
        app_dir.join("Harbour.toml"),
        format!(
            "[package]\n\
             name = \"app\"\n\
             version = \"0.1.0\"\n\
             \n\
             [targets.app]\n\
             kind = \"bin\"\n\
             sources = {sources_toml}\n\
             \n\
             [targets.app.private]\n\
             include_dirs = [\"generated\"]\n\
             \n\
             {prebuild}"
        ),
    )
    .unwrap();

    fs::write(
        app_dir.join("src/main.c"),
        "#include <stdio.h>\n\
         #include \"table.h\"\n\
         \n\
         int main(void) { printf(\"%d\\n\", generated_answer); return 0; }\n",
    )
    .unwrap();
}

#[test]
fn test_prebuild_generated_source_is_compiled_on_clean_build() {
    // Regression coverage for "run a code generator, then compile its
    // output" being broken on a *clean* build only.
    //
    // Source globs were expanded while the plan was built, but pre-build
    // steps ran later, during execution. So on a fresh checkout
    // `generated/*.c` matched nothing, the generated translation unit was
    // absent from the plan, and the link failed on the symbol it defines --
    // while the very next build succeeded, because by then the generator had
    // left the file on disk. That asymmetry is why this has to assert on
    // both builds, and on what the binary *prints*, not on exit status: for
    // a `staticlib` target the same bug produces a successful build and an
    // archive quietly missing a member.
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");

    write_codegen_app(&app_dir, r#"["src/**/*.c", "generated/*.c"]"#, 42);

    // Build 1: clean. Nothing under `generated/` exists yet; only the
    // generator knows what will be there.
    build_ok(&home, &app_dir);
    assert_eq!(
        run_built_exe(&app_dir, "app").out(),
        "42",
        "the generated translation unit must be compiled into the *clean* build"
    );

    // Build 2: incremental, nothing changed. The generator re-runs and
    // rewrites byte-identical output, which must not force a recompile.
    //
    // Asserted against the build tree rather than against the log. The
    // previous form -- `rebuild_log.contains("up to date")` -- could not
    // fail: the compiling line reads `Compiling 1 file(s) (0 up to date)`,
    // so the needle is present even when every file was recompiled, which
    // is exactly the thing this build is supposed to rule out.
    rebuild(&home, &app_dir).assert_reused_everything(
        "the generator re-ran and rewrote byte-identical output, so nothing \
         may be recompiled",
    );
    assert_eq!(run_built_exe(&app_dir, "app").out(), "42");

    // Build 3: the generator now emits a different value. The fingerprint of
    // the generated source is taken after regeneration, so this must
    // recompile and change what the program prints.
    write_answer_generator(&app_dir, 99);
    let (log, diff) = rebuild_and_diff(&home, &app_dir);
    diff.assert_touched(
        "table",
        "the generated source changed, so its object must be recompiled",
    );
    assert_eq!(
        run_built_exe(&app_dir, "app").out(),
        "99",
        "regenerated output must be recompiled, not served from the \
         fingerprint cache\n{log}\n{diff}"
    );
}

#[test]
fn test_prebuild_generated_source_named_explicitly_is_compiled() {
    // A generated source listed individually rather than matched by a glob.
    //
    // Naming a source that does not exist is normally a hard error, and this
    // used to need an exemption for targets with `prebuild`, because at plan
    // time the generator had not run and its output legitimately wasn't
    // there yet. Now that generators run before sources are resolved, the
    // file *is* present and no exemption is needed -- so this must build,
    // and the existence check can stay strict for everyone.
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");

    write_codegen_app(&app_dir, r#"["src/main.c", "generated/table.c"]"#, 7);

    // Built twice on purpose: the ordering bug this covers was visible only
    // on the clean build, and the second build must additionally reuse the
    // generated object rather than recompiling it because the generator
    // re-ran.
    build_twice(&home, &app_dir).assert_incremental_is_a_no_op();
    assert_eq!(
        run_built_exe(&app_dir, "app").out(),
        "7",
        "the explicitly named generated source must be compiled and linked"
    );
}

#[test]
fn test_prebuild_that_skips_a_declared_output_fails_loudly() {
    // A generator that exits 0 without writing what its `outputs` declares
    // is the failure this ordering fix exists to surface. Before, the named
    // source silently vanished from the compile set; now the build stops at
    // the generator, naming it and the file it owes.
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");

    write_codegen_app(&app_dir, r#"["src/**/*.c", "generated/*.c"]"#, 42);

    // Replace the generator with one that only writes the header.
    if cfg!(windows) {
        fs::write(
            app_dir.join("gen.cmd"),
            "@echo off\r\n\
             if not exist generated mkdir generated\r\n\
             >generated\\table.h echo extern int generated_answer;\r\n",
        )
        .unwrap();
    } else {
        fs::write(
            app_dir.join("gen.sh"),
            "#!/bin/sh\n\
             mkdir -p generated\n\
             echo 'extern int generated_answer;' > generated/table.h\n",
        )
        .unwrap();
    }

    harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .failure()
        .stderr(predicate::str::contains("did not produce"))
        .stderr(predicate::str::contains("table.c"));
}

/// Write a generator script named `stem` into `dir` that emits
/// `generated/<stem>.c` defining `int <symbol> = <value>;`, and return the
/// `program`/`args` TOML fragment that invokes it.
fn write_symbol_generator(dir: &std::path::Path, stem: &str, symbol: &str, value: i32) -> String {
    if cfg!(windows) {
        fs::write(
            dir.join(format!("{stem}.cmd")),
            format!(
                "@echo off\r\n\
                 if not exist generated mkdir generated\r\n\
                 >generated\\{stem}.c echo int {symbol} = {value};\r\n"
            ),
        )
        .unwrap();
        format!("program = \"cmd\"\nargs = [\"/C\", \"{stem}.cmd\"]\n")
    } else {
        fs::write(
            dir.join(format!("{stem}.sh")),
            format!(
                "#!/bin/sh\n\
                 mkdir -p generated\n\
                 echo 'int {symbol} = {value};' > generated/{stem}.c\n"
            ),
        )
        .unwrap();
        format!("program = \"sh\"\nargs = [\"{stem}.sh\"]\n")
    }
}

/// A generator that always fails. Used as a tripwire: if a non-matching
/// `when` block's generator runs, the build dies and the test says so.
fn write_failing_generator(dir: &std::path::Path, stem: &str) -> String {
    if cfg!(windows) {
        fs::write(
            dir.join(format!("{stem}.cmd")),
            "@echo off\r\necho this generator must not run 1>&2\r\nexit /b 1\r\n",
        )
        .unwrap();
        format!("program = \"cmd\"\nargs = [\"/C\", \"{stem}.cmd\"]\n")
    } else {
        fs::write(
            dir.join(format!("{stem}.sh")),
            "#!/bin/sh\necho 'this generator must not run' >&2\nexit 1\n",
        )
        .unwrap();
        format!("program = \"sh\"\nargs = [\"{stem}.sh\"]\n")
    }
}

/// The `os` value Harbour evaluates `[[targets.X.when]]` against on this host.
fn host_os_condition() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

#[test]
fn test_conditional_prebuild_runs_only_the_matching_generator() {
    // `prebuild` used to be a plain `Vec<CustomCommand>` on the target with
    // no `when` support, so a per-platform generator was inexpressible --
    // and a generator is often the *most* platform-specific thing a package
    // does. openssl runs perlasm scripts with `flavour elf` on Linux x86_64
    // and a different set with `flavour macosx` on Darwin; there is no
    // single script to run unconditionally.
    //
    // Three generators here: one unconditional, one behind a `when` that
    // matches this host, and one behind a `when` that cannot match. The
    // program's output proves the first two ran and were *compiled in*; the
    // third is rigged to exit non-zero, so the build succeeding at all
    // proves it was skipped rather than merely tolerated.
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");

    let base = write_symbol_generator(&app_dir, "base", "base_value", 1);
    let plat = write_symbol_generator(&app_dir, "plat", "plat_value", 10);
    let never = write_failing_generator(&app_dir, "never");

    fs::write(
        app_dir.join("Harbour.toml"),
        format!(
            "[package]\n\
             name = \"app\"\n\
             version = \"0.1.0\"\n\
             \n\
             [targets.app]\n\
             kind = \"bin\"\n\
             sources = [\"src/**/*.c\", \"generated/*.c\"]\n\
             \n\
             [[targets.app.prebuild]]\n\
             {base}\
             outputs = [\"generated/base.c\"]\n\
             \n\
             [[targets.app.when]]\n\
             os = \"{os}\"\n\
             \n\
             [[targets.app.when.prebuild]]\n\
             {plat}\
             outputs = [\"generated/plat.c\"]\n\
             \n\
             [[targets.app.when]]\n\
             arch = \"s390x\"\n\
             \n\
             [[targets.app.when.prebuild]]\n\
             {never}\
             outputs = [\"generated/never.c\"]\n",
            os = host_os_condition()
        ),
    )
    .unwrap();

    fs::write(
        app_dir.join("src/main.c"),
        "#include <stdio.h>\n\
         \n\
         extern int base_value;\n\
         extern int plat_value;\n\
         \n\
         int main(void) { printf(\"%d\\n\", base_value + plat_value); return 0; }\n",
    )
    .unwrap();

    // Clean build. If the matching `when` generator were ignored,
    // `plat_value` would be undefined and this would fail to link.
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
        "11",
        "both the unconditional and the matching conditional generator must \
         run and have their output compiled in"
    );

    // The non-matching generator must never have been invoked.
    assert!(
        !app_dir.join("generated/never.c").exists(),
        "a `when` block whose condition does not match must not run its generator"
    );

    // Second build, for the same reason the unconditional case checks it:
    // this class of bug passes on one build and fails on the other.
    harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .success();
    let out = Command::new(&exe).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "11");
}

/// Lay out an `app` package whose only `prebuild` step dumps its environment.
fn write_env_dumping_app(app_dir: &std::path::Path, extra_toml: &str) {
    let prebuild = write_env_dumping_generator(app_dir, extra_toml);
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
             {prebuild}"
        ),
    )
    .unwrap();
    fs::write(app_dir.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();
}

/// The architecture token a triple uses for this host, as an independent
/// oracle for `HARBOUR_TARGET_ARCH`.
///
/// Rust's `ARCH` agrees with the triple's first component on every platform
/// this project builds on, with one exception: 32-bit x86 is `x86` to Rust
/// and `i386`..`i686` in a triple, so that case accepts the family.
fn host_arch_tokens() -> Vec<&'static str> {
    match std::env::consts::ARCH {
        "x86" => vec!["x86", "i386", "i486", "i586", "i686"],
        other => vec![other],
    }
}

#[test]
fn prebuild_generator_is_told_about_its_target_and_toolchain() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");
    write_env_dumping_app(&app_dir, "");

    build_ok(&home, &app_dir);
    let dump = fs::read_to_string(app_dir.join("generated/env.txt")).unwrap();
    let get = |k: &str| {
        env_dump_get(&dump, k)
            .unwrap_or_else(|| panic!("`{k}` was not in the generator's environment:\n{dump}"))
    };

    // Parity with `recipe`: where the sources are, and where the artifacts
    // are expected.
    let package_root = PathBuf::from(get("HARBOUR_PACKAGE_ROOT"));
    assert_eq!(
        package_root.canonicalize().unwrap(),
        app_dir.canonicalize().unwrap(),
        "HARBOUR_PACKAGE_ROOT must be the declaring package's root\n{dump}"
    );
    let artifact_dir = get("HARBOUR_ARTIFACT_DIR");
    assert!(
        artifact_dir.contains(".harbour") && artifact_dir.ends_with("lib"),
        "HARBOUR_ARTIFACT_DIR must be this target's lib directory, got \
         `{artifact_dir}`\n{dump}"
    );

    // What platform this is. `HARBOUR_TARGET_OS` is the value a `when` block
    // matches, so it is `macos` and never `darwin` -- checked against Rust's
    // own name for the host, which uses the same spelling.
    assert_eq!(
        get("HARBOUR_TARGET_OS"),
        std::env::consts::OS,
        "HARBOUR_TARGET_OS must be the normalized name a `when` condition \
         matches\n{dump}"
    );
    let arch = get("HARBOUR_TARGET_ARCH");
    assert!(
        host_arch_tokens().contains(&arch.as_str()),
        "HARBOUR_TARGET_ARCH `{arch}` is not this host's architecture \
         ({:?})\n{dump}",
        host_arch_tokens()
    );

    // A native build: these two invariants need no oracle at all, and they
    // are the ones a cross build inverts.
    assert_eq!(
        get("HARBOUR_TARGET_TRIPLE"),
        get("HARBOUR_HOST_TRIPLE"),
        "a native build must report the same triple twice\n{dump}"
    );
    assert_eq!(
        get("HARBOUR_CROSS_COMPILING"),
        "0",
        "a native build is not a cross build\n{dump}"
    );

    // The toolchain. This is the group that turns openssl's `CC = "cc"`
    // guess into a fact, so it is not enough for the variables to exist:
    // each must name a tool that is really on this machine.
    for var in ["CC", "CXX", "AR"] {
        let tool = get(var);
        assert!(
            !tool.is_empty(),
            "{var} must name the tool Harbour resolved for the target\n{dump}"
        );
        assert!(
            std::path::Path::new(&tool).exists(),
            "{var}=`{tool}` does not exist, so it is a guess rather than the \
             resolved toolchain\n{dump}"
        );
    }

    // Native: the machine that runs the generator *is* the target, so the
    // host tools are the target tools -- same toolchain, not a second lookup
    // that could disagree with it.
    for var in ["CC", "CXX", "AR"] {
        assert_eq!(
            get(&format!("HARBOUR_HOST_{var}")),
            get(var),
            "a native build's host {var} must be its target {var}\n{dump}"
        );
    }
}

/// A generator that is a compiled program can build it with
/// `HARBOUR_HOST_CC` and run it, even when the target is another
/// architecture.
///
/// This is LuaJIT's `buildvm` shape: compile a tool, run it, use its output.
/// The tool has to be built for the machine running the build, so `CC` --
/// the target's compiler -- cannot be used. Asserted by behaviour, not by
/// the variable existing: the generator compiles a C program with
/// `HARBOUR_HOST_CC` and executes it, and the build succeeds only if that
/// worked.
///
/// The target is the *other* architecture of the host's own OS. On macOS
/// that needs no installed toolchain (`-arch`); on Linux it needs the
/// distribution's cross compiler (`gcc-aarch64-linux-gnu` or
/// `gcc-x86-64-linux-gnu`), and the test is skipped, loudly, when it is
/// absent -- a property of the machine, not a regression. CI installs it.
/// The target is a static library so nothing links and no target libc is
/// needed: the point is the generator, not the target code.
#[test]
#[cfg(unix)]
fn prebuild_generator_can_build_and_run_a_host_tool_when_cross_compiling() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let app_dir = tmp.path().join("app");
    fs::create_dir_all(app_dir.join("src")).unwrap();

    let target = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "x86_64-apple-darwin",
        ("macos", _) => "arm64-apple-darwin",
        ("linux", "x86_64") => "aarch64-unknown-linux-gnu",
        ("linux", "aarch64") => "x86_64-unknown-linux-gnu",
        (os, arch) => {
            eprintln!("skipping: no other-architecture target is defined for {os}/{arch}");
            return;
        }
    };
    if let Err(e) = harbour::builder::toolchain::detect_toolchain(Some(
        &harbour::core::target::TargetTriple::parse(target),
    )) {
        eprintln!("skipping: no toolchain for {target}: {e}");
        return;
    }

    fs::write(
        app_dir.join("gen.c"),
        "#include <stdio.h>\nint main(void) { puts(\"generated\"); return 0; }\n",
    )
    .unwrap();
    fs::write(
        app_dir.join("gen.sh"),
        "#!/bin/sh\nset -e\n\
         [ \"$HARBOUR_CROSS_COMPILING\" = 1 ] || { echo 'not a cross build' >&2; exit 1; }\n\
         \"${HARBOUR_HOST_CC:?HARBOUR_HOST_CC is not set}\" -o genhost gen.c\n\
         ./genhost > generated.txt\n",
    )
    .unwrap();
    fs::write(
        app_dir.join("Harbour.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         [targets.app]\nkind = \"staticlib\"\nsources = [\"src/main.c\"]\n\n\
         [[targets.app.prebuild]]\nprogram = \"sh\"\nargs = [\"gen.sh\"]\n\
         outputs = [\"generated.txt\"]\n",
    )
    .unwrap();
    fs::write(
        app_dir.join("src/main.c"),
        "int answer(void) { return 42; }\n",
    )
    .unwrap();

    harbour(&home)
        .current_dir(&app_dir)
        .args(["build", "--target-triple", target])
        .assert()
        .success();

    assert_eq!(
        fs::read_to_string(app_dir.join("generated.txt"))
            .unwrap()
            .trim(),
        "generated",
        "the host tool must have been built and run by the generator"
    );
}

/// A generator is told which of its package's features are active, so output
/// that depends on one (CPython's list of built-in modules) can be produced by
/// a single step instead of one step per feature writing the same file.
///
/// The value is the same set a `when feature = "..."` block matches, not a
/// second reading of the manifest: `default` is in it, as in Cargo, and a
/// feature a *dependent* asked for is in it too. That last case is the one
/// that matters, since `harbour build` has no `--features` of its own and a
/// package's features reach it through whoever depends on it.
#[test]
fn prebuild_generator_is_told_which_features_are_active() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let dep = tmp.path().join("dep");
    fs::create_dir_all(dep.join("src")).unwrap();
    fs::write(dep.join("src/lib.c"), "int dep_fn(void) { return 1; }\n").unwrap();
    let prebuild = write_env_dumping_generator(&dep, "");
    fs::write(
        dep.join("Harbour.toml"),
        format!(
            "[package]\nname = \"dep\"\nversion = \"0.1.0\"\n\n\
             [features]\ndefault = [\"alpha\"]\nalpha = []\nbeta = []\n\n\
             [targets.dep]\nkind = \"staticlib\"\nsources = [\"src/lib.c\"]\n\n{prebuild}"
        ),
    )
    .unwrap();

    let app = tmp.path().join("app");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(app.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();
    let app_manifest = |features: &str| {
        format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
             [dependencies]\ndep = {{ path = \"../dep\"{features} }}\n\n\
             [targets.app]\nkind = \"exe\"\nsources = [\"src/main.c\"]\n"
        )
    };
    let features_seen = |features: &str| {
        fs::write(app.join("Harbour.toml"), app_manifest(features)).unwrap();
        harbour_run(&home, &app, &["build"]).success();
        let dump = fs::read_to_string(dep.join("generated/env.txt")).unwrap();
        env_dump_get(&dump, "HARBOUR_FEATURES")
    };

    // `default` is itself an active feature, as in Cargo, and a
    // `when feature = "default"` block would match it too.
    assert_eq!(features_seen("").as_deref(), Some("alpha,default"));
    assert_eq!(
        features_seen(", features = [\"beta\"]").as_deref(),
        Some("alpha,beta,default"),
        "a feature the dependent asked for must reach the dependency's generator"
    );
    assert_eq!(
        features_seen(", default-features = false, features = [\"beta\"]").as_deref(),
        Some("beta"),
        "turning default features off must remove them from what the generator sees"
    );
}

#[test]
fn prebuild_manifest_env_overrides_harbours_own() {
    // The contract must stay overridable, exactly as it already was for a
    // `recipe`: a package that knows better -- or is working around a
    // generator that mis-parses a path -- keeps the last word.
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");
    write_env_dumping_app(
        &app_dir,
        "env = { CC = \"manifest-said-so\", HARBOUR_TARGET_ARCH = \"z80\" }\n",
    );

    build_ok(&home, &app_dir);
    let dump = fs::read_to_string(app_dir.join("generated/env.txt")).unwrap();
    assert_eq!(
        env_dump_get(&dump, "CC").as_deref(),
        Some("manifest-said-so"),
        "the manifest's `env` must win over Harbour's `CC`\n{dump}"
    );
    assert_eq!(
        env_dump_get(&dump, "HARBOUR_TARGET_ARCH").as_deref(),
        Some("z80"),
        "and over the target description too\n{dump}"
    );
}

// A probe answer reaching a generator (#135)
//
// openssl's generated config has exactly one genuine measurement of the
// target in it -- `sizeof(long)`, which decides `SIXTY_FOUR_BIT_LONG` vs
// `THIRTY_TWO_BIT` -- and before this it had to be expressed by *enumerating*
// 32-bit architectures in `when` blocks, because a generator could not see a
// probe answer. perl could have measured it, and would have measured the
// host: wrong in exactly the case that matters.
/// Lay out an `app` package whose generator writes a C source *from* a probe
/// answer, and whose `main` then checks that answer against what the
/// compiler really thinks.
///
/// The self-check is the point. A test that asserted `8` would pass on a
/// machine where the probe was wrong and the compiler agreed with it by
/// accident; comparing the generated value with `sizeof(long)` in the same
/// translation unit cannot.
fn write_probe_consuming_app(app_dir: &std::path::Path) {
    if cfg!(windows) {
        fs::write(
            app_dir.join("gen.cmd"),
            "@echo off\r\n\
             if not exist generated mkdir generated\r\n\
             >generated\\table.h echo extern int probed_sizeof_long;\r\n\
             >generated\\table.c echo int probed_sizeof_long = %HARBOUR_PROBE_SIZEOF_LONG%;\r\n\
             set > generated\\env.txt\r\n",
        )
        .unwrap();
    } else {
        fs::write(
            app_dir.join("gen.sh"),
            "#!/bin/sh\n\
             mkdir -p generated\n\
             echo 'extern int probed_sizeof_long;' > generated/table.h\n\
             echo \"int probed_sizeof_long = $HARBOUR_PROBE_SIZEOF_LONG;\" > generated/table.c\n\
             env > generated/env.txt\n",
        )
        .unwrap();
    }

    let prebuild = if cfg!(windows) {
        "[[targets.app.prebuild]]\n\
         program = \"cmd\"\n\
         args = [\"/C\", \"gen.cmd\"]\n\
         outputs = [\"generated/table.c\", \"generated/table.h\", \"generated/env.txt\"]\n"
    } else {
        "[[targets.app.prebuild]]\n\
         program = \"sh\"\n\
         args = [\"gen.sh\"]\n\
         outputs = [\"generated/table.c\", \"generated/table.h\", \"generated/env.txt\"]\n"
    };

    fs::write(
        app_dir.join("Harbour.toml"),
        format!(
            "[package]\n\
             name = \"app\"\n\
             version = \"0.1.0\"\n\
             \n\
             [targets.app]\n\
             kind = \"bin\"\n\
             sources = [\"src/**/*.c\", \"generated/*.c\"]\n\
             \n\
             [targets.app.private]\n\
             include_dirs = [\"generated\"]\n\
             \n\
             [targets.app.probes]\n\
             check_sizeof = [\"long\"]\n\
             check_headers = [\"stdio.h\"]\n\
             \n\
             [targets.app.probes.named.HAVE_HARBOUR_NO_SUCH_HEADER]\n\
             header = \"harbour_no_such_header_42.h\"\n\
             \n\
             {prebuild}"
        ),
    )
    .unwrap();

    fs::write(
        app_dir.join("src/main.c"),
        "#include <stdio.h>\n\
         #include \"table.h\"\n\
         \n\
         int main(void) {\n\
         \tprintf(\"%d %d\\n\", probed_sizeof_long, (int)sizeof(long));\n\
         \treturn probed_sizeof_long == (int)sizeof(long) ? 0 : 1;\n\
         }\n",
    )
    .unwrap();
}

#[test]
fn prebuild_generator_receives_probe_answers() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");
    write_probe_consuming_app(&app_dir);

    build_ok(&home, &app_dir);

    // The end-to-end claim: a measurement made by compiling for the target
    // reached a generator, became a translation unit, and agrees with the
    // compiler that built it.
    let run = run_built_exe(&app_dir, "app");
    let out = run.out().to_string();
    let (generated, actual) = out.split_once(' ').unwrap_or(("", ""));
    assert_eq!(
        generated, actual,
        "the generator wrote `{generated}` from HARBOUR_PROBE_SIZEOF_LONG, \
         but the compiler says sizeof(long) is `{actual}`"
    );
    assert!(
        !generated.is_empty(),
        "the generator produced no value at all; output was `{out}`"
    );

    // And the shape of the variables, read out of the generator's own
    // environment dump.
    let dump = fs::read_to_string(app_dir.join("generated/env.txt")).unwrap();
    assert_eq!(
        env_dump_get(&dump, "HARBOUR_PROBE_SIZEOF_LONG").as_deref(),
        Some(actual),
        "a `sizeof` answer arrives as the number\n{dump}"
    );
    assert_eq!(
        env_dump_get(&dump, "HARBOUR_PROBE_HAVE_STDIO_H").as_deref(),
        Some("1"),
        "a header that exists answers 1\n{dump}"
    );
    // A false answer is `0`, *not* an absent variable -- unlike the define,
    // which is left undefined so that `#ifdef` works. An environment has no
    // `#ifdef`, and an absent variable would be indistinguishable from a
    // misspelled one.
    assert_eq!(
        env_dump_get(&dump, "HARBOUR_PROBE_HAVE_HARBOUR_NO_SUCH_HEADER").as_deref(),
        Some("0"),
        "a header that does not exist must answer `0` rather than vanish: a \
         generator cannot tell an absent variable from a typo\n{dump}"
    );
}

/// Lay out an `app` whose generator appends a line to `runs.log` every time it
/// runs and copies `input.txt` to `generated/out.txt`, so a test can count
/// runs and see what the output was built from.
#[cfg(unix)]
fn write_counting_generator_app(app: &Path, step_extra: &str) {
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(app.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();
    fs::write(app.join("input.txt"), "one\n").unwrap();
    fs::write(
        app.join("gen.sh"),
        "#!/bin/sh\nset -e\necho run >> runs.log\nmkdir -p generated\ncp input.txt generated/out.txt\n",
    )
    .unwrap();
    fs::write(
        app.join("Harbour.toml"),
        format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
             [targets.app]\nkind = \"exe\"\nsources = [\"src/main.c\"]\n\n\
             [[targets.app.prebuild]]\nprogram = \"sh\"\nargs = [\"gen.sh\"]\n\
             outputs = [\"generated/out.txt\"]\n{step_extra}"
        ),
    )
    .unwrap();
}

#[cfg(unix)]
fn generator_runs(app: &Path) -> usize {
    fs::read_to_string(app.join("runs.log"))
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

/// A generator that lists its `inputs` is skipped while nothing it depends on
/// changes, and re-run when any of it does.
///
/// Each case below is a way a skip can be wrong. Skipping on an unchanged
/// *mtime* would miss an edit that restores the timestamp, so a touch must
/// not re-run it (content decides, not time). A deleted or hand-edited output
/// must be regenerated, not trusted. And the environment is part of the
/// fingerprint, so a change of toolchain or target -- which arrives as
/// `HARBOUR_*`/`CC` -- re-runs the step.
#[test]
#[cfg(unix)]
fn test_prebuild_with_inputs_is_skipped_until_something_it_depends_on_changes() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let app = tmp.path().join("app");
    write_counting_generator_app(&app, "inputs = [\"input.txt\", \"gen.sh\"]\n");

    harbour_run(&home, &app, &["build"]).success();
    assert_eq!(
        generator_runs(&app),
        1,
        "the first build must run the generator"
    );

    harbour_run(&home, &app, &["build"]).success();
    assert_eq!(
        generator_runs(&app),
        1,
        "nothing changed: the step must be skipped"
    );

    // Same content, new mtime.
    let content = fs::read(app.join("input.txt")).unwrap();
    fs::write(app.join("input.txt"), &content).unwrap();
    harbour_run(&home, &app, &["build"]).success();
    assert_eq!(
        generator_runs(&app),
        1,
        "an unchanged input must not re-run the step"
    );

    fs::write(app.join("input.txt"), "two\n").unwrap();
    harbour_run(&home, &app, &["build"]).success();
    assert_eq!(
        generator_runs(&app),
        2,
        "a changed input must re-run the step"
    );
    assert_eq!(
        fs::read_to_string(app.join("generated/out.txt")).unwrap(),
        "two\n"
    );

    fs::remove_file(app.join("generated/out.txt")).unwrap();
    harbour_run(&home, &app, &["build"]).success();
    assert_eq!(
        generator_runs(&app),
        3,
        "a missing output must be regenerated"
    );

    fs::write(app.join("generated/out.txt"), "tampered\n").unwrap();
    harbour_run(&home, &app, &["build"]).success();
    assert_eq!(
        generator_runs(&app),
        4,
        "an edited output must be regenerated"
    );
    assert_eq!(
        fs::read_to_string(app.join("generated/out.txt")).unwrap(),
        "two\n"
    );

    harbour_run(&home, &app, &["build"]).success();
    assert_eq!(generator_runs(&app), 4, "and then it settles");

    // The manifest's own `env` is part of what the step ran with.
    let manifest = fs::read_to_string(app.join("Harbour.toml")).unwrap();
    fs::write(
        app.join("Harbour.toml"),
        manifest.replace("inputs = ", "env = { EXTRA = \"1\" }\ninputs = "),
    )
    .unwrap();
    harbour_run(&home, &app, &["build"]).success();
    assert_eq!(
        generator_runs(&app),
        5,
        "a changed environment must re-run the step"
    );
}

/// A step with no `inputs` has promised nothing, so it runs on every build,
/// exactly as before.
#[test]
#[cfg(unix)]
fn test_prebuild_without_inputs_runs_on_every_build() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let app = tmp.path().join("app");
    write_counting_generator_app(&app, "");

    for expected in 1..=3 {
        harbour_run(&home, &app, &["build"]).success();
        assert_eq!(generator_runs(&app), expected);
    }
}

/// `inputs` accept directories and globs as well as files, and an entry that
/// matches nothing is refused: an `inputs` list decides whether a step may be
/// skipped, so one that silently watched nothing would let it skip while the
/// file the author meant to watch changed.
#[test]
#[cfg(unix)]
fn test_prebuild_inputs_accept_directories_and_globs_and_refuse_empty_matches() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let app = tmp.path().join("app");
    write_counting_generator_app(&app, "inputs = [\"data\", \"conf/*.cfg\"]\n");
    fs::create_dir_all(app.join("data/nested")).unwrap();
    fs::write(app.join("data/nested/deep.txt"), "a\n").unwrap();
    fs::create_dir_all(app.join("conf")).unwrap();
    fs::write(app.join("conf/x.cfg"), "a\n").unwrap();

    harbour_run(&home, &app, &["build"]).success();
    harbour_run(&home, &app, &["build"]).success();
    assert_eq!(generator_runs(&app), 1);

    fs::write(app.join("data/nested/deep.txt"), "b\n").unwrap();
    harbour_run(&home, &app, &["build"]).success();
    assert_eq!(
        generator_runs(&app),
        2,
        "a file deep inside an input directory counts"
    );

    fs::write(app.join("conf/x.cfg"), "b\n").unwrap();
    harbour_run(&home, &app, &["build"]).success();
    assert_eq!(
        generator_runs(&app),
        3,
        "a file matched by an input glob counts"
    );

    fs::write(
        app.join("Harbour.toml"),
        fs::read_to_string(app.join("Harbour.toml"))
            .unwrap()
            .replace("conf/*.cfg", "conf/*.cgf"),
    )
    .unwrap();
    let run = harbour_run(&home, &app, &["build"]);
    assert!(
        !run.status.success() && run.to_string().contains("matches no files"),
        "a typo'd `inputs` entry must be an error, not an empty watch list\n{run}"
    );
}
