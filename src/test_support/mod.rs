//! Test utilities and mocks for Harbour unit tests.
//!
//! This module provides mock implementations for key interfaces that are
//! difficult to test in isolation, such as filesystem operations, process
//! execution, and HTTP clients.
//!
//! # Example
//!
//! ```rust,ignore
//! use harbour::test_support::{MockFileSystem, MockExecutor, create_test_workspace};
//!
//! #[test]
//! fn test_example() {
//!     let mut fs = MockFileSystem::new();
//!     fs.add_file("Harbor.toml", b"[package]\nname = \"test\"");
//!
//!     let mut exec = MockExecutor::new();
//!     exec.expect("gcc --version", MockProcessOutput::success("gcc 12.0.0"));
//!
//!     // Use mocks in tests...
//! }
//! ```

pub mod fixtures;

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{bail, Result};

// Re-export fixtures for convenience
pub use fixtures::*;

/// Mock filesystem for testing without real I/O.
///
/// Provides an in-memory filesystem that can be used to test code that
/// performs file operations without touching the real filesystem.
#[derive(Debug, Clone, Default)]
pub struct MockFileSystem {
    files: HashMap<PathBuf, Vec<u8>>,
    dirs: Vec<PathBuf>,
}

impl MockFileSystem {
    /// Create a new empty mock filesystem.
    pub fn new() -> Self {
        MockFileSystem {
            files: HashMap::new(),
            dirs: Vec::new(),
        }
    }

    /// Add a file with the given content.
    pub fn add_file(&mut self, path: impl AsRef<Path>, content: impl Into<Vec<u8>>) {
        let path = path.as_ref().to_path_buf();
        // Ensure parent directories exist
        if let Some(parent) = path.parent() {
            self.add_dir(parent);
        }
        self.files.insert(path, content.into());
    }

    /// Add a directory.
    pub fn add_dir(&mut self, path: impl AsRef<Path>) {
        let path = path.as_ref().to_path_buf();
        if !self.dirs.contains(&path) {
            // Add all parent directories too
            let mut current = path.clone();
            while let Some(parent) = current.parent() {
                if parent.as_os_str().is_empty() {
                    break;
                }
                if !self.dirs.contains(&parent.to_path_buf()) {
                    self.dirs.push(parent.to_path_buf());
                }
                current = parent.to_path_buf();
            }
            self.dirs.push(path);
        }
    }

    /// Read a file's contents.
    pub fn read(&self, path: &Path) -> Result<Vec<u8>> {
        self.files
            .get(path)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("file not found: {}", path.display()))
    }

    /// Read a file as a string.
    pub fn read_to_string(&self, path: &Path) -> Result<String> {
        let bytes = self.read(path)?;
        String::from_utf8(bytes).map_err(|e| anyhow::anyhow!("invalid UTF-8: {}", e))
    }

    /// Write to a file.
    pub fn write(&mut self, path: &Path, content: impl Into<Vec<u8>>) -> Result<()> {
        if let Some(parent) = path.parent() {
            if !self.dir_exists(parent) && !parent.as_os_str().is_empty() {
                bail!("parent directory does not exist: {}", parent.display());
            }
        }
        self.files.insert(path.to_path_buf(), content.into());
        Ok(())
    }

    /// Check if a file exists.
    pub fn exists(&self, path: &Path) -> bool {
        self.files.contains_key(path) || self.dirs.contains(&path.to_path_buf())
    }

    /// Check if a path is a file.
    pub fn is_file(&self, path: &Path) -> bool {
        self.files.contains_key(path)
    }

    /// Check if a path is a directory.
    pub fn is_dir(&self, path: &Path) -> bool {
        self.dirs.contains(&path.to_path_buf())
    }

    /// Check if a directory exists.
    pub fn dir_exists(&self, path: &Path) -> bool {
        self.dirs.contains(&path.to_path_buf())
    }

    /// Create a directory and all parent directories.
    pub fn create_dir_all(&mut self, path: impl AsRef<Path>) -> Result<()> {
        self.add_dir(path);
        Ok(())
    }

    /// Remove a file.
    pub fn remove_file(&mut self, path: &Path) -> Result<()> {
        if self.files.remove(path).is_none() {
            bail!("file not found: {}", path.display());
        }
        Ok(())
    }

    /// Remove a directory (must be empty).
    pub fn remove_dir(&mut self, path: &Path) -> Result<()> {
        // Check if directory contains any files
        for file_path in self.files.keys() {
            if file_path.starts_with(path) {
                bail!("directory not empty: {}", path.display());
            }
        }
        self.dirs.retain(|d| d != path && !d.starts_with(path));
        Ok(())
    }

    /// Remove a directory and all its contents.
    pub fn remove_dir_all(&mut self, path: &Path) -> Result<()> {
        self.files.retain(|p, _| !p.starts_with(path));
        self.dirs.retain(|d| !d.starts_with(path) && d != path);
        Ok(())
    }

    /// List files in a directory.
    pub fn list_files(&self, path: &Path) -> Vec<PathBuf> {
        self.files
            .keys()
            .filter(|p| p.parent() == Some(path))
            .cloned()
            .collect()
    }

    /// List subdirectories in a directory.
    pub fn list_dirs(&self, path: &Path) -> Vec<PathBuf> {
        self.dirs
            .iter()
            .filter(|d| d.parent() == Some(path))
            .cloned()
            .collect()
    }

    /// Get all files (for debugging).
    pub fn all_files(&self) -> &HashMap<PathBuf, Vec<u8>> {
        &self.files
    }

    /// Get all directories (for debugging).
    pub fn all_dirs(&self) -> &[PathBuf] {
        &self.dirs
    }
}

/// Mock process output for testing command execution.
#[derive(Debug, Clone)]
pub struct MockProcessOutput {
    /// Exit status code (0 = success).
    pub status: i32,
    /// Standard output.
    pub stdout: String,
    /// Standard error.
    pub stderr: String,
}

impl MockProcessOutput {
    /// Create a successful output with the given stdout.
    pub fn success(stdout: impl Into<String>) -> Self {
        MockProcessOutput {
            status: 0,
            stdout: stdout.into(),
            stderr: String::new(),
        }
    }

    /// Create a failure output with the given stderr and status code.
    pub fn failure(status: i32, stderr: impl Into<String>) -> Self {
        MockProcessOutput {
            status,
            stdout: String::new(),
            stderr: stderr.into(),
        }
    }

    /// Create an output with both stdout and stderr.
    pub fn with_output(status: i32, stdout: impl Into<String>, stderr: impl Into<String>) -> Self {
        MockProcessOutput {
            status,
            stdout: stdout.into(),
            stderr: stderr.into(),
        }
    }

    /// Check if the process succeeded.
    pub fn success_status(&self) -> bool {
        self.status == 0
    }
}

impl Default for MockProcessOutput {
    fn default() -> Self {
        MockProcessOutput::success("")
    }
}

/// Pattern for matching commands in MockExecutor.
#[derive(Debug, Clone)]
pub enum CommandPattern {
    /// Exact match on full command string.
    Exact(String),
    /// Match if command starts with prefix.
    StartsWith(String),
    /// Match if command contains substring.
    Contains(String),
    /// Match using a regex pattern.
    Regex(String),
    /// Match any command.
    Any,
}

impl CommandPattern {
    /// Check if this pattern matches the given command.
    pub fn matches(&self, cmd: &str) -> bool {
        match self {
            CommandPattern::Exact(s) => cmd == s,
            CommandPattern::StartsWith(s) => cmd.starts_with(s),
            CommandPattern::Contains(s) => cmd.contains(s),
            CommandPattern::Regex(pattern) => {
                regex::Regex::new(pattern)
                    .map(|re| re.is_match(cmd))
                    .unwrap_or(false)
            }
            CommandPattern::Any => true,
        }
    }
}

/// Expectation for a command execution.
#[derive(Debug, Clone)]
pub struct CommandExpectation {
    /// Pattern to match against commands.
    pub pattern: CommandPattern,
    /// Output to return when matched.
    pub output: MockProcessOutput,
    /// Number of times this expectation can be used (None = unlimited).
    pub times: Option<usize>,
    /// Number of times this expectation has been used.
    pub used: usize,
}

impl CommandExpectation {
    /// Create a new expectation.
    pub fn new(pattern: CommandPattern, output: MockProcessOutput) -> Self {
        CommandExpectation {
            pattern,
            output,
            times: None,
            used: 0,
        }
    }

    /// Set the number of times this expectation can be used.
    pub fn times(mut self, n: usize) -> Self {
        self.times = Some(n);
        self
    }

    /// Check if this expectation can still be used.
    pub fn available(&self) -> bool {
        match self.times {
            Some(n) => self.used < n,
            None => true,
        }
    }
}

/// Mock process executor for testing command execution.
///
/// Records expected commands and their outputs, and verifies that
/// commands are called as expected.
#[derive(Debug, Default)]
pub struct MockExecutor {
    expectations: Vec<CommandExpectation>,
    calls: Vec<String>,
    default_output: Option<MockProcessOutput>,
}

impl MockExecutor {
    /// Create a new mock executor.
    pub fn new() -> Self {
        MockExecutor {
            expectations: Vec::new(),
            calls: Vec::new(),
            default_output: None,
        }
    }

    /// Add an expectation for an exact command match.
    pub fn expect(&mut self, cmd: &str, output: MockProcessOutput) -> &mut Self {
        self.expectations.push(CommandExpectation::new(
            CommandPattern::Exact(cmd.to_string()),
            output,
        ));
        self
    }

    /// Add an expectation for a command starting with a prefix.
    pub fn expect_prefix(&mut self, prefix: &str, output: MockProcessOutput) -> &mut Self {
        self.expectations.push(CommandExpectation::new(
            CommandPattern::StartsWith(prefix.to_string()),
            output,
        ));
        self
    }

    /// Add an expectation for a command containing a substring.
    pub fn expect_contains(&mut self, substring: &str, output: MockProcessOutput) -> &mut Self {
        self.expectations.push(CommandExpectation::new(
            CommandPattern::Contains(substring.to_string()),
            output,
        ));
        self
    }

    /// Add a custom expectation.
    pub fn expect_pattern(&mut self, expectation: CommandExpectation) -> &mut Self {
        self.expectations.push(expectation);
        self
    }

    /// Set a default output for commands that don't match any expectation.
    pub fn set_default(&mut self, output: MockProcessOutput) -> &mut Self {
        self.default_output = Some(output);
        self
    }

    /// Execute a command and return the mock output.
    pub fn run(&mut self, cmd: &str, args: &[&str]) -> Result<MockProcessOutput> {
        let full_cmd = if args.is_empty() {
            cmd.to_string()
        } else {
            format!("{} {}", cmd, args.join(" "))
        };

        self.calls.push(full_cmd.clone());

        // Find matching expectation
        for exp in &mut self.expectations {
            if exp.pattern.matches(&full_cmd) && exp.available() {
                exp.used += 1;
                return Ok(exp.output.clone());
            }
        }

        // Use default if set
        if let Some(ref default) = self.default_output {
            return Ok(default.clone());
        }

        bail!("unexpected command: {}", full_cmd)
    }

    /// Get all commands that were called.
    pub fn calls(&self) -> &[String] {
        &self.calls
    }

    /// Clear all recorded calls.
    pub fn clear_calls(&mut self) {
        self.calls.clear();
    }

    /// Verify that all expectations with a specific count were satisfied.
    pub fn verify(&self) -> Result<()> {
        for (i, exp) in self.expectations.iter().enumerate() {
            if let Some(expected) = exp.times {
                if exp.used != expected {
                    bail!(
                        "expectation {} was used {} times, expected {}",
                        i,
                        exp.used,
                        expected
                    );
                }
            }
        }
        Ok(())
    }
}

/// Mock HTTP response for testing downloads.
#[derive(Debug, Clone)]
pub struct MockHttpResponse {
    /// HTTP status code.
    pub status: u16,
    /// Response headers.
    pub headers: HashMap<String, String>,
    /// Response body.
    pub body: Vec<u8>,
}

impl MockHttpResponse {
    /// Create a successful response with the given body.
    pub fn ok(body: impl Into<Vec<u8>>) -> Self {
        MockHttpResponse {
            status: 200,
            headers: HashMap::new(),
            body: body.into(),
        }
    }

    /// Create a not found response.
    pub fn not_found() -> Self {
        MockHttpResponse {
            status: 404,
            headers: HashMap::new(),
            body: b"Not Found".to_vec(),
        }
    }

    /// Create a server error response.
    pub fn server_error(message: &str) -> Self {
        MockHttpResponse {
            status: 500,
            headers: HashMap::new(),
            body: message.as_bytes().to_vec(),
        }
    }

    /// Add a header to the response.
    pub fn with_header(mut self, key: &str, value: &str) -> Self {
        self.headers.insert(key.to_string(), value.to_string());
        self
    }

    /// Check if this is a successful response.
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// Mock HTTP client for testing tarball downloads and API calls.
#[derive(Debug, Default)]
pub struct MockHttpClient {
    responses: HashMap<String, MockHttpResponse>,
    requests: Vec<String>,
    default_response: Option<MockHttpResponse>,
}

impl MockHttpClient {
    /// Create a new mock HTTP client.
    pub fn new() -> Self {
        MockHttpClient {
            responses: HashMap::new(),
            requests: Vec::new(),
            default_response: None,
        }
    }

    /// Add a response for a URL.
    pub fn mock_url(&mut self, url: &str, response: MockHttpResponse) -> &mut Self {
        self.responses.insert(url.to_string(), response);
        self
    }

    /// Set a default response for unmatched URLs.
    pub fn set_default(&mut self, response: MockHttpResponse) -> &mut Self {
        self.default_response = Some(response);
        self
    }

    /// Make a GET request.
    pub fn get(&mut self, url: &str) -> Result<MockHttpResponse> {
        self.requests.push(url.to_string());

        if let Some(response) = self.responses.get(url) {
            return Ok(response.clone());
        }

        // Try prefix matching
        for (pattern, response) in &self.responses {
            if url.starts_with(pattern) {
                return Ok(response.clone());
            }
        }

        if let Some(ref default) = self.default_response {
            return Ok(default.clone());
        }

        bail!("no mock response for URL: {}", url)
    }

    /// Download content to a writer.
    pub fn download(&mut self, url: &str, writer: &mut impl Write) -> Result<u64> {
        let response = self.get(url)?;
        if !response.is_success() {
            bail!("HTTP error {}: {}", response.status, url);
        }
        let len = response.body.len() as u64;
        writer.write_all(&response.body)?;
        Ok(len)
    }

    /// Get all requested URLs.
    pub fn requests(&self) -> &[String] {
        &self.requests
    }

    /// Clear request history.
    pub fn clear_requests(&mut self) {
        self.requests.clear();
    }
}

/// Helper to create a temporary test workspace with a Harbor.toml.
///
/// Returns the TempDir handle - dropping it will clean up the directory.
pub fn create_test_workspace(_name: &str, manifest: &str) -> tempfile::TempDir {
    let tmp = tempfile::TempDir::new().expect("failed to create temp dir");
    let manifest_path = tmp.path().join("Harbor.toml");
    std::fs::write(&manifest_path, manifest).expect("failed to write manifest");

    // Create default source directory
    let src_dir = tmp.path().join("src");
    std::fs::create_dir_all(&src_dir).expect("failed to create src dir");

    tmp
}

/// Helper to create a minimal Harbor.toml content.
pub fn minimal_manifest(name: &str) -> String {
    format!(
        r#"[package]
name = "{}"
version = "0.1.0"

[targets.{}]
kind = "staticlib"
sources = ["src/**/*.c"]
"#,
        name, name
    )
}

/// Helper to create an executable manifest.
pub fn exe_manifest(name: &str) -> String {
    format!(
        r#"[package]
name = "{}"
version = "0.1.0"

[targets.{}]
kind = "exe"
sources = ["src/**/*.c"]
"#,
        name, name
    )
}

/// Helper to create a library manifest with dependencies.
pub fn lib_manifest_with_deps(name: &str, deps: &[(&str, &str)]) -> String {
    let mut manifest = format!(
        r#"[package]
name = "{}"
version = "0.1.0"

"#,
        name
    );

    if !deps.is_empty() {
        manifest.push_str("[dependencies]\n");
        for (dep_name, spec) in deps {
            manifest.push_str(&format!("{} = {}\n", dep_name, spec));
        }
        manifest.push('\n');
    }

    manifest.push_str(&format!(
        r#"[targets.{}]
kind = "staticlib"
sources = ["src/**/*.c"]
"#,
        name
    ));

    manifest
}

/// Create a minimal C source file.
pub fn minimal_c_source() -> &'static str {
    r#"// Minimal C source
int placeholder_func(void) {
    return 0;
}
"#
}

/// Create a minimal main.c for executables.
pub fn minimal_main_c() -> &'static str {
    r#"#include <stdio.h>

int main(void) {
    printf("Hello from test!\n");
    return 0;
}
"#
}

/// Create a minimal header file.
pub fn minimal_header(name: &str) -> String {
    let guard = name.to_uppercase().replace('-', "_");
    format!(
        r#"#ifndef {}_H
#define {}_H

// Public API for {}

#endif // {}_H
"#,
        guard, guard, name, guard
    )
}

/// Trait for injectable dependencies in tests.
///
/// Types implementing this trait can have their behavior swapped
/// for testing purposes.
pub trait Injectable: Send + Sync {
    /// Get the type name for debugging.
    fn type_name(&self) -> &'static str;
}

/// Thread-local test context for injecting mocks.
///
/// This allows tests to inject mock implementations without
/// changing function signatures.
pub struct TestContext {
    filesystem: Option<Arc<Mutex<MockFileSystem>>>,
    executor: Option<Arc<Mutex<MockExecutor>>>,
    http_client: Option<Arc<Mutex<MockHttpClient>>>,
}

impl TestContext {
    /// Create a new empty test context.
    pub fn new() -> Self {
        TestContext {
            filesystem: None,
            executor: None,
            http_client: None,
        }
    }

    /// Set the mock filesystem.
    pub fn with_filesystem(mut self, fs: MockFileSystem) -> Self {
        self.filesystem = Some(Arc::new(Mutex::new(fs)));
        self
    }

    /// Set the mock executor.
    pub fn with_executor(mut self, exec: MockExecutor) -> Self {
        self.executor = Some(Arc::new(Mutex::new(exec)));
        self
    }

    /// Set the mock HTTP client.
    pub fn with_http_client(mut self, client: MockHttpClient) -> Self {
        self.http_client = Some(Arc::new(Mutex::new(client)));
        self
    }

    /// Get the mock filesystem if set.
    pub fn filesystem(&self) -> Option<Arc<Mutex<MockFileSystem>>> {
        self.filesystem.clone()
    }

    /// Get the mock executor if set.
    pub fn executor(&self) -> Option<Arc<Mutex<MockExecutor>>> {
        self.executor.clone()
    }

    /// Get the mock HTTP client if set.
    pub fn http_client(&self) -> Option<Arc<Mutex<MockHttpClient>>> {
        self.http_client.clone()
    }
}

impl Default for TestContext {
    fn default() -> Self {
        Self::new()
    }
}

/// Assertion helpers for testing.
pub mod assertions {
    use super::*;

    /// Assert that a result is Ok and return the value.
    pub fn assert_ok<T, E: std::fmt::Debug>(result: Result<T, E>) -> T {
        match result {
            Ok(v) => v,
            Err(e) => panic!("expected Ok, got Err: {:?}", e),
        }
    }

    /// Assert that a result is Err and return the error.
    pub fn assert_err<T: std::fmt::Debug, E>(result: Result<T, E>) -> E {
        match result {
            Ok(v) => panic!("expected Err, got Ok: {:?}", v),
            Err(e) => e,
        }
    }

    /// Assert that an error message contains a substring.
    pub fn assert_error_contains<T: std::fmt::Debug>(
        result: Result<T, anyhow::Error>,
        substring: &str,
    ) {
        match result {
            Ok(v) => panic!("expected Err containing '{}', got Ok: {:?}", substring, v),
            Err(e) => {
                let msg = e.to_string();
                assert!(
                    msg.contains(substring),
                    "error '{}' does not contain '{}'",
                    msg,
                    substring
                );
            }
        }
    }

    /// Assert that a path exists in the mock filesystem.
    pub fn assert_path_exists(fs: &MockFileSystem, path: impl AsRef<Path>) {
        let path = path.as_ref();
        assert!(
            fs.exists(path),
            "expected path to exist: {}",
            path.display()
        );
    }

    /// Assert that a file contains specific content.
    pub fn assert_file_contains(fs: &MockFileSystem, path: impl AsRef<Path>, content: &str) {
        let path = path.as_ref();
        let actual = fs
            .read_to_string(path)
            .unwrap_or_else(|_| panic!("file not found: {}", path.display()));
        assert!(
            actual.contains(content),
            "file {} does not contain '{}'\nactual content:\n{}",
            path.display(),
            content,
            actual
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mock_filesystem_basic() {
        let mut fs = MockFileSystem::new();

        fs.add_dir("/project");
        fs.add_file("/project/Harbor.toml", b"[package]\nname = \"test\"");

        assert!(fs.exists(Path::new("/project")));
        assert!(fs.exists(Path::new("/project/Harbor.toml")));
        assert!(!fs.exists(Path::new("/project/nonexistent")));

        let content = fs.read_to_string(Path::new("/project/Harbor.toml")).unwrap();
        assert!(content.contains("name = \"test\""));
    }

    #[test]
    fn test_mock_filesystem_directories() {
        let mut fs = MockFileSystem::new();

        fs.add_dir("/a/b/c");
        assert!(fs.is_dir(Path::new("/a")));
        assert!(fs.is_dir(Path::new("/a/b")));
        assert!(fs.is_dir(Path::new("/a/b/c")));
    }

    #[test]
    fn test_mock_executor_basic() {
        let mut exec = MockExecutor::new();

        exec.expect("gcc --version", MockProcessOutput::success("gcc 12.0.0"));
        exec.expect_prefix("cmake", MockProcessOutput::success("cmake output"));

        let result = exec.run("gcc", &["--version"]).unwrap();
        assert!(result.success_status());
        assert_eq!(result.stdout, "gcc 12.0.0");

        let result = exec.run("cmake", &["-G", "Ninja"]).unwrap();
        assert!(result.success_status());
    }

    #[test]
    fn test_mock_executor_unexpected() {
        let mut exec = MockExecutor::new();

        let result = exec.run("unknown", &[]);
        assert!(result.is_err());
    }

    #[test]
    fn test_mock_http_client() {
        let mut client = MockHttpClient::new();

        client.mock_url(
            "https://example.com/file.tar.gz",
            MockHttpResponse::ok(b"tarball content"),
        );

        let response = client.get("https://example.com/file.tar.gz").unwrap();
        assert!(response.is_success());
        assert_eq!(response.body, b"tarball content");
    }

    #[test]
    fn test_create_test_workspace() {
        let manifest = minimal_manifest("testpkg");
        let workspace = create_test_workspace("testpkg", &manifest);

        assert!(workspace.path().join("Harbor.toml").exists());
        assert!(workspace.path().join("src").exists());
    }

    #[test]
    fn test_minimal_manifest() {
        let manifest = minimal_manifest("mylib");
        assert!(manifest.contains("name = \"mylib\""));
        assert!(manifest.contains("version = \"0.1.0\""));
        assert!(manifest.contains("kind = \"staticlib\""));
    }

    #[test]
    fn test_assertions() {
        use assertions::*;

        let ok_result: Result<i32, &str> = Ok(42);
        assert_eq!(assert_ok(ok_result), 42);

        let err_result: Result<i32, &str> = Err("error");
        assert_eq!(assert_err(err_result), "error");
    }
}
