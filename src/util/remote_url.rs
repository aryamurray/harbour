//! Remote URLs as people write them.
//!
//! `git` accepts three spellings of an SSH remote: `ssh://git@host/owner/repo`,
//! and the scp-like `git@host:owner/repo`, which is what GitHub's "Clone" button
//! shows. The `url` crate only knows the first, and a registry or git dependency
//! written the second way used to be dropped without a word.
//!
//! [`parse_remote_url`] accepts all of them. An scp-like remote becomes the
//! `ssh://` URL that names the same repository, which is what identifies the
//! source (cache directory, lock file); the URL handed to `git` is the one the
//! user wrote ([`git_arg`]), because the two spell different paths when the path
//! is relative (`host:repo` is relative to the login's home, `ssh://host/repo` is
//! absolute) and `git` understands its own syntax.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use anyhow::{bail, Result};
use url::Url;

/// The scp-like URLs seen so far, by the `ssh://` URL that stands for each.
static WRITTEN: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

/// Whether `s` is `[user@]host:path`, the way `git` decides: no `://`, and the
/// first `:` comes before any `/`. A single letter before the colon is a
/// Windows drive (`C:\repo`, `C:/repo`), a local path.
pub fn is_scp_like(s: &str) -> bool {
    if s.contains("://") {
        return false;
    }
    let Some(colon) = host_end(s) else {
        return false;
    };
    let host = &s[..colon];
    if s[..colon].contains('/') || s[colon + 1..].is_empty() {
        return false;
    }
    let bare = host.rsplit_once('@').map_or(host, |(_, h)| h);
    // `C:` is a drive, not a host.
    !(bare.len() == 1 && bare.chars().all(|c| c.is_ascii_alphabetic()))
        && !bare.is_empty()
        && !host.is_empty()
}

/// The index of the `:` that ends the host of an scp-like URL.
fn host_end(s: &str) -> Option<usize> {
    // A bracketed IPv6 host may contain colons: `git@[::1]:repo`.
    if let Some(open) = s.find('[') {
        if let Some(close) = s[open..].find(']') {
            let after = open + close + 1;
            return s[after..].starts_with(':').then_some(after);
        }
    }
    s.find(':')
}

/// Parse a registry or git URL: `https://`, `ssh://`, `file://` and the like, or
/// the scp-like `[user@]host:path`.
pub fn parse_remote_url(s: &str) -> Result<Url> {
    if is_scp_like(s) {
        return scp_to_url(s);
    }
    let expected =
        "write it as https://host/path, ssh://[user@]host/path, file:///path, or user@host:path";
    match Url::parse(s) {
        // `C:\repo` parses as a URL with the scheme `c`; it is a local path.
        Ok(url) if url.scheme().len() == 1 => {
            bail!("'{s}' is not a repository URL (it looks like a Windows path); {expected}")
        }
        Ok(url) => Ok(url),
        Err(e) => bail!("'{s}' is not a repository URL ({e}); {expected}"),
    }
}

fn scp_to_url(s: &str) -> Result<Url> {
    let colon = host_end(s).expect("is_scp_like checked");
    let (host, path) = (&s[..colon], &s[colon + 1..]);
    // An absolute scp path (`host:/srv/repo`) keeps its leading `/`, which makes
    // `ssh://host//srv/repo`: distinct from the relative `host:srv/repo`
    // (`ssh://host/srv/repo`), which names the same repository only on a server
    // that keeps its repositories at the root.
    let normalized = Url::parse(&format!("ssh://{host}/{path}"))
        .map_err(|e| anyhow::anyhow!("'{s}' is not a repository URL: {e}"))?;
    WRITTEN
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .insert(normalized.as_str().to_string(), s.to_string());
    Ok(normalized)
}

/// The URL to give `git`: what the user wrote for an scp-like remote, otherwise
/// `url` itself.
pub fn git_arg(url: &str) -> String {
    WRITTEN
        .get()
        .and_then(|m| m.lock().unwrap().get(url).cloned())
        .unwrap_or_else(|| url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_the_spellings_of_a_remote() {
        for scp in [
            "git@github.com:aryamurray/harbour-registry.git",
            "github.com:owner/repo",
            "me@example.org:/srv/git/repo.git",
            "git@host:~alice/repo.git",
            "git@host:2222/repo.git",
            "git@[::1]:repo.git",
            "host:a:b",
        ] {
            assert!(is_scp_like(scp), "{scp} is scp-like");
        }
        for not_scp in [
            "https://github.com/o/r.git",
            "ssh://git@github.com/o/r.git",
            "file:///tmp/registry",
            "file:///C:/registry",
            "C:\\registry",
            "C:/registry",
            "/abs/path",
            "./rel:path/x",
            "relative/path:x",
            "host:",
            "",
            "no-colon",
            "@:x",
        ] {
            assert!(!is_scp_like(not_scp), "{not_scp} is not scp-like");
        }
    }

    #[test]
    fn an_scp_like_remote_becomes_the_ssh_url_of_the_same_repository() {
        let url = parse_remote_url("git@github.com:aryamurray/harbour-registry.git").unwrap();
        assert_eq!(
            url.as_str(),
            "ssh://git@github.com/aryamurray/harbour-registry.git"
        );
        // Same repository as the ssh:// spelling of it.
        assert_eq!(
            url,
            parse_remote_url("ssh://git@github.com/aryamurray/harbour-registry.git").unwrap()
        );
    }

    #[test]
    fn a_port_like_segment_or_a_tilde_is_part_of_the_path() {
        // In an scp-like URL `:2222/repo` is the path `2222/repo`, never a port.
        let port_like = parse_remote_url("git@host:2222/repo.git").unwrap();
        assert_eq!(port_like.as_str(), "ssh://git@host/2222/repo.git");
        assert_eq!(port_like.port(), None);
        let tilde = parse_remote_url("git@host:~alice/repo.git").unwrap();
        assert_eq!(tilde.path(), "/~alice/repo.git");
    }

    #[test]
    fn relative_and_absolute_scp_paths_stay_distinct() {
        let relative = parse_remote_url("git@host:srv/repo.git").unwrap();
        let absolute = parse_remote_url("git@host:/srv/repo.git").unwrap();
        assert_ne!(relative, absolute);
    }

    #[test]
    fn git_gets_the_url_as_written() {
        let written = "git@host.example:team/reg.git";
        let url = parse_remote_url(written).unwrap();
        assert_eq!(git_arg(url.as_str()), written);
        let absolute = "git@host.example:/srv/reg.git";
        let url = parse_remote_url(absolute).unwrap();
        assert_eq!(git_arg(url.as_str()), absolute);
        // Anything else passes through untouched.
        assert_eq!(
            git_arg("https://example.org/a.git"),
            "https://example.org/a.git"
        );
    }

    #[test]
    fn ordinary_urls_parse_as_before() {
        for u in [
            "https://github.com/o/r.git",
            "ssh://git@github.com/o/r.git",
            "file:///tmp/registry",
        ] {
            assert_eq!(parse_remote_url(u).unwrap(), Url::parse(u).unwrap());
        }
    }

    #[test]
    fn a_string_that_is_no_url_is_a_loud_error_naming_the_forms() {
        for bad in ["not a url", "registry", "/just/a/path", "C:\\x", ""] {
            let err = parse_remote_url(bad).unwrap_err().to_string();
            assert!(err.contains("is not a repository URL"), "{bad}: {err}");
            assert!(err.contains("user@host:path"), "{bad}: {err}");
        }
    }

    #[test]
    fn the_cache_directory_is_stable_and_shared_with_the_https_spelling() {
        use crate::util::fs::sanitize_url_for_path;
        let scp = parse_remote_url("git@github.com:me/reg.git").unwrap();
        let https = parse_remote_url("https://github.com/me/reg.git").unwrap();
        assert_eq!(sanitize_url_for_path(&scp), sanitize_url_for_path(&https));
        assert_eq!(sanitize_url_for_path(&scp), "github.com-me-reg");
    }
}
