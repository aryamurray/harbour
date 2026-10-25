//! A login written into a git or registry URL must not reach `Harbour.lock` or a
//! message that prints the source.

use harbour::core::source_id::GitReference;
use harbour::core::SourceId;
use url::Url;

#[test]
fn the_lock_string_and_the_display_of_a_git_source_carry_no_login() {
    let url = Url::parse("https://tok:en@example.com/me/lib.git").unwrap();
    let id = SourceId::for_git(&url, GitReference::Tag("v1".into())).unwrap();
    for text in [id.to_url_string(), id.to_string()] {
        assert!(!text.contains("tok"), "{text}");
        assert!(text.contains("example.com/me/lib.git"), "{text}");
    }
    // The source still fetches with its login: only what is written is redacted.
    assert_eq!(id.url().username(), "tok");
}

#[test]
fn the_lock_string_of_a_registry_carries_no_login() {
    let url = Url::parse("http://alice:s3cret@127.0.0.1:8765/srv.git").unwrap();
    let id = SourceId::for_registry(&url).unwrap();
    assert_eq!(id.to_url_string(), "registry+http://127.0.0.1:8765/srv.git");
    assert_eq!(id.to_string(), "registry+http://127.0.0.1:8765/srv.git");
}

#[test]
fn a_source_without_a_login_is_written_as_before() {
    let url = Url::parse("https://example.com/me/lib.git").unwrap();
    let id = SourceId::for_git(&url, GitReference::Branch("main".into())).unwrap();
    assert_eq!(
        id.to_url_string(),
        "git+https://example.com/me/lib.git?branch=main"
    );
}

#[test]
fn an_scp_style_source_has_no_password_to_remove() {
    // `git@host:path` is not mangled: there is no login in it to redact.
    let url = harbour::util::remote_url::parse_remote_url("git@github.com:me/lib.git").unwrap();
    let id = SourceId::for_git(&url, GitReference::Tag("v1".into())).unwrap();
    assert_eq!(
        id.to_url_string(),
        "git+ssh://git@github.com/me/lib.git?tag=v1"
    );
    assert_eq!(
        harbour::util::redact::redact_url("git@github.com:me/lib.git"),
        "git@github.com:me/lib.git"
    );
}
