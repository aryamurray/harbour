//! The spellings of a remote URL a registry or a git dependency may use.
//!
//! `git@github.com:owner/repo.git` is what GitHub's "Clone" button shows. It is
//! not a URL to the `url` crate, and a registry written that way used to be
//! skipped in silence, ending in "package not found in registries". It now
//! reaches `git` exactly as written, and a string that is no remote at all is an
//! error that says so.
//!
//! The tests that watch what reaches `ssh` replace it with a script (via
//! `GIT_SSH_COMMAND`) that records its arguments and fails, so nothing touches
//! the network; they are unix-only because the script is a shell script. The URL
//! classification itself is covered by unit tests that run everywhere.

mod common;

use assert_cmd::prelude::*;
use common::{harbour, home};
use predicates::prelude::*;
use tempfile::TempDir;

#[test]
fn a_registry_that_is_not_a_url_is_an_error_not_a_missing_package() {
    let tmp = TempDir::new().unwrap();
    let home = home(&tmp);
    harbour(&home)
        .args(["new", "addtest"])
        .current_dir(tmp.path())
        .assert()
        .success();

    harbour(&home)
        .env("HARBOUR_TEST_REGISTRY_URL", "not a url")
        .args(["add", "somepkg"])
        .current_dir(tmp.path().join("addtest"))
        .assert()
        .failure()
        .stderr(predicate::str::contains("has an invalid URL"))
        .stderr(predicate::str::contains("user@host:path"))
        .stderr(predicate::str::contains("not found in registries").not());
}

#[cfg(unix)]
mod reaches_ssh {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    use super::*;

    /// An `ssh` that records its arguments in the returned log and fails.
    fn recording_ssh(dir: &Path) -> (String, PathBuf) {
        let log = dir.join("ssh-args.log");
        let script = dir.join("fake-ssh");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{}'\nexit 255\n",
                log.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        (format!("'{}'", script.display()), log)
    }

    fn project(tmp: &TempDir, home: &Path) -> PathBuf {
        harbour(home)
            .args(["new", "addtest"])
            .current_dir(tmp.path())
            .assert()
            .success();
        tmp.path().join("addtest")
    }

    #[test]
    fn an_scp_style_registry_reaches_git_as_written() {
        let tmp = TempDir::new().unwrap();
        let home = home(&tmp);
        let project = project(&tmp, &home);
        let (ssh, log) = recording_ssh(tmp.path());

        harbour(&home)
            .env("GIT_SSH_COMMAND", ssh)
            .env("HARBOUR_TEST_REGISTRY_URL", "git@fake.example:team/reg.git")
            .args(["add", "somepkg"])
            .current_dir(&project)
            .assert()
            .failure()
            .stderr(predicate::str::contains("failed to read registry"))
            .stderr(predicate::str::contains("not found in registries").not());

        let args = fs::read_to_string(&log).expect("git never ran ssh");
        assert!(args.contains("git@fake.example"), "{args}");
        // `host:team/reg.git` is relative to the login's home: it must not turn
        // into the absolute `/team/reg.git` of the ssh:// spelling.
        assert!(args.contains("'team/reg.git'"), "{args}");
    }

    #[test]
    fn an_absolute_scp_path_stays_absolute() {
        let tmp = TempDir::new().unwrap();
        let home = home(&tmp);
        let project = project(&tmp, &home);
        let (ssh, log) = recording_ssh(tmp.path());

        harbour(&home)
            .env("GIT_SSH_COMMAND", ssh)
            .env(
                "HARBOUR_TEST_REGISTRY_URL",
                "git@fake.example:/srv/git/reg.git",
            )
            .args(["add", "somepkg"])
            .current_dir(&project)
            .assert()
            .failure();

        let args = fs::read_to_string(&log).expect("git never ran ssh");
        assert!(args.contains("'/srv/git/reg.git'"), "{args}");
    }

    #[test]
    fn an_scp_style_git_dependency_reaches_git_as_written() {
        let tmp = TempDir::new().unwrap();
        let home = home(&tmp);
        let project = project(&tmp, &home);
        let (ssh, log) = recording_ssh(tmp.path());
        let manifest = project.join("Harbour.toml");
        let mut text = fs::read_to_string(&manifest).unwrap();
        text.push_str(
            "\n[dependencies]\nlib = { git = \"git@fake.example:team/lib.git\", tag = \"v1\" }\n",
        );
        fs::write(&manifest, text).unwrap();

        harbour(&home)
            .env("GIT_SSH_COMMAND", ssh)
            .arg("build")
            .current_dir(&project)
            .assert()
            .failure()
            .stderr(predicate::str::contains("failed to clone"));

        let args = fs::read_to_string(&log).expect("git never ran ssh");
        assert!(args.contains("'team/lib.git'"), "{args}");
    }
}
