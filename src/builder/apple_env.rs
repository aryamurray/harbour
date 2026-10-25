//! The Apple deployment target and SDK a compile will actually use.
//!
//! `MACOSX_DEPLOYMENT_TARGET` (and `SDKROOT`) change the code clang emits --
//! the `minos` of the binary, which availability-guarded code is compiled in,
//! which declarations a header exposes -- without changing a single argument
//! on the command line, so neither the compile fingerprint nor the probe
//! cache saw them. Both are fed by [`ToolchainFingerprint`], which is where
//! the answer goes.
//!
//! The answer is asked of the compiler rather than reconstructed from the
//! environment: `clang -###` prints the `-triple` (which carries the resolved
//! deployment target, whichever variable or default produced it) and the
//! `-isysroot` it would pass. That also covers the per-OS variables
//! (`IPHONEOS_DEPLOYMENT_TARGET` and friends) without listing them.
//!
//! [`ToolchainFingerprint`]: crate::builder::fingerprint::ToolchainFingerprint

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

/// What the compiler resolved for an Apple target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppleBuildEnv {
    /// The OS part of clang's resolved triple, e.g. `macosx12.0.0`: the
    /// effective deployment target.
    pub deployment_target: String,
    /// The SDK clang would compile against: its path and, when it has one,
    /// the version from `SDKSettings.json`.
    pub sdk: String,
}

/// Ask `cc` what it would use for `target_flags`, with the environment the
/// build will give it (`env`) on top of this process's.
///
/// `None` when the compiler cannot be run or does not say, which leaves the
/// fingerprint as it was rather than inventing a value.
pub fn detect(
    cc: &Path,
    target_flags: &[String],
    env: &[(String, String)],
) -> Option<AppleBuildEnv> {
    // The inputs that can change the answer. Spawning a compiler for every
    // fingerprint would be wasteful; spawning it once per distinct question
    // is not.
    let inherited: Vec<(&str, Option<String>)> = INHERITED
        .iter()
        .map(|k| (*k, std::env::var(k).ok()))
        .collect();
    let key = format!("{cc:?}|{target_flags:?}|{env:?}|{inherited:?}");

    static CACHE: OnceLock<Mutex<HashMap<String, Option<AppleBuildEnv>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(hit) = cache.lock().ok()?.get(&key) {
        return hit.clone();
    }

    let found = ask_compiler(cc, target_flags, env);
    cache.lock().ok()?.insert(key, found.clone());
    found
}

/// Variables clang reads from the inherited environment. Only part of the
/// cache key: the values themselves are read by the compiler.
const INHERITED: &[&str] = &[
    "MACOSX_DEPLOYMENT_TARGET",
    "IPHONEOS_DEPLOYMENT_TARGET",
    "TVOS_DEPLOYMENT_TARGET",
    "WATCHOS_DEPLOYMENT_TARGET",
    "XROS_DEPLOYMENT_TARGET",
    "SDKROOT",
];

fn ask_compiler(
    cc: &Path,
    target_flags: &[String],
    env: &[(String, String)],
) -> Option<AppleBuildEnv> {
    let mut cmd = std::process::Command::new(cc);
    cmd.arg("-###")
        .args(target_flags)
        .args(["-c", "-x", "c", "/dev/null"]);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let output = cmd.output().ok()?;
    if !output.status.success() {
        return None;
    }
    parse(&String::from_utf8_lossy(&output.stderr))
}

/// Pull the triple and sysroot out of `-###` output (which goes to stderr).
fn parse(text: &str) -> Option<AppleBuildEnv> {
    let triple = value_after(text, "-triple")?;
    let (_, os) = triple.split_once("-apple-")?;
    let sdk = match value_after(text, "-isysroot") {
        Some(root) => {
            let version = sdk_version(Path::new(&root));
            format!("{root}@{}", version.unwrap_or_default())
        }
        None => String::new(),
    };
    Some(AppleBuildEnv {
        deployment_target: os.to_string(),
        sdk,
    })
}

/// The quoted argument following `flag` in a `-###` command line.
fn value_after(text: &str, flag: &str) -> Option<String> {
    let needle = format!("\"{flag}\" \"");
    let start = text.find(&needle)? + needle.len();
    let rest = &text[start..];
    Some(rest[..rest.find('"')?].to_string())
}

/// `Version` from the SDK's `SDKSettings.json`, without a JSON dependency on
/// the layout beyond that one key.
fn sdk_version(sdk: &Path) -> Option<String> {
    let text = std::fs::read_to_string(sdk.join("SDKSettings.json")).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    json.get("Version")?.as_str().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"Apple clang version 21.0.0
 "/Library/Developer/CommandLineTools/usr/bin/clang" "-cc1" "-triple" "arm64-apple-macosx12.0.0" "-isysroot" "/nonexistent/MacOSX.sdk" "-x" "c" "/dev/null""#;

    #[test]
    fn parses_triple_and_sysroot() {
        let env = parse(SAMPLE).unwrap();
        assert_eq!(env.deployment_target, "macosx12.0.0");
        assert_eq!(env.sdk, "/nonexistent/MacOSX.sdk@");
    }

    #[test]
    fn a_non_apple_triple_yields_nothing() {
        assert_eq!(parse(r#""-triple" "x86_64-unknown-linux-gnu""#), None);
    }
}
