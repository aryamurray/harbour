//! Fetching from git remotes with the system `git`.
//!
//! libgit2 reads none of the configuration that makes a remote reachable for a
//! person: credential helpers, `gh auth setup-git`, the keychain, `~/.ssh/config`
//! aliases and `ProxyJump`, `GIT_SSH_COMMAND`, `includeIf`, `url.<base>.insteadOf`,
//! hardware keys. The `git` command reads all of it, so every network operation
//! (clone, fetch) runs `git` and nothing is reimplemented. libgit2 stays for the
//! local work it does well: opening the repository and writing the working tree
//! after the fetch. (Cargo's `net.git-fetch-with-cli` exists for the same reason.)
//!
//! `git` runs with the user's own configuration untouched. Only two things are
//! set: `GIT_TERMINAL_PROMPT=0`, so a missing login is an error rather than a
//! hang, and the variables a surrounding `git` hook exports (`GIT_DIR` and
//! friends) are removed so they cannot redirect the fetch into the wrong
//! repository. A command that runs longer than [`DEFAULT_TIMEOUT`] is killed
//! (`HARBOUR_GIT_TIMEOUT=<seconds>` changes it, `0` disables it).
//!
//! When `git` is not installed Harbour falls back to libgit2, warns once that
//! private remotes need `git`, and public remotes keep working.

use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use git2::build::{CheckoutBuilder, RepoBuilder};
use git2::{Oid, Repository, ResetType};

use crate::util::redact::{redact_text, redact_url, warn_if_embedded_login};
use crate::util::remote_url::git_arg;

/// How long one `git` command may run.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Variables a `git` hook sets that would point our `git` at the wrong repository.
const HOOK_VARIABLES: [&str; 6] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_PREFIX",
];

/// What carries out a network operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// The system `git` command.
    Cli,
    /// libgit2, which cannot use the user's credentials.
    Libgit2,
}

/// The backend to use: `git` when it is installed. `HARBOUR_GIT_BACKEND=libgit2`
/// forces the fallback.
pub fn backend() -> Backend {
    if std::env::var("HARBOUR_GIT_BACKEND").is_ok_and(|v| v.eq_ignore_ascii_case("libgit2")) {
        return Backend::Libgit2;
    }
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    let available = *AVAILABLE.get_or_init(|| {
        Command::new("git")
            .arg("--version")
            .stdin(Stdio::null())
            .output()
            .is_ok_and(|o| o.status.success())
    });
    if available {
        Backend::Cli
    } else {
        static WARNED: OnceLock<()> = OnceLock::new();
        WARNED.get_or_init(|| {
            tracing::warn!(
                "git was not found on PATH; fetching with the built-in libgit2, which cannot use your git credentials. Private remotes need git installed"
            );
        });
        Backend::Libgit2
    }
}

/// Checkout options that write exactly the committed bytes.
///
/// A patch is verified by the sha256 of its bytes and applied with `git apply`
/// against the upstream tree, so neither the registry nor an upstream source may
/// have its line endings converted on the way to disk. libgit2 honours the
/// user's `core.autocrlf` (Git for Windows sets it to `true` by default), which
/// turns every LF in a checkout into CRLF: the registry's patches then fail
/// their hash, and an upstream text file no longer matches a patch's context
/// lines. Filters are disabled for the checkout instead of relying on each
/// registry's `.gitattributes`, which protects patches but not upstream trees.
pub fn exact_checkout() -> CheckoutBuilder<'static> {
    let mut checkout = CheckoutBuilder::new();
    checkout.disable_filters(true).force();
    checkout
}

// ---------------------------------------------------------------------------
// Operations
// ---------------------------------------------------------------------------

/// Clone `url` into `dest`.
///
/// `shallow` fetches only the tip commit. `exact` writes exactly the committed
/// bytes (see [`exact_checkout`]) and records `core.autocrlf = false` in the new
/// repository so later operations on it, `git apply` included, agree.
pub fn clone_repo(url: &str, dest: &Path, shallow: bool, exact: bool) -> Result<Repository> {
    clone_repo_with(backend(), url, dest, shallow, exact)
}

pub fn clone_repo_with(
    backend: Backend,
    url: &str,
    dest: &Path,
    shallow: bool,
    exact: bool,
) -> Result<Repository> {
    warn_if_embedded_login(url);
    // `git` gets the URL as the user wrote it (see `remote_url`).
    let remote = git_arg(url);
    let repo = match backend {
        Backend::Cli => {
            run_git(
                None,
                &clone_args(&remote, dest, shallow, exact),
                url,
                "failed to clone",
            )?;
            Repository::open(dest).context("failed to open the cloned repository")?
        }
        Backend::Libgit2 => {
            let mut builder = RepoBuilder::new();
            if exact {
                builder.with_checkout(exact_checkout());
            }
            builder
                .clone(&remote, dest)
                .map_err(|e| failure("failed to clone", url, &e.to_string()))?
        }
    };
    if exact {
        if backend == Backend::Cli {
            // The clone wrote nothing (`--no-checkout`): write the tree ourselves.
            let head = repo.head()?.peel_to_commit()?;
            repo.reset(
                head.as_object(),
                ResetType::Hard,
                Some(&mut exact_checkout()),
            )?;
        }
        if let Ok(mut config) = repo.config() {
            let _ = config.set_bool("core.autocrlf", false);
        }
    }
    Ok(repo)
}

/// Bring every branch (and tag) of `origin` up to date in the clone at `repo_dir`.
pub fn fetch_branches(repo_dir: &Path, url: &str) -> Result<()> {
    fetch_branches_with(backend(), repo_dir, url)
}

pub fn fetch_branches_with(backend: Backend, repo_dir: &Path, url: &str) -> Result<()> {
    match backend {
        Backend::Cli => {
            run_git(
                Some(repo_dir),
                &fetch_branches_args(),
                url,
                "failed to fetch",
            )?;
        }
        Backend::Libgit2 => {
            let repo = Repository::open(repo_dir).context("failed to open git repository")?;
            repo.find_remote("origin")?
                .fetch(&["refs/heads/*:refs/heads/*"], None, None)
                .map_err(|e| failure("failed to fetch", url, &e.to_string()))?;
        }
    }
    Ok(())
}

/// Fetch the remote's current `HEAD` into the clone at `repo_dir` and return its
/// commit. `shallow` fetches only that commit.
pub fn fetch_head(repo_dir: &Path, url: &str, shallow: bool) -> Result<Oid> {
    fetch_head_with(backend(), repo_dir, url, shallow)
}

pub fn fetch_head_with(backend: Backend, repo_dir: &Path, url: &str, shallow: bool) -> Result<Oid> {
    let repo = Repository::open(repo_dir).context("failed to open the repository")?;
    match backend {
        Backend::Cli => {
            run_git(
                Some(repo_dir),
                &fetch_head_args(shallow),
                url,
                "failed to fetch",
            )?;
            Ok(repo.revparse_single("FETCH_HEAD")?.peel_to_commit()?.id())
        }
        Backend::Libgit2 => {
            repo.find_remote("origin")?
                .fetch(&["refs/heads/*:refs/heads/*"], None, None)
                .map_err(|e| failure("failed to fetch", url, &e.to_string()))?;
            Ok(repo.head()?.peel_to_commit()?.id())
        }
    }
}

/// Create a repository at `dest` holding commit `rev` of `url` (not checked out).
///
/// Asks the server for just that commit first, which is a few kilobytes for a
/// large project; a server that will not serve an arbitrary commit that way gets
/// a full fetch instead.
pub fn fetch_rev(url: &str, dest: &Path, rev: &str) -> Result<Repository> {
    fetch_rev_with(backend(), url, dest, rev)
}

pub fn fetch_rev_with(backend: Backend, url: &str, dest: &Path, rev: &str) -> Result<Repository> {
    anyhow::ensure!(
        !rev.is_empty() && rev.chars().all(|c| c.is_ascii_hexdigit()),
        "invalid commit id: {rev}"
    );
    warn_if_embedded_login(url);
    let repo = Repository::init(dest).context("failed to create the repository")?;
    repo.remote("origin", &git_arg(url))?;
    match backend {
        Backend::Cli => {
            if let Err(e) = run_git(Some(dest), &fetch_rev_args(rev), url, "failed to fetch") {
                tracing::debug!("fetching {rev} alone failed ({e:#}); fetching everything instead");
                run_git(Some(dest), &fetch_all_args(), url, "failed to fetch")?;
            }
        }
        Backend::Libgit2 => {
            repo.find_remote("origin")?
                .fetch(
                    &[
                        "+refs/heads/*:refs/remotes/origin/*",
                        "+refs/tags/*:refs/tags/*",
                    ],
                    None,
                    None,
                )
                .map_err(|e| failure("failed to fetch", url, &e.to_string()))?;
        }
    }
    Ok(repo)
}

// ---------------------------------------------------------------------------
// Command construction (pure, so it can be tested without a server)
// ---------------------------------------------------------------------------

fn os(s: &str) -> OsString {
    OsString::from(s)
}

pub(crate) fn clone_args(url: &str, dest: &Path, shallow: bool, exact: bool) -> Vec<OsString> {
    let mut args = vec![os("clone"), os("--quiet")];
    if shallow {
        args.extend([os("--depth"), os("1")]);
    }
    if exact {
        args.push(os("--no-checkout"));
    }
    args.extend([os("--"), os(url), dest.as_os_str().to_os_string()]);
    args
}

pub(crate) fn fetch_branches_args() -> Vec<OsString> {
    [
        "fetch",
        "--quiet",
        "--tags",
        "--update-head-ok",
        "origin",
        "+refs/heads/*:refs/heads/*",
    ]
    .iter()
    .map(|s| os(s))
    .collect()
}

pub(crate) fn fetch_head_args(shallow: bool) -> Vec<OsString> {
    let mut args = vec![os("fetch"), os("--quiet")];
    if shallow {
        args.extend([os("--depth"), os("1")]);
    }
    args.extend([os("origin"), os("HEAD")]);
    args
}

pub(crate) fn fetch_rev_args(rev: &str) -> Vec<OsString> {
    vec![
        os("fetch"),
        os("--quiet"),
        os("--depth"),
        os("1"),
        os("origin"),
        os(rev),
    ]
}

pub(crate) fn fetch_all_args() -> Vec<OsString> {
    [
        "fetch",
        "--quiet",
        "--tags",
        "origin",
        "+refs/heads/*:refs/remotes/origin/*",
    ]
    .iter()
    .map(|s| os(s))
    .collect()
}

/// The timeout for one command, from `HARBOUR_GIT_TIMEOUT` (seconds; `0` = none).
fn timeout() -> Option<Duration> {
    parse_timeout(std::env::var("HARBOUR_GIT_TIMEOUT").ok().as_deref())
}

fn parse_timeout(value: Option<&str>) -> Option<Duration> {
    match value.and_then(|v| v.trim().parse::<u64>().ok()) {
        Some(0) => None,
        Some(secs) => Some(Duration::from_secs(secs)),
        None => Some(DEFAULT_TIMEOUT),
    }
}

/// A `git` command with the environment described in the module docs.
fn command(cwd: Option<&Path>, args: &[OsString]) -> Command {
    let mut cmd = Command::new("git");
    cmd.args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for var in HOOK_VARIABLES {
        cmd.env_remove(var);
    }
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    cmd
}

/// Run `git args` and turn a failure into an error about `url`.
fn run_git(cwd: Option<&Path>, args: &[OsString], url: &str, what: &str) -> Result<()> {
    tracing::debug!(
        "git {}",
        args.iter()
            .map(|a| redact_text(&a.to_string_lossy()))
            .collect::<Vec<_>>()
            .join(" ")
    );
    let mut child = command(cwd, args)
        .spawn()
        .map_err(|e| failure(what, url, &format!("could not run git: {e}")))?;

    // Drain both pipes on threads so a chatty git cannot block on a full pipe.
    let drain = |pipe: Option<Box<dyn std::io::Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = String::new();
            if let Some(mut p) = pipe {
                let mut bytes = Vec::new();
                let _ = p.read_to_end(&mut bytes);
                buf = String::from_utf8_lossy(&bytes).into_owned();
            }
            buf
        })
    };
    let stderr = drain(child.stderr.take().map(|p| Box::new(p) as _));
    let stdout = drain(child.stdout.take().map(|p| Box::new(p) as _));

    let deadline = timeout().map(|t| (Instant::now() + t, t));
    let status = loop {
        match child.try_wait()? {
            Some(status) => break Some(status),
            None => {
                if let Some((at, _)) = deadline {
                    if Instant::now() >= at {
                        let _ = child.kill();
                        let _ = child.wait();
                        break None;
                    }
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    };
    let stderr = stderr.join().unwrap_or_default();
    let _ = stdout.join();

    match status {
        Some(status) if status.success() => Ok(()),
        Some(status) => {
            let detail = stderr.trim();
            let code = status
                .code()
                .map_or_else(|| "was killed".to_string(), |c| format!("exited with {c}"));
            Err(failure(
                what,
                url,
                &if detail.is_empty() {
                    format!("git {code}")
                } else {
                    detail.to_string()
                },
            ))
        }
        None => {
            let secs = deadline.map_or(0, |(_, t)| t.as_secs());
            Err(failure(
                what,
                url,
                &format!(
                    "git did not finish within {secs}s and was stopped (set HARBOUR_GIT_TIMEOUT=<seconds> to change this); {}",
                    stderr.trim()
                ),
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// `<what> <url>: <detail>` with logins removed, plus a `hint:` when the failure
/// is one the user can fix.
pub(crate) fn failure(what: &str, url: &str, detail: &str) -> anyhow::Error {
    let detail = redact_text(detail.trim());
    let mut msg = format!("{what} {}: {detail}", redact_url(url));
    if let Some(hint) = hint_for(&detail, url) {
        msg.push_str("\n  hint: ");
        msg.push_str(&hint);
    }
    anyhow!(msg)
}

fn is_ssh(url: &str) -> bool {
    url.starts_with("ssh://") || (!url.contains("://") && url.contains('@') && url.contains(':'))
}

fn host_of(url: &str) -> Option<String> {
    if let Ok(parsed) = url::Url::parse(url) {
        return parsed.host_str().map(str::to_string);
    }
    // scp-style: user@host:path
    let after_user = url.split_once('@').map_or(url, |(_, rest)| rest);
    after_user.split_once(':').map(|(h, _)| h.to_string())
}

/// What to try, for the failures `git` reports when a remote cannot be reached.
pub(crate) fn hint_for(detail: &str, url: &str) -> Option<String> {
    let lower = detail.to_lowercase();
    if lower.contains("host key verification failed") {
        let host = host_of(url).unwrap_or_else(|| "<host>".to_string());
        return Some(format!(
            "the host key for {host} is not trusted; run `ssh {host}` once to trust it, or add it with `ssh-keyscan {host} >> ~/.ssh/known_hosts`"
        ));
    }
    if lower.contains("permission denied (publickey")
        || lower.contains("permission denied, please try again")
    {
        return Some(
            "ssh rejected your key; check `ssh -T` against the host, load your key with `ssh-add`, or set the key in ~/.ssh/config".to_string(),
        );
    }
    if lower.contains("terminal prompts disabled")
        || lower.contains("authentication failed")
        || lower.contains("invalid username or password")
        || lower.contains("could not read username")
        || lower.contains("could not read password")
    {
        return Some(
            "git has no stored login for this remote; set up a credential helper (`gh auth setup-git`, or `git config --global credential.helper <helper>`) and sign in once with `git ls-remote <url>`".to_string(),
        );
    }
    if lower.contains("repository not found") || lower.contains("returned error: 404") {
        return Some(if is_ssh(url) {
            "the repository does not exist, or your key has no access to it".to_string()
        } else {
            "the repository does not exist, or your login has no access to it (private repositories answer 404 to an unauthenticated request)".to_string()
        });
    }
    if lower.contains("could not run git") {
        return Some("is git installed and on PATH?".to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strs(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn clone_arguments_depend_on_the_mode() {
        let dest = Path::new("/tmp/d");
        assert_eq!(
            strs(&clone_args("https://h/r.git", dest, false, false)),
            ["clone", "--quiet", "--", "https://h/r.git", "/tmp/d"]
        );
        assert_eq!(
            strs(&clone_args("https://h/r.git", dest, true, true)),
            [
                "clone",
                "--quiet",
                "--depth",
                "1",
                "--no-checkout",
                "--",
                "https://h/r.git",
                "/tmp/d"
            ]
        );
    }

    #[test]
    fn a_url_that_looks_like_an_option_is_not_one() {
        // `--` ends option parsing, so `--upload-pack=...` in a manifest URL is a
        // repository name, not a flag.
        let args = strs(&clone_args(
            "--upload-pack=touch /tmp/x",
            Path::new("d"),
            false,
            false,
        ));
        let dashdash = args.iter().position(|a| a == "--").unwrap();
        let url = args
            .iter()
            .position(|a| a.starts_with("--upload-pack"))
            .unwrap();
        assert!(dashdash < url);
    }

    #[test]
    fn fetch_arguments() {
        assert_eq!(
            strs(&fetch_head_args(true)),
            ["fetch", "--quiet", "--depth", "1", "origin", "HEAD"]
        );
        assert_eq!(
            strs(&fetch_head_args(false)),
            ["fetch", "--quiet", "origin", "HEAD"]
        );
        assert_eq!(
            strs(&fetch_rev_args("abc123")),
            ["fetch", "--quiet", "--depth", "1", "origin", "abc123"]
        );
        assert!(strs(&fetch_branches_args()).contains(&"--update-head-ok".to_string()));
    }

    #[test]
    fn git_never_prompts_and_ignores_hook_variables() {
        let cmd = command(None, &[os("--version")]);
        let envs: Vec<(String, Option<String>)> = cmd
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect();
        assert!(envs.contains(&("GIT_TERMINAL_PROMPT".into(), Some("0".into()))));
        for var in HOOK_VARIABLES {
            assert!(envs.contains(&(var.into(), None)), "{var} must be removed");
        }
        // Nothing else is overridden: the user's own configuration applies.
        assert!(envs
            .iter()
            .all(|(k, _)| k == "GIT_TERMINAL_PROMPT" || k.starts_with("GIT_")));
        assert!(!envs
            .iter()
            .any(|(k, _)| k == "GIT_SSH_COMMAND" || k == "GIT_ASKPASS"));
    }

    #[test]
    fn failures_hide_the_login_and_say_what_to_do() {
        let e = failure(
            "failed to clone",
            "https://tok:en@github.com/me/reg.git",
            "fatal: could not read Username for 'https://tok:en@github.com': terminal prompts disabled",
        )
        .to_string();
        assert!(!e.contains("tok:en"), "{e}");
        assert!(
            e.starts_with("failed to clone https://github.com/me/reg.git:"),
            "{e}"
        );
        assert!(
            e.contains("hint:") && e.contains("gh auth setup-git"),
            "{e}"
        );
    }

    #[test]
    fn each_failure_gets_its_own_hint() {
        let ssh = "git@github.com:me/reg.git";
        assert!(hint_for("Permission denied (publickey).", ssh)
            .unwrap()
            .contains("ssh-add"));
        let host = hint_for("Host key verification failed.", ssh).unwrap();
        assert!(host.contains("ssh github.com"), "{host}");
        assert!(hint_for("remote: Repository not found.", ssh)
            .unwrap()
            .contains("key"));
        assert!(hint_for("remote: Repository not found.", "https://h/r")
            .unwrap()
            .contains("login"));
        assert!(hint_for(
            "fatal: unable to access: Could not resolve host",
            "https://h/r"
        )
        .is_none());
    }

    #[test]
    fn the_timeout_can_be_set_or_disabled() {
        assert_eq!(parse_timeout(None), Some(DEFAULT_TIMEOUT));
        assert_eq!(parse_timeout(Some("junk")), Some(DEFAULT_TIMEOUT));
        assert_eq!(parse_timeout(Some(" 7 ")), Some(Duration::from_secs(7)));
        assert_eq!(parse_timeout(Some("0")), None);
    }
}
