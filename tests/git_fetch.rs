//! The git fetch layer against a local repository served over `file://`.
//!
//! Hermetic: no network, no credentials, and it runs on Windows. `git` is a hard
//! requirement of Harbour, so these tests need it too and fail loudly without it.
//! Repositories are built and inspected with the `git` command through the same
//! hermetic fixture helper as every other test (`local_registry::git`).

use std::path::Path;

use harbour::sources::git_cli;
use harbour::test_support::fixtures::local_registry::git;

/// Commit `files` (path, bytes) on top of the repository's current head and
/// return the new commit id.
fn commit(dir: &Path, files: &[(&str, &[u8])], message: &str) -> String {
    for (path, bytes) in files {
        let full = dir.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, bytes).unwrap();
    }
    git(dir, &["add", "-A"]).unwrap();
    git(dir, &["commit", "--quiet", "-m", message]).unwrap();
    git(dir, &["rev-parse", "HEAD"]).unwrap()
}

fn file_url(dir: &Path) -> String {
    url::Url::from_file_path(dir).unwrap().to_string()
}

/// An upstream with three commits; returns its first commit and its tip.
fn upstream(dir: &Path) -> (String, String) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["-c", "init.defaultBranch=master", "init", "--quiet"]).unwrap();
    // A checkout that honoured this would turn every LF into CRLF.
    let first = commit(
        dir,
        &[
            (".gitattributes", b"* text eol=crlf\n"),
            ("a.txt", b"one\ntwo\n"),
        ],
        "first",
    );
    commit(dir, &[("a.txt", b"one\ntwo\nthree\n")], "second");
    let tip = commit(dir, &[("b.txt", b"b\n")], "third");
    (first, tip)
}

/// Define the `shout` filter in the repository at `dir` as one that always fails.
fn failing_filter(dir: &Path) {
    git(dir, &["config", "filter.shout.smudge", "exit 1"]).unwrap();
    git(dir, &["config", "filter.shout.required", "true"]).unwrap();
}

fn is_shallow(dir: &Path) -> bool {
    git(dir, &["rev-parse", "--is-shallow-repository"]).unwrap() == "true"
}

#[test]
fn an_exact_clone_writes_the_committed_bytes_whatever_the_attributes_say() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, tip) = upstream(&tmp.path().join("up"));
    let dest = tmp.path().join("clone");
    let head = git_cli::clone_repo(&file_url(&tmp.path().join("up")), &dest, false, true)
        .unwrap_or_else(|e| panic!("{e:#}"));
    assert_eq!(head, tip);
    assert_eq!(
        std::fs::read(dest.join("a.txt")).unwrap(),
        b"one\ntwo\nthree\n",
        "line endings must not be converted"
    );
    assert_eq!(
        git(&dest, &["config", "--bool", "core.autocrlf"]).unwrap(),
        "false"
    );
}

/// What a tree can ask git to do to a file on the way to disk: line
/// endings, `$Id$` expansion, a smudge filter. A registry or an
/// upstream controls its own tree, so none of it may apply to an exact checkout,
/// on a fresh clone or on any later update of it.
#[test]
fn an_exact_checkout_ignores_every_conversion_the_tree_asks_for() {
    let tmp = tempfile::tempdir().unwrap();
    let up = tmp.path().join("up");
    std::fs::create_dir_all(&up).unwrap();
    git(&up, &["-c", "init.defaultBranch=master", "init", "--quiet"]).unwrap();
    // `filter=shout` names a driver the clones below define as required and
    // failing, standing for one the user has installed (git-lfs, say): a tree
    // can name any of them, and a checkout that ran it would fail.
    let hostile = b"* text eol=crlf ident filter=shout\n\
                    *.bin binary\n";
    commit(
        &up,
        &[
            (".gitattributes", hostile),
            ("a.txt", b"one\ntwo\n"),
            ("id.txt", b"$Id$\n"),
            ("sub/deep.txt", b"x\ny\n"),
        ],
        "hostile",
    );

    let url = file_url(&up);
    let dest = tmp.path().join("clone");
    git_cli::clone_repo(&url, &dest, true, true).unwrap_or_else(|e| panic!("{e:#}"));
    let check = |dest: &Path, expect_a: &[u8]| {
        assert_eq!(std::fs::read(dest.join("a.txt")).unwrap(), expect_a);
        assert_eq!(std::fs::read(dest.join("id.txt")).unwrap(), b"$Id$\n");
        assert_eq!(std::fs::read(dest.join("sub/deep.txt")).unwrap(), b"x\ny\n");
    };
    check(&dest, b"one\ntwo\n");

    // The update path: fetch the new tip, then check it out exactly.
    commit(&up, &[("a.txt", b"one\ntwo\nthree\n")], "next");
    let tip = git_cli::fetch_head(&dest, &url, true).unwrap();
    failing_filter(&dest);
    git_cli::checkout_exact(&dest, &tip).unwrap();
    check(&dest, b"one\ntwo\nthree\n");

    // The pinned-commit path.
    let pinned = tmp.path().join("pinned");
    git_cli::fetch_rev(&url, &pinned, &tip).unwrap();
    failing_filter(&pinned);
    git_cli::checkout_exact(&pinned, &tip).unwrap();
    check(&pinned, b"one\ntwo\nthree\n");
}

#[test]
fn a_shallow_clone_fetches_only_the_tip() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, tip) = upstream(&tmp.path().join("up"));
    let dest = tmp.path().join("clone");
    let head = git_cli::clone_repo(&file_url(&tmp.path().join("up")), &dest, true, true).unwrap();
    assert!(is_shallow(&dest), "history must not have been fetched");
    assert_eq!(head, tip);
    assert!(dest.join("b.txt").is_file(), "the tree is still written");
}

#[test]
fn fetching_head_returns_the_new_tip_and_stays_shallow() {
    let tmp = tempfile::tempdir().unwrap();
    let up = tmp.path().join("up");
    upstream(&up);
    let url = file_url(&up);
    let dest = tmp.path().join("clone");
    git_cli::clone_repo(&url, &dest, true, true).unwrap();

    let newer = commit(&up, &[("c.txt", b"c\n")], "fourth");
    let tip = git_cli::fetch_head(&dest, &url, true).unwrap_or_else(|e| panic!("{e:#}"));
    assert_eq!(tip, newer);
    assert!(is_shallow(&dest));
}

#[test]
fn fetching_branches_brings_in_a_new_branch() {
    let tmp = tempfile::tempdir().unwrap();
    let up = tmp.path().join("up");
    upstream(&up);
    let url = file_url(&up);
    let dest = tmp.path().join("clone");
    git_cli::clone_repo(&url, &dest, false, false).unwrap();

    git(&up, &["branch", "feature"]).unwrap();
    git_cli::fetch_branches(&dest, &url).unwrap_or_else(|e| panic!("{e:#}"));
    assert!(
        git_cli::resolve_commit(&dest, "refs/heads/feature").is_ok(),
        "the new branch must be a local branch of the clone"
    );
}

#[test]
fn a_pinned_commit_that_is_not_the_tip_can_be_fetched() {
    let tmp = tempfile::tempdir().unwrap();
    let (first, _) = upstream(&tmp.path().join("up"));
    let dest = tmp.path().join("src");
    git_cli::fetch_rev(&file_url(&tmp.path().join("up")), &dest, &first)
        .unwrap_or_else(|e| panic!("{e:#}"));
    assert!(git_cli::has_commit(&dest, &first));
}

#[test]
fn a_pinned_commit_the_remote_lacks_is_an_error_naming_the_remote() {
    let tmp = tempfile::tempdir().unwrap();
    upstream(&tmp.path().join("up"));
    let url = file_url(&tmp.path().join("up"));
    let missing = "0123456789012345678901234567890123456789";
    let err = git_cli::fetch_rev(&url, &tmp.path().join("src"), missing).unwrap_err();
    let text = format!("{err:#}");
    assert!(text.contains(&url), "{text}");
    assert!(text.contains(missing), "{text}");
}

#[test]
fn a_pinned_commit_must_be_a_commit_id() {
    // Anything else would be handed to `git fetch` as an argument.
    let tmp = tempfile::tempdir().unwrap();
    let err = git_cli::fetch_rev(
        "file:///nowhere",
        &tmp.path().join("src"),
        "--upload-pack=touch x",
    )
    .unwrap_err();
    assert!(format!("{err:#}").contains("invalid commit id"), "{err:#}");
    // Abbreviations are not pins either.
    let err = git_cli::fetch_rev("file:///nowhere", &tmp.path().join("src"), "abc123").unwrap_err();
    assert!(format!("{err:#}").contains("invalid commit id"), "{err:#}");
}

#[test]
fn a_clone_of_a_missing_remote_says_so_and_names_it() {
    let tmp = tempfile::tempdir().unwrap();
    let url = file_url(&tmp.path().join("does-not-exist"));
    let err = git_cli::clone_repo(&url, &tmp.path().join("c"), false, true).unwrap_err();
    let text = format!("{err:#}");
    assert!(text.starts_with("failed to clone"), "{text}");
    assert!(text.contains("does-not-exist"), "{text}");
}

#[test]
fn a_remote_and_a_cache_path_with_spaces_work() {
    let tmp = tempfile::tempdir().unwrap();
    let up = tmp.path().join("an upstream");
    let (_, tip) = upstream(&up);
    let dest = tmp.path().join("a cache").join("clone dir");
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    let head =
        git_cli::clone_repo(&file_url(&up), &dest, false, true).unwrap_or_else(|e| panic!("{e:#}"));
    assert_eq!(head, tip);
    assert!(dest.join("b.txt").is_file());
}

#[test]
fn head_commit_is_none_outside_a_repository_even_inside_another() {
    let tmp = tempfile::tempdir().unwrap();
    let outer = tmp.path().join("outer");
    upstream(&outer);
    let plain = outer.join("not-a-repo");
    std::fs::create_dir_all(&plain).unwrap();
    assert!(git_cli::head_commit(&outer).is_some());
    assert_eq!(
        git_cli::head_commit(&plain),
        None,
        "a directory that is not a repository must not read as its parent"
    );
}
