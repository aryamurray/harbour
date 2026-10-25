//! An object file a generator wrote is a valid entry in `sources`.
//!
//! LuaJIT is the case that needs it: under MSVC its generator (`buildvm -m
//! peobj`) cannot emit assembly and writes `lj_vm.obj` directly, so the build has
//! to link a file it never compiled. Before this, an `.obj` in `sources` was
//! handed to the compiler as C.
#![cfg(unix)]

mod common;

use std::fs;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

/// A generator that compiles `generated/answer.c` to `generated/answer.o` with
/// the compiler Harbour gives it (`$CC`), standing in for a tool that writes an
/// object itself.
const GENERATOR: &str = "mkdir -p generated && \
    printf 'int generated_answer(void) { return 42; }\\n' > generated/answer.c && \
    \"$CC\" -c generated/answer.c -o generated/answer.o";

fn write(dir: &Path, rel: &str, text: &str) {
    let path = dir.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn prebuild() -> String {
    format!(
        "[[targets.{{T}}.prebuild]]\nprogram = \"sh\"\nargs = [\"-c\", '''{GENERATOR}''']\n\
         outputs = [\"generated/answer.o\"]\n"
    )
}

/// The object is linked into an executable.
#[test]
fn a_generated_object_is_linked_into_an_executable() {
    let tmp = TempDir::new().unwrap();
    let home = common::home(&tmp);
    let app = tmp.path().join("app");
    write(
        &app,
        "src/main.c",
        "int generated_answer(void);\nint main(void) { return generated_answer() == 42 ? 0 : 1; }\n",
    );
    write(
        &app,
        "Harbour.toml",
        &format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
             [targets.app]\nkind = \"exe\"\nsources = [\"src/main.c\", \"generated/answer.o\"]\n\n{}",
            prebuild().replace("{T}", "app")
        ),
    );

    let build = common::harbour(&home)
        .current_dir(&app)
        .arg("build")
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    let ran = Command::new(common::built_exe(&app, "app"))
        .status()
        .unwrap();
    assert!(ran.success(), "the program must call the generated object");
}

/// The object is archived into a static library and reaches a sibling
/// executable that links it.
#[test]
fn a_generated_object_is_archived_into_a_static_library() {
    let tmp = TempDir::new().unwrap();
    let home = common::home(&tmp);
    let pkg = tmp.path().join("pkg");
    write(&pkg, "src/core.c", "int core_marker(void) { return 1; }\n");
    write(
        &pkg,
        "src/app.c",
        "int generated_answer(void);\nint main(void) { return generated_answer() == 42 ? 0 : 1; }\n",
    );
    write(
        &pkg,
        "Harbour.toml",
        &format!(
            "[package]\nname = \"pkg\"\nversion = \"0.1.0\"\n\n\
             [targets.core]\nkind = \"staticlib\"\nsources = [\"src/core.c\", \"generated/answer.o\"]\n\n\
             {}\n[targets.app]\nkind = \"exe\"\nsources = [\"src/app.c\"]\n\n\
             [targets.app.deps]\ncore = {{ local = true }}\n",
            prebuild().replace("{T}", "core")
        ),
    );

    let build = common::harbour(&home)
        .current_dir(&pkg)
        .arg("build")
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    let ran = Command::new(common::built_exe(&pkg, "app"))
        .status()
        .unwrap();
    assert!(ran.success(), "the archive must carry the generated object");
}
