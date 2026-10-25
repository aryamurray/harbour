//! A failed compile, link or archive must show what the tool said, whichever
//! stream the tool said it on.
//!
//! GNU and LLVM tools write diagnostics to stderr; MSVC's `cl.exe`, `link.exe`
//! and `lib.exe` write theirs to **stdout**. Harbour read only stderr, so every
//! MSVC failure printed `compilation failed for foo.c` and then nothing -- no
//! `C2065`, no `LNK1181` -- which made a first Windows build impossible to
//! debug. These tests stand a script in for the compiler that talks on stdout,
//! as `cl` does, so the behaviour is pinned on every operating system.
#![cfg(unix)]

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use tempfile::TempDir;

/// A compiler that reports on STDOUT only, as `cl.exe` does, and exits 1. With
/// `fail_on` = "compile" it fails every `-c`; with "link" it compiles normally
/// and fails the link.
fn talkative_cc(dir: &Path, fail_on: &str) -> std::path::PathBuf {
    let shim = dir.join("stdout-cc");
    let body = match fail_on {
        "compile" => "case \" $* \" in *\" -c \"*) echo 'lib.c(1): error C2065: simulated on stdout'; exit 1;; esac\nexec cc \"$@\"\n",
        _ => "case \" $* \" in *\" -c \"*) exec cc \"$@\";; esac\necho 'LINK : fatal error LNK1181: simulated on stdout'; exit 1\n",
    };
    fs::write(&shim, format!("#!/bin/sh\n{body}")).unwrap();
    let mut perms = fs::metadata(&shim).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&shim, perms).unwrap();
    shim
}

fn app(tmp: &TempDir) -> std::path::PathBuf {
    let app = tmp.path().join("app");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(app.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();
    fs::write(
        app.join("Harbour.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         [targets.app]\nkind = \"exe\"\nsources = [\"src/main.c\"]\n",
    )
    .unwrap();
    app
}

fn build_failing_with(fail_on: &str) -> String {
    let tmp = TempDir::new().unwrap();
    let home = common::home(&tmp);
    let app = app(&tmp);
    let cc = talkative_cc(tmp.path(), fail_on);
    let out = common::harbour(&home)
        .current_dir(&app)
        .arg("build")
        .env("CC", &cc)
        .output()
        .unwrap();
    assert!(!out.status.success(), "the simulated tool fails");
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn a_compile_error_the_tool_wrote_to_stdout_is_shown() {
    let text = build_failing_with("compile");
    assert!(
        text.contains("compilation failed") && text.contains("C2065: simulated on stdout"),
        "the compiler's own diagnostic must reach the user\n{text}"
    );
}

#[test]
fn a_link_error_the_tool_wrote_to_stdout_is_shown() {
    let text = build_failing_with("link");
    assert!(
        text.contains("linking failed") && text.contains("LNK1181: simulated on stdout"),
        "the linker's own diagnostic must reach the user\n{text}"
    );
}
