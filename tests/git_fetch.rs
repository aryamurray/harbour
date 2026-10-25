//! The git fetch layer against a local repository served over `file://`.
//!
//! Hermetic: no network, no credentials, and it runs on Windows. Every test runs
//! against both backends (the system `git` and the libgit2 fallback), because the
//! fallback is what a machine without `git` gets and it must not drift. The tests
//! that need the `git` command skip, saying so, where it is not installed.

use std::path::Path;
use std::process::{Command, Stdio};

use git2::{IndexAddOption, Oid, Repository, Signature};
use harbour::sources::git_cli::{self, Backend};

fn git_installed() -> bool {
    Command::new("git")
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .is_ok_and(|o| o.status.success())
}

fn backends() -> Vec<Backend> {
    let mut all = vec![Backend::Libgit2];
    if git_installed() {
        all.push(Backend::Cli);
    } else {
        eprintln!("skipping the git CLI backend: git is not installed");
    }
    all
}

/// Commit `files` (path, bytes) on top of the repository's current head.
fn commit(repo: &Repository, files: &[(&str, &[u8])], message: &str) -> Oid {
    let dir = repo.workdir().unwrap().to_path_buf();
    for (path, bytes) in files {
        let full = dir.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, bytes).unwrap();
    }
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"].iter(), IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = Signature::now("Test", "test@example.invalid").unwrap();
    let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
    let parents: Vec<&git2::Commit> = parent.iter().collect();
    repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)
        .unwrap()
}

fn file_url(dir: &Path) -> String {
    url::Url::from_file_path(dir).unwrap().to_string()
}

/// An upstream with three commits; returns it, the first commit and the tip.
fn upstream(dir: &Path) -> (Repository, Oid, Oid) {
    let repo = Repository::init(dir).unwrap();
    // A checkout that honoured this would turn every LF into CRLF.
    let first = commit(
        &repo,
        &[
            (".gitattributes", b"* text eol=crlf\n"),
            ("a.txt", b"one\ntwo\n"),
        ],
        "first",
    );
    commit(&repo, &[("a.txt", b"one\ntwo\nthree\n")], "second");
    let tip = commit(&repo, &[("b.txt", b"b\n")], "third");
    (repo, first, tip)
}

#[test]
fn an_exact_clone_writes_the_committed_bytes_whatever_the_attributes_say() {
    for backend in backends() {
        let tmp = tempfile::tempdir().unwrap();
        let (_up, _, tip) = upstream(&tmp.path().join("up"));
        let dest = tmp.path().join("clone");
        let repo = git_cli::clone_repo_with(
            backend,
            &file_url(&tmp.path().join("up")),
            &dest,
            false,
            true,
        )
        .unwrap_or_else(|e| panic!("{backend:?}: {e:#}"));
        assert_eq!(repo.head().unwrap().target(), Some(tip), "{backend:?}");
        assert_eq!(
            std::fs::read(dest.join("a.txt")).unwrap(),
            b"one\ntwo\nthree\n",
            "{backend:?}: line endings must not be converted"
        );
        assert_eq!(
            repo.config().unwrap().get_bool("core.autocrlf").ok(),
            Some(false),
            "{backend:?}"
        );
    }
}

#[test]
fn a_shallow_clone_fetches_only_the_tip() {
    if !git_installed() {
        eprintln!("skipping: git is not installed");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let (_up, _, tip) = upstream(&tmp.path().join("up"));
    let dest = tmp.path().join("clone");
    let repo = git_cli::clone_repo_with(
        Backend::Cli,
        &file_url(&tmp.path().join("up")),
        &dest,
        true,
        true,
    )
    .unwrap();
    assert!(repo.is_shallow(), "history must not have been fetched");
    assert_eq!(repo.head().unwrap().target(), Some(tip));
    assert!(dest.join("b.txt").is_file(), "the tree is still written");
}

#[test]
fn fetching_head_returns_the_new_tip_and_stays_shallow() {
    for backend in backends() {
        let tmp = tempfile::tempdir().unwrap();
        let (up, _, _) = upstream(&tmp.path().join("up"));
        let url = file_url(&tmp.path().join("up"));
        let dest = tmp.path().join("clone");
        let shallow = backend == Backend::Cli;
        git_cli::clone_repo_with(backend, &url, &dest, shallow, true).unwrap();

        let newer = commit(&up, &[("c.txt", b"c\n")], "fourth");
        let tip = git_cli::fetch_head_with(backend, &dest, &url, shallow)
            .unwrap_or_else(|e| panic!("{backend:?}: {e:#}"));
        assert_eq!(tip, newer, "{backend:?}");
        assert_eq!(Repository::open(&dest).unwrap().is_shallow(), shallow);
    }
}

#[test]
fn fetching_branches_brings_in_a_new_branch() {
    for backend in backends() {
        let tmp = tempfile::tempdir().unwrap();
        let (up, _, tip) = upstream(&tmp.path().join("up"));
        let url = file_url(&tmp.path().join("up"));
        let dest = tmp.path().join("clone");
        git_cli::clone_repo_with(backend, &url, &dest, false, false).unwrap();

        up.branch("feature", &up.find_commit(tip).unwrap(), false)
            .unwrap();
        git_cli::fetch_branches_with(backend, &dest, &url)
            .unwrap_or_else(|e| panic!("{backend:?}: {e:#}"));
        let clone = Repository::open(&dest).unwrap();
        assert!(
            clone
                .find_branch("feature", git2::BranchType::Local)
                .is_ok(),
            "{backend:?}"
        );
    }
}

#[test]
fn a_pinned_commit_that_is_not_the_tip_can_be_fetched() {
    for backend in backends() {
        let tmp = tempfile::tempdir().unwrap();
        let (_up, first, _) = upstream(&tmp.path().join("up"));
        let dest = tmp.path().join("src");
        let repo = git_cli::fetch_rev_with(
            backend,
            &file_url(&tmp.path().join("up")),
            &dest,
            &first.to_string(),
        )
        .unwrap_or_else(|e| panic!("{backend:?}: {e:#}"));
        assert!(repo.find_commit(first).is_ok(), "{backend:?}");
    }
}

#[test]
fn a_pinned_commit_the_remote_lacks_is_an_error_naming_the_remote() {
    for backend in backends() {
        let tmp = tempfile::tempdir().unwrap();
        let (_up, _, _) = upstream(&tmp.path().join("up"));
        let url = file_url(&tmp.path().join("up"));
        let dest = tmp.path().join("src");
        let missing = "0123456789012345678901234567890123456789";
        let result = git_cli::fetch_rev_with(backend, &url, &dest, missing)
            .map(|repo| repo.find_commit(Oid::from_str(missing).unwrap()).is_ok());
        assert!(
            !result.unwrap_or(false),
            "{backend:?}: a commit the remote lacks must not appear"
        );
    }
}

#[test]
fn a_pinned_commit_must_be_a_commit_id() {
    // Anything else would be handed to `git fetch` as an argument.
    let tmp = tempfile::tempdir().unwrap();
    let err = git_cli::fetch_rev_with(
        Backend::Libgit2,
        "file:///nowhere",
        &tmp.path().join("src"),
        "--upload-pack=touch x",
    )
    .map(|_| ())
    .unwrap_err();
    assert!(format!("{err:#}").contains("invalid commit id"), "{err:#}");
}

#[test]
fn a_clone_of_a_missing_remote_says_so_and_names_it() {
    for backend in backends() {
        let tmp = tempfile::tempdir().unwrap();
        let url = file_url(&tmp.path().join("does-not-exist"));
        let err = git_cli::clone_repo_with(backend, &url, &tmp.path().join("c"), false, true)
            .map(|_| ())
            .unwrap_err();
        let text = format!("{err:#}");
        assert!(text.starts_with("failed to clone"), "{backend:?}: {text}");
        assert!(text.contains("does-not-exist"), "{backend:?}: {text}");
    }
}

#[test]
fn the_default_backend_is_the_git_command_when_it_is_installed() {
    if !git_installed() {
        eprintln!("skipping: git is not installed");
        return;
    }
    // The workflow this change exists for, a user's own git setup, only works if
    // the real command is what runs.
    assert_eq!(git_cli::backend(), Backend::Cli);
}
