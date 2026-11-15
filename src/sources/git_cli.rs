//! Every git operation Harbour performs, through the system `git`.
//!
//! `git` is a hard requirement. A reimplementation never reaches the
//! configuration that makes a remote reachable for a person: credential
//! helpers, `gh auth setup-git`, the keychain, `~/.ssh/config` aliases and
//! `ProxyJump`, `GIT_SSH_COMMAND`, `includeIf`, `url.<base>.insteadOf`, hardware
//! keys. The `git` command reads all of it, so clone, fetch, checkout and
//! `git apply` all run `git` and nothing is reimplemented. (Cargo's
//! `net.git-fetch-with-cli` exists for the same reason.) This module is the only
//! place that starts `git`; callers get paths and commit ids back, never a
//! repository handle. A machine without `git` gets one clear error
//! ([`git_program`]), never a raw "No such file or directory".
//!
//! `git` runs with the user's own configuration untouched, because that is the
//! point. Only these things are set: `GIT_TERMINAL_PROMPT=0`, so a missing login
//! is an error rather than a hang; the variables a surrounding `git` hook
//! exports (`GIT_DIR` and friends) are removed so they cannot redirect a command
//! into the wrong repository; repository discovery stops at the working
//! directory's parent (`GIT_CEILING_DIRECTORIES`), so a directory that is not a
//! repository is never mistaken for part of an enclosing one (the cache can sit
//! inside a project's own repository); and `safe.directory=*` is passed for
//! commands that run in a directory Harbour manages, so a cache shared between
//! users does not trip git's ownership check. A command that runs longer than
//! [`DEFAULT_TIMEOUT`] is killed (`HARBOUR_GIT_TIMEOUT=<seconds>` changes it, `0`
//! disables it).
//!
//! Byte-exact checkouts ([`checkout_exact`]) are the one place the user's
//! configuration must NOT apply; see there.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};

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

/// Said when `git` cannot be found: what needs it, and how to get it.
pub const MISSING_GIT: &str = "git was not found on PATH. Harbour runs the system `git` to fetch git dependencies and registries and to apply registry patches, so git must be installed (Debian/Ubuntu: `apt install git`; Fedora: `dnf install git`; macOS: `xcode-select --install` or `brew install git`; Windows: `winget install Git.Git`). `harbour doctor` checks for it";

/// The `git` executable, found on `PATH`.
///
/// The one check every operation goes through. `which` resolves `git.exe` on
/// Windows, and the full path is what gets spawned, so no shell is involved.
pub fn git_program() -> Result<PathBuf> {
    warn_about_removed_switch();
    which::which("git").map_err(|_| anyhow!(MISSING_GIT))
}

/// `HARBOUR_GIT_BACKEND` selected a libgit2 fallback that no longer exists.
/// Setting it is not an error (an old CI file may still export it), but it is
/// not honoured silently either.
fn warn_about_removed_switch() {
    static WARNED: OnceLock<()> = OnceLock::new();
    if std::env::var_os("HARBOUR_GIT_BACKEND").is_some() {
        WARNED.get_or_init(|| {
            tracing::warn!(
                "HARBOUR_GIT_BACKEND no longer exists and is ignored: Harbour always uses the system git. Remove it from your environment"
            );
        });
    }
}

/// A full commit id: 40 hexadecimal digits.
///
/// Everything handed to `git` as a revision is checked with this first, so a
/// manifest or shim cannot smuggle an option (`--upload-pack=...`) in as a
/// "rev", and an abbreviation (which names different commits as a history
/// grows) is not accepted where a pin is expected.
pub fn validate_commit_id(rev: &str) -> Result<()> {
    anyhow::ensure!(
        rev.len() == 40 && rev.chars().all(|c| c.is_ascii_hexdigit()),
        "invalid commit id: {rev} (expected 40 hexadecimal digits)"
    );
    Ok(())
}

/// Attributes that switch off every content conversion git does on checkout.
/// Written to `info/attributes`, which outranks any `.gitattributes` in the tree
/// and the user's `core.attributesFile`.
const EXACT_ATTRIBUTES: &str = "# Written by Harbour: check files out exactly as committed.\n* -text -eol -filter -ident -working-tree-encoding -export-subst\n";

// ---------------------------------------------------------------------------
// Operations
// ---------------------------------------------------------------------------

/// Clone `url` into `dest` and return the commit its `HEAD` is at.
///
/// `shallow` fetches only the tip commit. `exact` writes exactly the committed
/// bytes (see [`checkout_exact`]).
pub fn clone_repo(url: &str, dest: &Path, shallow: bool, exact: bool) -> Result<String> {
    warn_if_embedded_login(url);
    // `git` gets the URL as the user wrote it (see `remote_url`).
    let remote = git_arg(url);
    run_git(
        None,
        &clone_args(&remote, dest, shallow, exact),
        url,
        "failed to clone",
    )?;
    let head = resolve_in(dest, "HEAD", url, "failed to read the cloned commit of")?;
    if exact {
        // The clone wrote nothing (`--no-checkout`): write the tree ourselves.
        checkout_exact(dest, &head)?;
    }
    Ok(head)
}

/// Bring every branch (and tag) of `origin` up to date in the clone at `repo_dir`.
pub fn fetch_branches(repo_dir: &Path, url: &str) -> Result<()> {
    run_git(
        Some(repo_dir),
        &fetch_branches_args(),
        url,
        "failed to fetch",
    )?;
    Ok(())
}

/// Fetch the remote's current `HEAD` into the clone at `repo_dir` and return its
/// commit. `shallow` fetches only that commit.
pub fn fetch_head(repo_dir: &Path, url: &str, shallow: bool) -> Result<String> {
    run_git(
        Some(repo_dir),
        &fetch_head_args(shallow),
        url,
        "failed to fetch",
    )?;
    resolve_in(
        repo_dir,
        "FETCH_HEAD",
        url,
        "failed to read the fetched commit of",
    )
}

/// Create a repository at `dest` holding commit `rev` of `url` (not checked out).
///
/// Asks the server for just that commit first, which is a few kilobytes for a
/// large project; a server that will not serve an arbitrary commit that way gets
/// a full fetch instead. A remote that has no such commit is an error.
pub fn fetch_rev(url: &str, dest: &Path, rev: &str) -> Result<()> {
    validate_commit_id(rev)?;
    warn_if_embedded_login(url);
    std::fs::create_dir_all(dest)
        .with_context(|| format!("failed to create {}", dest.display()))?;
    run_git(
        Some(dest),
        &osv(&["init", "--quiet"]),
        url,
        "failed to create a repository for",
    )?;
    // `--` so a URL cannot be taken for an option.
    let mut add = osv(&["remote", "add", "--", "origin"]);
    add.push(OsString::from(git_arg(url)));
    run_git(Some(dest), &add, url, "failed to set up a repository for")?;
    if let Err(e) = run_git(Some(dest), &fetch_rev_args(rev), url, "failed to fetch") {
        tracing::debug!("fetching {rev} alone failed ({e:#}); fetching everything instead");
        run_git(Some(dest), &fetch_all_args(), url, "failed to fetch")?;
    }
    if !has_commit(dest, rev) {
        bail!("{} has no commit {rev}", redact_url(url));
    }
    Ok(())
}

/// Write the working tree of the repository at `dir` to exactly `commit`
/// (`git reset --hard`), with the user's configuration applying as it would to
/// any checkout of their own.
pub fn reset_hard(dir: &Path, commit: &str) -> Result<()> {
    validate_commit_id(commit)?;
    let mut args = osv(&["reset", "--hard", "--quiet"]);
    args.push(OsString::from(commit));
    run_git(
        Some(dir),
        &args,
        &dir.display().to_string(),
        "failed to reset",
    )?;
    Ok(())
}

/// Like [`reset_hard`], but every file is written with exactly the committed
/// bytes, and the repository is left set up so later operations on it agree.
///
/// A patch is verified by the sha256 of its bytes and applied with `git apply`
/// against the upstream tree, so neither the registry nor an upstream source may
/// have its line endings converted on the way to disk. `git` honours the user's
/// `core.autocrlf` (Git for Windows sets it to `true` by default), which turns
/// every LF in a checkout into CRLF: the registry's patches then fail their
/// hash, and an upstream text file no longer matches a patch's context lines.
///
/// Configuration alone cannot prevent that: a `.gitattributes` in the tree
/// (`* text eol=crlf`, `filter=...`, `ident`, `working-tree-encoding=...`) is
/// applied whatever `core.autocrlf` says, and the registry or upstream controls
/// its own tree. So the repository's `info/attributes`, which outranks every
/// other source of attributes, unsets all of them ([`EXACT_ATTRIBUTES`]); the
/// conversion settings are forced off for the checkout itself; and
/// `core.autocrlf = false` is recorded in the repository so a later `git apply`
/// there agrees. Together that is what libgit2's `disable_filters` did.
pub fn checkout_exact(dir: &Path, commit: &str) -> Result<()> {
    validate_commit_id(commit)?;
    let subject = dir.display().to_string();
    let attributes = dir.join(".git").join("info").join("attributes");
    if let Some(parent) = attributes.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    std::fs::write(&attributes, EXACT_ATTRIBUTES)
        .with_context(|| format!("failed to write {}", attributes.display()))?;
    run_git(
        Some(dir),
        &osv(&["config", "core.autocrlf", "false"]),
        &subject,
        "failed to configure",
    )?;
    let mut args = osv(&[
        "-c",
        "core.autocrlf=false",
        "-c",
        "core.eol=lf",
        "-c",
        "core.safecrlf=false",
        "reset",
        "--hard",
        "--quiet",
    ]);
    args.push(OsString::from(commit));
    run_git(Some(dir), &args, &subject, "failed to check out")?;
    Ok(())
}

/// The commit `HEAD` is at in the repository at `dir`, or `None` when `dir` is
/// not a repository (or has no commits).
pub fn head_commit(dir: &Path) -> Option<String> {
    resolve_commit(dir, "HEAD").ok()
}

/// Whether the repository at `dir` has the commit `id` (a full commit id).
pub fn has_commit(dir: &Path, id: &str) -> bool {
    validate_commit_id(id).is_ok()
        && exec(
            Some(dir),
            &osv(&["cat-file", "-e", &format!("{id}^{{commit}}")]),
        )
        .is_ok_and(|out| out.success)
}

/// The commit that `spec` (`HEAD`, a full ref name, a full commit id) names in
/// the repository at `dir`.
///
/// `spec` is never taken for an option: callers pass a full ref name or a
/// validated id, and the `^{commit}` suffix means it cannot start with `-`
/// either way.
pub fn resolve_commit(dir: &Path, spec: &str) -> Result<String> {
    let out = exec(
        Some(dir),
        &osv(&[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{spec}^{{commit}}"),
        ]),
    )?;
    if !out.success {
        bail!("{spec} is not a commit in {}", dir.display());
    }
    Ok(out.stdout.trim().to_string())
}

/// Check (`check_only`) or apply the patch at `patch` to the files in
/// `source_dir`.
///
/// `Ok(None)` is success; `Ok(Some(stderr))` is a patch git rejected, left to
/// the caller to word for its own context. A tarball's extracted tree is not a
/// repository, and `git apply` run in a directory that is inside one treats a
/// patch's paths as relative to that repository's root, not to the current
/// directory. Discovery stops at `source_dir`'s parent (see the module docs), so
/// either `source_dir` is itself the repository (a git source) or there is none
/// and git applies the patch to the files in place.
pub fn apply_patch(source_dir: &Path, patch: &Path, check_only: bool) -> Result<Option<String>> {
    let mut args = osv(&["apply"]);
    if check_only {
        args.push(OsString::from("--check"));
    }
    args.push(patch.as_os_str().to_os_string());
    let out = exec(Some(source_dir), &args)?;
    Ok((!out.success).then_some(out.stderr))
}

/// A `git` command for code that builds fixtures, with the environment described
/// in the module docs. The caller adds whatever isolation it needs.
pub fn command_in(cwd: &Path, args: &[&str]) -> Result<Command> {
    Ok(command(&git_program()?, Some(cwd), &osv(args)))
}

/// [`resolve_commit`], failing like a git command about `subject`.
fn resolve_in(dir: &Path, spec: &str, subject: &str, what: &str) -> Result<String> {
    let out = run_git(
        Some(dir),
        &osv(&[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{spec}^{{commit}}"),
        ]),
        subject,
        what,
    )?;
    Ok(out.trim().to_string())
}

// ---------------------------------------------------------------------------
// Command construction (pure, so it can be tested without a server)
// ---------------------------------------------------------------------------

fn os(s: &str) -> OsString {
    OsString::from(s)
}

fn osv(args: &[&str]) -> Vec<OsString> {
    args.iter().map(|s| os(s)).collect()
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
    osv(&[
        "fetch",
        "--quiet",
        "--tags",
        "--update-head-ok",
        "origin",
        "+refs/heads/*:refs/heads/*",
    ])
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
    osv(&[
        "fetch",
        "--quiet",
        "--tags",
        "origin",
        "+refs/heads/*:refs/remotes/origin/*",
    ])
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
fn command(git: &Path, cwd: Option<&Path>, args: &[OsString]) -> Command {
    let mut cmd = Command::new(git);
    // Git for Windows refuses to create a file whose path is longer than 260
    // characters unless this is set, and it is off by default. A dependency's
    // tree is not ours to shorten: harfbuzz's test data alone has names that,
    // under a cache directory, pass the limit, and the checkout failed with
    // "Filename too long" before a single file was compiled. Other systems
    // ignore the setting.
    cmd.args(["-c", "core.longpaths=true"]);
    if cwd.is_some() {
        cmd.args(["-c", "safe.directory=*"]);
    }
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
        if let Some(ceiling) = std::path::absolute(cwd)
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
        {
            cmd.env("GIT_CEILING_DIRECTORIES", ceiling);
        }
    }
    cmd
}

/// What a finished `git` command left behind.
struct Output {
    success: bool,
    /// "exited with 128", "was killed".
    how: String,
    stdout: String,
    stderr: String,
    /// The limit, in seconds, when the command was stopped for running too long.
    timed_out: Option<u64>,
}

/// Run `git args` to completion (or the timeout). An `Err` means git could not
/// be started at all.
fn exec(cwd: Option<&Path>, args: &[OsString]) -> Result<Output> {
    let git = git_program()?;
    tracing::debug!(
        "git {}",
        args.iter()
            .map(|a| redact_text(&a.to_string_lossy()))
            .collect::<Vec<_>>()
            .join(" ")
    );
    let mut child = command(&git, cwd, args)
        .spawn()
        .map_err(|e| anyhow!("could not run git: {e}"))?;

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
    let stdout = stdout.join().unwrap_or_default();

    Ok(match status {
        Some(status) => Output {
            success: status.success(),
            how: status
                .code()
                .map_or_else(|| "was killed".to_string(), |c| format!("exited with {c}")),
            stdout,
            stderr,
            timed_out: None,
        },
        None => Output {
            success: false,
            how: "was stopped".to_string(),
            stdout,
            stderr,
            timed_out: Some(deadline.map_or(0, |(_, t)| t.as_secs())),
        },
    })
}

/// Run `git args` and turn a failure into an error about `subject` (a URL, or
/// the directory a local command ran in). Returns what git printed on stdout.
fn run_git(cwd: Option<&Path>, args: &[OsString], subject: &str, what: &str) -> Result<String> {
    let out = exec(cwd, args).map_err(|e| failure(what, subject, &format!("{e}")))?;
    if let Some(secs) = out.timed_out {
        return Err(failure(
            what,
            subject,
            &format!(
                "git did not finish within {secs}s and was stopped (set HARBOUR_GIT_TIMEOUT=<seconds> to change this); {}",
                out.stderr.trim()
            ),
        ));
    }
    if out.success {
        return Ok(out.stdout);
    }
    let detail = out.stderr.trim();
    Err(failure(
        what,
        subject,
        &if detail.is_empty() {
            format!("git {}", out.how)
        } else {
            detail.to_string()
        },
    ))
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
        let cmd = command(Path::new("git"), None, &[os("--version")]);
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

    #[test]
    fn only_a_full_hex_commit_id_is_a_commit_id() {
        let full = "0123456789abcdef0123456789ABCDEF01234567";
        assert!(validate_commit_id(full).is_ok());
        for bad in [
            "",
            "abc123",
            "--upload-pack=touch x",
            &full[..39],
            &format!("{full}0"),
            "g123456789012345678901234567890123456789",
        ] {
            let e = validate_commit_id(bad).unwrap_err().to_string();
            assert!(e.contains("invalid commit id"), "{bad}: {e}");
        }
    }

    #[test]
    fn exact_checkouts_unset_every_conversion() {
        for attr in [
            "-text",
            "-eol",
            "-filter",
            "-ident",
            "-working-tree-encoding",
            "-export-subst",
        ] {
            assert!(EXACT_ATTRIBUTES.contains(attr), "{attr}");
        }
    }

    #[test]
    fn the_missing_git_message_says_what_needs_it_and_how_to_install_it() {
        for needle in [
            "not found on PATH",
            "dependencies",
            "registries",
            "patches",
            "apt install git",
            "winget",
        ] {
            assert!(MISSING_GIT.contains(needle), "{needle}");
        }
    }
}
