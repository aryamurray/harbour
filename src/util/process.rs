//! Subprocess execution utilities.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output, Stdio};

use anyhow::{bail, Context, Result};

/// Builder for subprocess execution.
#[derive(Debug, Clone)]
pub struct ProcessBuilder {
    program: PathBuf,
    args: Vec<String>,
    env: HashMap<String, String>,
    env_remove: Vec<String>,
    cwd: Option<PathBuf>,
    stdin: Option<Vec<u8>>,
}

impl ProcessBuilder {
    /// Create a new process builder for the given program.
    pub fn new(program: impl AsRef<Path>) -> Self {
        ProcessBuilder {
            program: program.as_ref().to_path_buf(),
            args: Vec::new(),
            env: HashMap::new(),
            env_remove: Vec::new(),
            cwd: None,
            stdin: None,
        }
    }

    /// Add a single argument.
    pub fn arg(mut self, arg: impl AsRef<OsStr>) -> Self {
        self.args.push(arg.as_ref().to_string_lossy().into_owned());
        self
    }

    /// Add multiple arguments.
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args.extend(
            args.into_iter()
                .map(|s| s.as_ref().to_string_lossy().into_owned()),
        );
        self
    }

    /// Set an environment variable.
    pub fn env(mut self, key: impl AsRef<str>, value: impl AsRef<str>) -> Self {
        self.env
            .insert(key.as_ref().to_string(), value.as_ref().to_string());
        self
    }

    /// Remove an environment variable.
    pub fn env_remove(mut self, key: impl AsRef<str>) -> Self {
        self.env_remove.push(key.as_ref().to_string());
        self
    }

    /// Set the working directory.
    pub fn cwd(mut self, cwd: impl AsRef<Path>) -> Self {
        self.cwd = Some(cwd.as_ref().to_path_buf());
        self
    }

    /// Set stdin data.
    pub fn stdin(mut self, data: impl Into<Vec<u8>>) -> Self {
        self.stdin = Some(data.into());
        self
    }

    /// Get the program path.
    pub fn get_program(&self) -> &Path {
        &self.program
    }

    /// Get the arguments.
    pub fn get_args(&self) -> &[String] {
        &self.args
    }

    /// Build the Command.
    fn build_command(&self) -> Command {
        let mut cmd = Command::new(&self.program);
        cmd.args(&self.args);

        for (key, value) in &self.env {
            cmd.env(key, value);
        }

        for key in &self.env_remove {
            cmd.env_remove(key);
        }

        if let Some(ref cwd) = self.cwd {
            cmd.current_dir(cwd);
        }

        cmd
    }

    /// Execute the command and wait for completion.
    pub fn exec(&self) -> Result<Output> {
        let mut cmd = self.build_command();

        if self.stdin.is_some() {
            cmd.stdin(Stdio::piped());
        }
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let mut child = cmd
            .spawn()
            .with_context(|| format!("failed to spawn `{}`", self.program.display()))?;

        if let Some(ref stdin_data) = self.stdin {
            use std::io::Write;
            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(stdin_data)?;
            }
        }

        let output = child
            .wait_with_output()
            .with_context(|| format!("failed to wait for `{}`", self.program.display()))?;

        Ok(output)
    }

    /// Execute and require success.
    pub fn exec_and_check(&self) -> Result<Output> {
        let output = self.exec()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!(
                "`{}` failed with exit code {:?}\n{}",
                self.display_command(),
                output.status.code(),
                stderr
            );
        }
        Ok(output)
    }

    /// Execute and return status only.
    pub fn status(&self) -> Result<ExitStatus> {
        let mut cmd = self.build_command();
        let status = cmd
            .status()
            .with_context(|| format!("failed to execute `{}`", self.program.display()))?;
        Ok(status)
    }

    /// Display the command for error messages.
    pub fn display_command(&self) -> String {
        let mut parts = vec![self.program.display().to_string()];
        parts.extend(self.args.iter().cloned());
        parts.join(" ")
    }
}

/// Find an executable in PATH.
pub fn find_executable(name: &str) -> Option<PathBuf> {
    which::which(name).ok()
}

/// Find a C compiler.
pub fn find_c_compiler() -> Option<PathBuf> {
    // Check CC environment variable first
    if let Ok(cc) = std::env::var("CC") {
        if let Some(path) = find_executable(&cc) {
            return Some(path);
        }
    }

    // Try common compilers
    for compiler in &["cc", "gcc", "clang", "cl"] {
        if let Some(path) = find_executable(compiler) {
            return Some(path);
        }
    }

    None
}

/// Find the ar archiver.
pub fn find_ar() -> Option<PathBuf> {
    // Check AR environment variable first
    if let Ok(ar) = std::env::var("AR") {
        if let Some(path) = find_executable(&ar) {
            return Some(path);
        }
    }

    // Try common archivers
    for archiver in &["ar", "llvm-ar", "lib"] {
        if let Some(path) = find_executable(archiver) {
            return Some(path);
        }
    }

    None
}

/// Find CMake.
pub fn find_cmake() -> Option<PathBuf> {
    find_executable("cmake")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_process_builder() {
        let output = ProcessBuilder::new("echo").arg("hello").exec().unwrap();

        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.trim() == "hello" || stdout.contains("hello"));
    }

    #[test]
    fn test_display_command() {
        let pb = ProcessBuilder::new("gcc").args(["-Wall", "-o", "output", "input.c"]);

        assert_eq!(pb.display_command(), "gcc -Wall -o output input.c");
    }
}
