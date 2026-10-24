//! Manifest validation and miscellaneous build behaviour.
#![allow(unused_imports)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::prelude::*;
use predicates::prelude::*;
use tempfile::TempDir;

use harbour::test_support::fixtures::local_registry;

use crate::common::*;

/// `--message-format json` must put nothing but JSON on stdout.
///
/// Logs used to go to stdout, so INFO records landed interleaved with the
/// JSON-lines output, ANSI escapes included, and anything consuming it broke
/// on the second line. stdout is the data channel; diagnostics belong on
/// stderr.
#[test]
fn test_json_message_format_keeps_stdout_machine_readable() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");

    let out = harbour(&home)
        .args(["build", "--message-format", "json"])
        .current_dir(&app_dir)
        .output()
        .unwrap();
    assert!(out.status.success());

    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut lines = 0;
    for line in stdout.lines().filter(|l| !l.trim().is_empty()) {
        lines += 1;
        assert!(
            serde_json::from_str::<serde_json::Value>(line).is_ok(),
            "every stdout line must parse as JSON, got: {line:?}"
        );
    }
    assert!(lines > 0, "expected some JSON output, got nothing");

    // The diagnostics still have to go somewhere.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("Compiling") || stderr.contains("Finished"),
        "build progress must still be reported on stderr, got: {stderr:?}"
    );
}

/// A per-platform generated header, reached through a conditional
/// `include_dirs`, must resolve while the package is a *dependency*.
///
/// This is the configure-derived `config.h` case: a shim vendors one per
/// platform and points at the right directory from a
/// `[[targets.NAME.when]]` block. Expressing it through that block's
/// `cflags` instead (`-Iharbour-config/<platform>`) compiles here and
/// fails as a dependency, because a bare relative `-I` resolves against
/// the *root* package's working directory. The fixture therefore only ever
/// builds the library through a dependent.
#[test]
fn test_conditional_include_dirs_resolve_relative_to_their_own_package() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let lib = tmp.path().join("cfglib");
    fs::create_dir_all(lib.join("src")).unwrap();
    fs::create_dir_all(lib.join("include")).unwrap();
    // One vendored config per platform; only the matching one is on the
    // include path, so picking the wrong directory changes the answer.
    for (dir, value) in [("this-platform", 7), ("other-platform", 99)] {
        let d = lib.join("harbour-config").join(dir);
        fs::create_dir_all(&d).unwrap();
        fs::write(
            d.join("cfg_generated.h"),
            format!("#define CFG_VALUE {value}\n"),
        )
        .unwrap();
    }
    fs::write(lib.join("include/cfglib.h"), "int cfg_value(void);\n").unwrap();
    fs::write(
        lib.join("src/lib.c"),
        "#include \"cfg_generated.h\"\nint cfg_value(void) { return CFG_VALUE; }\n",
    )
    .unwrap();

    // Condition on the host's own os/arch so the block matches wherever
    // this runs, while the non-matching directory stays unreachable.
    let os = if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "linux"
    };
    fs::write(
        lib.join("Harbour.toml"),
        format!(
            r#"[package]
name = "cfglib"
version = "0.1.0"

[targets.cfglib]
kind = "staticlib"
sources = ["src/**/*.c"]

[targets.cfglib.surface.compile.public]
include_dirs = ["include"]

[[targets.cfglib.when]]
os = "{os}"
include_dirs = ["harbour-config/this-platform"]
"#
        ),
    )
    .unwrap();

    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");
    harbour(&home)
        .args(["add", "cfglib", "--path", "../cfglib"])
        .current_dir(&app_dir)
        .assert()
        .success();
    fs::write(
        app_dir.join("src/main.c"),
        "#include <stdio.h>\n#include \"cfglib.h\"\n\nint main(void) { printf(\"%d\\n\", cfg_value()); return 0; }\n",
    )
    .unwrap();

    harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .success()
        .stderr(predicate::str::contains("Finished"));

    let exe = built_exe_path(&app_dir, "app");
    let out = Command::new(&exe).output().unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "7",
        "the conditional include dir must resolve against cfglib's own root, \
         not the dependent's working directory"
    );
}

/// A source named individually that does not exist is an error; a glob that
/// matches nothing is not.
///
/// Generated manifests list sources one per line -- the harvest tool emits
/// 1082 for openssl -- and a vendored file that failed to ship would
/// otherwise vanish while the defines describing it remained. For openssl
/// that means asserting an assembly implementation exists for a primitive
/// whose object is absent. A glob has to stay permissive, because
/// `src/**/*.S` legitimately matches nothing on a platform without assembly.
#[test]
fn test_missing_named_source_is_an_error_but_empty_glob_is_not() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let dir = tmp.path().join("lib");
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join("src/present.c"),
        "int present(void) { return 1; }\n",
    )
    .unwrap();

    let manifest = |sources: &str| {
        format!(
            r#"[package]
name = "lib"
version = "0.1.0"

[targets.lib]
kind = "staticlib"
sources = {sources}
"#
        )
    };

    // A glob that matches nothing alongside one that matches: fine.
    fs::write(
        dir.join("Harbour.toml"),
        manifest(r#"["src/**/*.c", "src/**/*.S"]"#),
    )
    .unwrap();
    harbour(&home)
        .args(["build"])
        .current_dir(&dir)
        .assert()
        .success();

    // A named file that is absent: rejected, and the message names it.
    fs::write(
        dir.join("Harbour.toml"),
        manifest(r#"["src/present.c", "src/vendored_asm.S"]"#),
    )
    .unwrap();
    harbour(&home)
        .args(["build"])
        .current_dir(&dir)
        .assert()
        .failure()
        .stderr(predicate::str::contains("src/vendored_asm.S"));
}

/// `supports` warns rather than blocking, and names the package.
///
/// The list records what someone has built, not what can build: above the
/// freestanding/hosted line C guarantees nothing, so glibc, musl, MSVC and
/// newlib disagree on POSIX coverage. Rejecting an unlisted triple would mean
/// rejecting working builds as targets proliferate.
#[test]
fn test_unlisted_target_warns_but_still_builds() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let lib = tmp.path().join("declared");
    fs::create_dir_all(lib.join("src")).unwrap();
    fs::create_dir_all(lib.join("include")).unwrap();
    fs::write(lib.join("include/declared.h"), "int dv(void);\n").unwrap();
    fs::write(lib.join("src/lib.c"), "int dv(void) { return 5; }\n").unwrap();
    // A triple no CI runner uses, so the warning fires everywhere.
    fs::write(
        lib.join("Harbour.toml"),
        r#"[package]
name = "declared"
version = "0.1.0"
requires = "hosted"
supports = ["mips64-unknown-linux-gnuabi64"]

[targets.declared]
kind = "staticlib"
sources = ["src/**/*.c"]

[targets.declared.surface.compile.public]
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
        .args(["add", "declared", "--path", "../declared"])
        .current_dir(&app_dir)
        .assert()
        .success();
    fs::write(
        app_dir.join("src/main.c"),
        "#include <stdio.h>\n#include \"declared.h\"\n\nint main(void) { printf(\"%d\\n\", dv()); return 0; }\n",
    )
    .unwrap();

    // Warns, names the package, and succeeds anyway.
    harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .success()
        .stderr(predicate::str::contains("does not list"))
        .stderr(predicate::str::contains("declared"));

    let exe = built_exe_path(&app_dir, "app");
    let out = Command::new(&exe).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "5");
}

/// A `surface.when` block's private requirements must reach the compiler,
/// and an unrecognised key in one must be rejected.
///
/// `ConditionalSurface` had only `compile.public`/`link.public`, and its
/// condition fields are `#[serde(flatten)]`ed, so serde absorbed
/// `compile.private` as a condition it did not recognise: the table parsed
/// cleanly and did nothing. `harbour new` scaffolds `-Wall -Wextra` (and
/// `/W4` for MSVC) into exactly that table, so no generated project had
/// ever been compiled with warnings enabled.
#[test]
fn test_conditional_private_requirements_reach_the_compiler() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    harbour(&home)
        .args(["new", "app"])
        .current_dir(tmp.path())
        .assert()
        .success();
    let app_dir = tmp.path().join("app");

    // The scaffold's own warning flags, unmodified.
    harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .success();

    let cc = fs::read_to_string(app_dir.join(".harbour/compile_commands.json")).unwrap();
    let expected = if cfg!(target_env = "msvc") {
        "/W4"
    } else {
        "-Wall"
    };
    assert!(
        cc.contains(expected),
        "the scaffold declares {expected} in a `surface.when` block's \
         compile.private; it must reach the compiler. compile_commands.json:\n{cc}"
    );

    // A key that is neither a condition nor a requirement table is a
    // mistake, and must not be absorbed as an unknown condition.
    let manifest = app_dir.join("Harbour.toml");
    let text = fs::read_to_string(&manifest)
        .unwrap()
        .replace("compile.private", "compile.privat");
    fs::write(&manifest, text).unwrap();

    harbour(&home)
        .args(["build"])
        .current_dir(&app_dir)
        .assert()
        .failure()
        .stderr(predicate::str::contains("compile.privat"));
}

/// Declaring something Harbour cannot honour fails the build, by name, with
/// somewhere to go.
///
/// This started as three fields from the #102 sweep, each of which parsed
/// cleanly and then reached nothing. Two have since been implemented --
/// `[profile.NAME]` (#106) and `optional = true` (#108), both covered by
/// their own tests at the end of this file -- and the third,
/// `[targets.NAME.backend]`, has been **removed from the schema** rather
/// than implemented: it was a second, weaker spelling of
/// `[targets.NAME.recipe]`, which already dispatches per target and checks
/// its per-backend option keys (#107).
///
/// Checked end to end rather than only in `Manifest::parse`, because what
/// matters is that the *user running `harbour build`* is told: before this,
/// all three produced an ordinary green build.
#[test]
fn test_declared_but_unimplemented_manifest_settings_fail_the_build() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let lib = tmp.path().join("unimplib");
    fs::create_dir_all(lib.join("src")).unwrap();
    fs::write(lib.join("src/l.c"), "int l(void) { return 1; }\n").unwrap();
    fs::write(
        lib.join("Harbour.toml"),
        "[package]\n\
         name = \"unimplib\"\n\
         version = \"1.0.0\"\n\
         \n\
         [targets.unimplib]\n\
         kind = \"staticlib\"\n\
         sources = [\"src/l.c\"]\n",
    )
    .unwrap();

    let app = tmp.path().join("unimpapp");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(app.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();

    let base = "[package]\n\
                name = \"unimpapp\"\n\
                version = \"0.1.0\"\n\
                \n\
                [targets.unimpapp]\n\
                kind = \"exe\"\n\
                sources = [\"src/main.c\"]\n";

    // The table is gone from the schema, so the diagnostic has to name the
    // table, point at the spelling that does dispatch, and say where the
    // decision is recorded.
    fs::write(
        app.join("Harbour.toml"),
        format!("{base}\n[targets.unimpapp.backend]\nbackend = \"cmake\"\n"),
    )
    .unwrap();
    let run = harbour_run(&home, &app, &["build"]);
    assert!(
        !run.status.success(),
        "`[targets.X.backend]` is not in the schema, so the build must fail \
         rather than build natively and report `[native]`\n{run}"
    );
    let message = run.combined();
    assert!(
        message.contains("`[targets.unimpapp.backend]`"),
        "the diagnostic must name the table\n{run}"
    );
    assert!(
        message.contains("recipe"),
        "and point at the spelling that dispatches\n{run}"
    );
    assert!(
        message.contains("issues/107"),
        "and say where the decision is tracked\n{run}"
    );
    assert!(
        !built_exe_path_in(&app, "debug", "unimpapp").exists(),
        "nothing may have been built"
    );

    // And the same manifest without the offending fragment still builds and
    // runs, so the rejection is the only thing being tested here.
    fs::write(app.join("Harbour.toml"), base).unwrap();
    harbour_run(&home, &app, &["build"]).success();
    assert!(built_exe_path_in(&app, "debug", "unimpapp").exists());
}

/// `harbour ffi generate` for a language with no generator exits non-zero
/// and writes nothing -- and the one language that does work still does.
///
/// It used to print "not yet implemented. Contributions welcome!" and exit
/// **0**, having created no files and not even the output directory. In CI
/// the exit code is the only thing read, so that is a green binding-
/// generation step that generated no bindings. Confirmed by running before
/// the change: exit 0, `<output>` absent.
#[test]
fn test_ffi_generate_for_an_unimplemented_language_exits_non_zero() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let app = tmp.path().join("ffiapp");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::create_dir_all(app.join("include")).unwrap();
    fs::write(
        app.join("Harbour.toml"),
        r#"[package]
name = "ffiapp"
version = "0.1.0"

[targets.ffiapp]
kind = "sharedlib"
sources = ["src/lib.c"]
public_headers = ["include/**/*.h"]

[targets.ffiapp.ffi]
header_files = ["include/**/*.h"]
"#,
    )
    .unwrap();
    fs::write(
        app.join("include/ffiapp.h"),
        "#ifndef FFIAPP_H\n#define FFIAPP_H\nint ffiapp_answer(void);\n#endif\n",
    )
    .unwrap();
    fs::write(
        app.join("src/lib.c"),
        "#include \"ffiapp.h\"\nint ffiapp_answer(void) { return 42; }\n",
    )
    .unwrap();

    for lang in ["python", "csharp", "rust"] {
        let out = app.join(format!("bindings-{lang}"));
        let run = harbour_run(
            &home,
            &app,
            &[
                "ffi",
                "generate",
                "--lang",
                lang,
                "--output",
                out.to_str().unwrap(),
            ],
        );
        assert!(
            !run.status.success(),
            "`--lang {lang}` writes no files, so it must not exit 0\n{run}"
        );
        assert!(
            run.combined().contains("issues/109"),
            "and say where the work is tracked\n{run}"
        );
        assert!(
            !out.exists(),
            "nothing was generated, so `{}` must not exist either",
            out.display()
        );
    }

    // TypeScript is the one that works, and must keep working.
    let out = app.join("bindings-ts");
    harbour_run(
        &home,
        &app,
        &[
            "ffi",
            "generate",
            "--lang",
            "typescript",
            "--output",
            out.to_str().unwrap(),
        ],
    )
    .success();
    let generated: Vec<_> = fs::read_dir(&out)
        .unwrap_or_else(|e| panic!("typescript must generate into {}: {e}", out.display()))
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    assert!(
        generated.iter().any(|f| f.ends_with(".ts")),
        "a `.ts` file must be produced: {generated:?}"
    );
}

/// Two settings that are *correct* to ignore, but were ignored silently.
///
/// Neither is rejected. A dependency's `[build]` is overridden by design --
/// one graph, one C++ ABI -- and `public_headers` genuinely does something
/// (it drives the private-define ABI lint and FFI header discovery), it just
/// does not add an include directory. What was missing in both cases was
/// anyone saying so, which is the whole complaint in #102 items 7 and 8.
///
/// Warnings rather than errors, so the assertion is "the build succeeds AND
/// says this" -- a rejection here would make a package unusable as a
/// dependency for describing its own build honestly.
#[test]
fn test_settings_that_are_ignored_by_design_say_so() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    // A dependency that sets the whole ABI-relevant `[build]` table, and
    // declares public headers without a public include dir.
    let lib = tmp.path().join("quietlib");
    fs::create_dir_all(lib.join("src")).unwrap();
    fs::create_dir_all(lib.join("include")).unwrap();
    fs::write(lib.join("src/l.c"), "int l(void) { return 1; }\n").unwrap();
    fs::write(lib.join("include/l.h"), "int l(void);\n").unwrap();
    let lib_manifest = |public: &str| {
        format!(
            "[package]\n\
             name = \"quietlib\"\n\
             version = \"1.0.0\"\n\
             \n\
             [build]\n\
             cpp_std = \"20\"\n\
             exceptions = false\n\
             rtti = false\n\
             \n\
             [targets.quietlib]\n\
             kind = \"staticlib\"\n\
             sources = [\"src/l.c\"]\n\
             public_headers = [\"include/*.h\"]\n\
             {public}"
        )
    };
    fs::write(lib.join("Harbour.toml"), lib_manifest("")).unwrap();

    let app = tmp.path().join("quietapp");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(app.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();
    fs::write(
        app.join("Harbour.toml"),
        "[package]\n\
         name = \"quietapp\"\n\
         version = \"0.1.0\"\n\
         \n\
         [dependencies]\n\
         quietlib = { path = \"../quietlib\" }\n\
         \n\
         [targets.quietapp]\n\
         kind = \"exe\"\n\
         sources = [\"src/main.c\"]\n\
         \n\
         [targets.quietapp.deps]\n\
         quietlib = \"quietlib\"\n",
    )
    .unwrap();

    let run = harbour_run(&home, &app, &["build"]).success();
    let said = run.combined();

    assert!(
        said.contains("quietlib") && said.contains("`[build]`") && said.contains("ignored"),
        "a dependency's `[build]` is not read, and the dependency's author \
         cannot see this build, so it has to be reported\n{run}"
    );
    assert!(
        said.contains("exceptions = false") && said.contains("rtti = false"),
        "and it has to name which settings were dropped\n{run}"
    );
    assert!(
        said.contains("requires_cpp"),
        "for `cpp_std` specifically there *is* a spelling that travels to a \
         consumer, and the warning should name it\n{run}"
    );
    assert!(
        said.contains("public_headers") && said.contains("include_dirs"),
        "declared public headers that no consumer can find must be reported\n{run}"
    );
    assert_eq!(
        run_built_exe(&app, "quietapp").status.code(),
        Some(0),
        "and all of this is a warning, not a failure"
    );

    // Adding the include dir silences that one and only that one.
    fs::write(
        lib.join("Harbour.toml"),
        lib_manifest("\n[targets.quietlib.public]\ninclude_dirs = [\"include\"]\n"),
    )
    .unwrap();
    let run = harbour_run(&home, &app, &["build"]).success();
    assert!(
        !run.combined().contains("consumers cannot include"),
        "with a public include dir there is nothing to warn about\n{run}"
    );
    assert!(
        run.combined().contains("`[build]`"),
        "the dependency's `[build]` is still being ignored, so that one stays\n{run}"
    );
}
