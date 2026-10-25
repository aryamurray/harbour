//! A probe answers "usable at the deployment target", not "present in the SDK".
//!
//! On Apple platforms an SDK declares APIs from OS releases newer than the
//! deployment target. They compile and link -- the compiler marks the reference
//! weak and the linker accepts it -- and then crash at run time on an OS that
//! lacks them. curl, probed on a machine whose SDK (27.0) was newer than its OS
//! (26.7), answered `HAVE_PIPE2 1`, called `pipe2` through a null weak
//! reference and segfaulted at address 0. These tests pin the fix: the probe
//! compile turns `-Wunguarded-availability-new` into an error.
//!
//! macOS only, because only Apple's SDKs annotate availability this way.
#![cfg(target_os = "macos")]

mod common;

use std::fs;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

/// An API the SDK annotates as introduced in macOS 14.4.
const HEADER: &str = "os/os_sync_wait_on_address.h";
const SYMBOL: &str = "os_sync_wait_on_address";

/// Build a one-target project that probes [`SYMBOL`] through [`HEADER`], with
/// the given deployment target, and whose `main.c` fails to compile unless the
/// probe answered `expect_present`.
fn builds_when_probe_answers(
    tmp: &TempDir,
    name: &str,
    target: &str,
    expect_present: bool,
) -> bool {
    let home = common::home(tmp);
    let app = tmp.path().join(name);
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(
        app.join("Harbour.toml"),
        format!(
            "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n\n\
             [targets.{name}]\nkind = \"exe\"\nsources = [\"src/main.c\"]\n\n\
             [targets.{name}.probes.named.HAVE_PROBED]\n\
             symbol = \"{SYMBOL}\"\nprelude = [\"{HEADER}\"]\n"
        ),
    )
    .unwrap();
    // The build is the assertion: no program needs to run, so this works on a
    // host older than the deployment target it asks about.
    let (bad, why) = if expect_present {
        (
            "#ifndef HAVE_PROBED",
            "the probe said the API is unavailable",
        )
    } else {
        ("#ifdef HAVE_PROBED", "the probe said the API is available")
    };
    fs::write(
        app.join("src/main.c"),
        format!("{bad}\n#error {why}\n#endif\nint main(void) {{ return 0; }}\n"),
    )
    .unwrap();

    common::harbour(&home)
        .current_dir(&app)
        .env("MACOSX_DEPLOYMENT_TARGET", target)
        .arg("build")
        .output()
        .unwrap()
        .status
        .success()
}

/// Whether this machine's SDK has the header at all (older SDKs do not), so the
/// test skips instead of failing for want of an API to ask about.
fn sdk_has_the_api(dir: &Path) -> bool {
    let src = dir.join("sdk_check.c");
    fs::write(
        &src,
        format!("#include <{HEADER}>\nint main(void) {{ return 0; }}\n"),
    )
    .unwrap();
    Command::new("cc")
        .args(["-fsyntax-only"])
        .arg(&src)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[test]
fn a_symbol_newer_than_the_deployment_target_probes_as_absent() {
    let tmp = tempfile::tempdir().unwrap();
    if !sdk_has_the_api(tmp.path()) {
        eprintln!("skipping: this SDK has no <{HEADER}>");
        return;
    }

    // The control: the deployment target is new enough, so the API is usable.
    // If this fails the SDK cannot target 15.0 or does not declare the API
    // as expected, and the real assertion below would prove nothing.
    if !builds_when_probe_answers(&tmp, "newenough", "15.0", true) {
        eprintln!("skipping: with a 15.0 deployment target the probe does not see {SYMBOL}");
        return;
    }

    assert!(
        builds_when_probe_answers(&tmp, "tooold", "11.0", false),
        "`{SYMBOL}` is declared by the SDK but introduced in macOS 14.4, so a program \
         built for macOS 11.0 cannot call it unconditionally; the probe must answer \
         absent. `present` here is the curl `pipe2` crash: a config header that links \
         and then jumps through a null weak reference."
    );
}
