//! `harbour completions` writing into a pipe whose reader goes away.

use std::io::{BufRead, BufReader};
use std::process::Stdio;

use crate::common::*;

/// `harbour completions bash | head -1` closes the pipe after one line. The
/// script is larger than a pipe buffer, so the write fails with `BrokenPipe`;
/// that used to surface as a panic with a backtrace from inside
/// `clap_complete::generate`. A reader that stops early is not an error.
#[test]
fn test_completions_into_a_closed_pipe_does_not_panic() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);

    let mut child = harbour(&home)
        .args(["completions", "bash"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let mut first = String::new();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    stdout.read_line(&mut first).unwrap();
    assert!(!first.is_empty(), "expected the first line of the script");
    // Close the read end while the child still has most of the script to write.
    drop(stdout);

    let out = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("panicked") && !stderr.contains("Broken pipe"),
        "a closed pipe must end the command quietly\n{stderr}"
    );
    assert!(out.status.success(), "exit {:?}\n{stderr}", out.status);
}
