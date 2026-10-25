//! Keeping logins out of logs and messages.
//!
//! A URL can carry a login: `https://TOKEN@host/x`, `https://user:pass@host/x`.
//! Harbour never prints one. Every URL that reaches a log line or an error goes
//! through [`redact_url`], and free text that may contain URLs (a `git` error,
//! say) through [`redact_text`].

/// `url` without a password or token embedded in it.
///
/// `https://TOKEN@host/x` and `https://user:pass@host/x` lose their userinfo.
/// An `ssh://` URL keeps its user name (it is not a secret) but not a password.
/// scp-style `git@host:path` has no password and is returned unchanged.
pub fn redact_url(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    let Some((userinfo, host)) = authority.rsplit_once('@') else {
        return url.to_string();
    };
    if scheme.eq_ignore_ascii_case("ssh") {
        if !userinfo.contains(':') {
            return url.to_string();
        }
        let user = userinfo.split(':').next().unwrap_or_default();
        return format!("{scheme}://{user}@{host}{tail}");
    }
    format!("{scheme}://{host}{tail}")
}

/// Redact every URL in free text such as the stderr of a failed `git`.
pub fn redact_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("://") {
        // Walk back to the start of the scheme.
        let scheme_start = rest[..i]
            .rfind(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')))
            .map_or(0, |p| p + 1);
        let after = &rest[i + 3..];
        let url_end = after
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '`'))
            .unwrap_or(after.len());
        out.push_str(&rest[..scheme_start]);
        out.push_str(&redact_url(&rest[scheme_start..i + 3 + url_end]));
        rest = &after[url_end..];
    }
    out.push_str(rest);
    out
}

/// Warn when a login is written into a URL: it is kept in `Harbour.toml` and
/// `Harbour.lock`, which are usually committed.
pub fn warn_if_embedded_login(url: &str) {
    if redact_url(url) != url {
        tracing::warn!(
            "{} has a login written into its URL; Harbour.toml and Harbour.lock record it. Prefer a git credential helper (`gh auth setup-git`)",
            redact_url(url)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_a_token_or_password_from_http_urls() {
        assert_eq!(
            redact_url("https://ghp_secret@github.com/me/reg.git"),
            "https://github.com/me/reg.git"
        );
        assert_eq!(
            redact_url("http://alice:s3cret@127.0.0.1:8080/srv.git?x=1#f"),
            "http://127.0.0.1:8080/srv.git?x=1#f"
        );
    }

    #[test]
    fn leaves_urls_without_a_login_alone() {
        for u in [
            "https://github.com/me/reg.git",
            "git@github.com:me/reg.git",
            "ssh://git@github.com/me/reg.git",
            "file:///tmp/reg",
            "/some/local/path",
            "https://host/a@b",
        ] {
            assert_eq!(redact_url(u), u, "{u}");
        }
    }

    #[test]
    fn ssh_keeps_the_user_but_not_a_password() {
        assert_eq!(
            redact_url("ssh://git:pw@host:2222/x.git"),
            "ssh://git@host:2222/x.git"
        );
    }

    #[test]
    fn redacts_urls_inside_error_text() {
        let text = "fatal: unable to access 'https://u:p@host/r.git/': 403; also git@host:r.git";
        let out = redact_text(text);
        assert!(!out.contains("u:p"), "{out}");
        assert!(out.contains("'https://host/r.git/'"), "{out}");
        assert!(out.contains("git@host:r.git"), "{out}");
    }

    #[test]
    fn text_without_urls_is_unchanged() {
        assert_eq!(redact_text("nothing to see"), "nothing to see");
    }
}
