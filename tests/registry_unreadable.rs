//! A registry Harbour cannot read is reported as such, not as "package not found".
//!
//! A private registry the user's git login cannot open used to end in
//! `package 'x' not found in registries`, which sent people looking for a typo
//! instead of at their credentials.

mod common;

use assert_cmd::prelude::*;
use common::{harbour, home};
use harbour::test_support::fixtures::local_registry;
use predicates::prelude::*;
use tempfile::TempDir;

#[test]
fn add_says_the_registry_could_not_be_read() {
    let tmp = TempDir::new().unwrap();
    let home = home(&tmp);
    harbour(&home)
        .args(["new", "addtest"])
        .current_dir(tmp.path())
        .assert()
        .success();

    // A registry that is not there: the fetch fails, which is what a private
    // repository the login cannot open looks like.
    let missing = local_registry::file_url(&tmp.path().join("no-such-registry"));
    harbour(&home)
        .env("HARBOUR_TEST_REGISTRY_URL", &missing)
        .args(["add", "somepkg"])
        .current_dir(tmp.path().join("addtest"))
        .assert()
        .failure()
        .stderr(predicate::str::contains("failed to read registry"))
        .stderr(predicate::str::contains("no-such-registry"))
        .stderr(predicate::str::contains("not found in registries").not());
}
