//! Helpers shared by the integration tests that live in their own files.
//!
//! `tests/cli_integration.rs` is one 13,000-line crate whose end every new
//! test was appended to, so each PR collided with the next. New integration
//! tests go in their own file and share what they need from here (the usual
//! Cargo pattern: a `common/mod.rs` is not itself a test crate). These are the
//! few helpers a test of the binary needs, kept deliberately small; the large
//! `RunLog` machinery stays in `cli_integration.rs`.
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

/// An isolated `HOME` inside the test's own temporary directory, so nothing
/// reads or writes the developer's real `~/.harbour` or git configuration.
pub fn home(tmp: &TempDir) -> PathBuf {
    let home = tmp.path().join(".harbour-home");
    fs::create_dir_all(&home).unwrap();
    home
}

/// [`home`], with a global git configuration that converts line endings the way
/// Git for Windows does by default (`core.autocrlf = true`).
///
/// libgit2 and `git` both read `$HOME/.gitconfig`, so this reproduces that
/// default on any operating system: a checkout of a file without `-text`
/// comes out with CRLF line endings.
pub fn home_that_converts_line_endings(tmp: &TempDir) -> PathBuf {
    let home = home(tmp);
    fs::write(home.join(".gitconfig"), "[core]\n\tautocrlf = true\n").unwrap();
    home
}

/// A `harbour` command that cannot see the real home, cache or any ambient
/// vcpkg or registry configuration.
pub fn harbour(home: &Path) -> Command {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("harbour"));
    cmd.env("HOME", home)
        .env("XDG_CACHE_HOME", home.join("xdg-cache"))
        .env("XDG_CONFIG_HOME", home.join("xdg-config"))
        .env("XDG_DATA_HOME", home.join("xdg-data"))
        .env("APPDATA", home.join("AppData/Roaming"))
        .env("LOCALAPPDATA", home.join("AppData/Local"))
        .env_remove("VCPKG_ROOT")
        .env_remove("HARBOUR_TEST_REGISTRY_URL");
    cmd
}

/// What a run printed and whether it succeeded.
pub struct Run {
    pub success: bool,
    pub text: String,
}

/// Run `harbour <args>` in `dir`, against the registry at `registry_url`.
pub fn run_in_registry(home: &Path, dir: &Path, args: &[&str], registry_url: &str) -> Run {
    let out = harbour(home)
        .current_dir(dir)
        .args(args)
        .env("HARBOUR_TEST_REGISTRY_URL", registry_url)
        .output()
        .unwrap();
    Run {
        success: out.status.success(),
        text: format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    }
}

/// The program a build of `app` wrote.
pub fn built_exe(app: &Path, name: &str) -> PathBuf {
    let exe = app
        .join(".harbour/target/debug/bin")
        .join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    assert!(
        exe.exists(),
        "expected a built executable at {}",
        exe.display()
    );
    exe
}

/// The stdout of a built program, trimmed.
pub fn run_exe(app: &Path, name: &str) -> String {
    let out = Command::new(built_exe(app, name)).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Write an application that depends on `deps_toml`, includes `headers`, and
/// prints the `int` expression `call`.
pub fn write_app(app: &Path, deps_toml: &str, headers: &[&str], call: &str) {
    fs::create_dir_all(app.join("src")).unwrap();
    let includes: String = headers
        .iter()
        .map(|h| format!("#include \"{h}\"\n"))
        .collect();
    fs::write(
        app.join("src/main.c"),
        format!(
            "#include <stdio.h>\n{includes}int main(void) {{ printf(\"%d\\n\", {call}); return 0; }}\n"
        ),
    )
    .unwrap();
    fs::write(
        app.join("Harbour.toml"),
        format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\n{deps_toml}\n\n\
             [targets.app]\nkind = \"exe\"\nsources = [\"src/main.c\"]\n"
        ),
    )
    .unwrap();
}

/// A unified diff that adds a one-library `Harbour.toml` for `name`.
pub fn manifest_patch_text(name: &str, version: &str) -> String {
    let manifest = format!(
        "[package]\nname = \"{name}\"\nversion = \"{version}\"\n\n\
         [targets.{name}]\nkind = \"staticlib\"\nsources = [\"{name}.c\"]\n\n\
         [targets.{name}.surface.compile.public]\ninclude_dirs = [\".\"]\n"
    );
    let added: String = manifest.lines().map(|l| format!("+{l}\n")).collect();
    format!(
        "diff --git a/Harbour.toml b/Harbour.toml\nnew file mode 100644\n--- /dev/null\n\
         +++ b/Harbour.toml\n@@ -0,0 +1,{} @@\n{added}",
        manifest.lines().count()
    )
}

/// Write `text` as `index/<l>/<name>/<version>.patch` in `registry` and return
/// the file name and the sha256 of exactly those bytes.
pub fn write_patch(registry: &Path, name: &str, version: &str, text: &str) -> (String, String) {
    let dir = registry
        .join("index")
        .join(name.chars().next().unwrap().to_string())
        .join(name);
    fs::create_dir_all(&dir).unwrap();
    let file = format!("{version}.patch");
    fs::write(dir.join(&file), text).unwrap();
    (file, harbour::util::hash::sha256_str(text))
}

/// Every regular file under `dir`, recursively.
pub fn walk_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                out.extend(walk_files(&path));
            } else {
                out.push(path);
            }
        }
    }
    out
}
