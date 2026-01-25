//! Centralized shell output and progress management.
//!
//! The Shell module provides a unified API for all CLI output, including:
//! - Status messages with consistent formatting
//! - Progress bars (via indicatif)
//! - Scoped timing spans with delayed start
//! - JSON output mode for machine-readable output
//!
//! # Design Principles
//!
//! 1. **Commands never manage spacing/indentation directly** - Shell handles all formatting
//! 2. **JSON mode is mutually exclusive** - No human output when JSON mode is enabled
//! 3. **Spans have delayed start** - Start message only printed after 200ms or on first progress
//! 4. **Timing is always shown** - End messages include duration unless in quiet mode

use std::fmt::Display;
use std::io::{self, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use indicatif::{ProgressBar, ProgressStyle};

/// Shell output mode - Human and Json are mutually exclusive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellMode {
    /// Human-readable output with optional colors and progress bars.
    Human {
        verbosity: Verbosity,
        color: ColorChoice,
    },
    /// Machine-readable JSON output only.
    Json,
}

impl Default for ShellMode {
    fn default() -> Self {
        ShellMode::Human {
            verbosity: Verbosity::Normal,
            color: ColorChoice::Auto,
        }
    }
}

/// Output verbosity level (Human mode only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Verbosity {
    /// --quiet: errors only, no progress
    Quiet,
    /// Default: status messages + progress bars
    #[default]
    Normal,
    /// --verbose: immediate status lines, debug info, no progress bars
    Verbose,
}

/// Color output mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorChoice {
    /// Detect TTY and use colors if available.
    #[default]
    Auto,
    /// Always use ANSI colors.
    Always,
    /// Never use ANSI colors.
    Never,
}

impl std::str::FromStr for ColorChoice {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "auto" => Ok(ColorChoice::Auto),
            "always" => Ok(ColorChoice::Always),
            "never" => Ok(ColorChoice::Never),
            _ => Err(format!(
                "invalid color choice '{}'; expected 'auto', 'always', or 'never'",
                s
            )),
        }
    }
}

/// Status types for output messages.
///
/// Shell handles all formatting - callers just specify the semantic status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    // Success statuses (green)
    Added,
    Created,
    Finished,
    Updated,
    Removed,

    // In-progress statuses (cyan)
    Compiling,
    Fetching,
    Resolving,
    Linking,
    Building,

    // Info statuses (blue/default)
    Info,

    // Warning statuses (yellow)
    Skipped,
    Warning,

    // Error status (red)
    Error,
}

impl Status {
    /// Get the display text for this status.
    fn as_str(&self) -> &'static str {
        match self {
            Status::Added => "Added",
            Status::Created => "Created",
            Status::Finished => "Finished",
            Status::Updated => "Updated",
            Status::Removed => "Removed",
            Status::Compiling => "Compiling",
            Status::Fetching => "Fetching",
            Status::Resolving => "Resolving",
            Status::Linking => "Linking",
            Status::Building => "Building",
            Status::Info => "Info",
            Status::Skipped => "Skipped",
            Status::Warning => "Warning",
            Status::Error => "error",
        }
    }

    /// Get the ANSI color code for this status.
    fn color_code(&self) -> &'static str {
        match self {
            // Success: bold green
            Status::Added | Status::Created | Status::Finished | Status::Updated | Status::Removed => {
                "\x1b[1;32m"
            }
            // In-progress: bold cyan
            Status::Compiling | Status::Fetching | Status::Resolving | Status::Linking | Status::Building => {
                "\x1b[1;36m"
            }
            // Info: bold blue
            Status::Info => "\x1b[1;34m",
            // Warning: bold yellow
            Status::Skipped | Status::Warning => "\x1b[1;33m",
            // Error: bold red
            Status::Error => "\x1b[1;31m",
        }
    }

    /// Get the width for alignment (12 characters).
    fn width(&self) -> usize {
        12
    }
}

/// Central shell for all CLI output.
#[derive(Debug)]
pub struct Shell {
    mode: ShellMode,
    use_color: bool,
    /// JSON output buffer for machine-readable mode
    json_output: Mutex<Vec<String>>,
    /// Whether we've printed anything (for newline management)
    has_output: AtomicBool,
}

impl Shell {
    /// Create a new shell with the given mode.
    pub fn new(mode: ShellMode) -> Self {
        let use_color = match &mode {
            ShellMode::Json => false,
            ShellMode::Human { color, .. } => match color {
                ColorChoice::Auto => io::stderr().is_terminal(),
                ColorChoice::Always => true,
                ColorChoice::Never => false,
            },
        };

        Shell {
            mode,
            use_color,
            json_output: Mutex::new(Vec::new()),
            has_output: AtomicBool::new(false),
        }
    }

    /// Create a shell from CLI flags with proper precedence.
    ///
    /// JSON mode takes precedence over quiet/verbose.
    pub fn from_flags(
        quiet: bool,
        verbose: bool,
        color: ColorChoice,
        message_format_json: bool,
    ) -> Self {
        let mode = if message_format_json {
            ShellMode::Json
        } else {
            let verbosity = if quiet {
                Verbosity::Quiet
            } else if verbose {
                Verbosity::Verbose
            } else {
                Verbosity::Normal
            };
            ShellMode::Human {
                verbosity,
                color,
            }
        };

        Shell::new(mode)
    }

    /// Get the current shell mode.
    pub fn mode(&self) -> &ShellMode {
        &self.mode
    }

    /// Check if shell is in quiet mode.
    pub fn is_quiet(&self) -> bool {
        matches!(
            self.mode,
            ShellMode::Human {
                verbosity: Verbosity::Quiet,
                ..
            }
        )
    }

    /// Check if shell is in verbose mode.
    pub fn is_verbose(&self) -> bool {
        matches!(
            self.mode,
            ShellMode::Human {
                verbosity: Verbosity::Verbose,
                ..
            }
        )
    }

    /// Check if shell is in JSON mode.
    pub fn is_json(&self) -> bool {
        matches!(self.mode, ShellMode::Json)
    }

    /// Check if colors are enabled.
    pub fn use_color(&self) -> bool {
        self.use_color
    }

    /// Print a status message.
    ///
    /// Format: `{status:>12} {message}`
    ///
    /// In quiet mode, only Error status is printed.
    /// In JSON mode, messages are silently ignored (use json_event for JSON output).
    pub fn status(&self, status: Status, msg: impl Display) {
        if self.is_json() {
            return;
        }

        if self.is_quiet() && status != Status::Error {
            return;
        }

        let prefix = self.format_status(status);
        eprintln!("{} {}", prefix, msg);
        self.has_output.store(true, Ordering::SeqCst);
    }

    /// Print an info message.
    pub fn note(&self, msg: impl Display) {
        self.status(Status::Info, msg);
    }

    /// Print a warning message.
    pub fn warn(&self, msg: impl Display) {
        self.status(Status::Warning, msg);
    }

    /// Print an error message.
    ///
    /// In JSON mode, this outputs a JSON error event.
    pub fn error(&self, msg: impl Display) {
        if self.is_json() {
            let event = serde_json::json!({
                "reason": "error",
                "message": msg.to_string()
            });
            self.json_event(&event);
        } else {
            self.status(Status::Error, msg);
        }
    }

    /// Print a JSON event to stdout.
    ///
    /// Only works in JSON mode; silently ignored in human mode.
    pub fn json_event(&self, event: &serde_json::Value) {
        if !self.is_json() {
            return;
        }

        let json_str = serde_json::to_string(event).unwrap_or_default();
        println!("{}", json_str);
        let _ = io::stdout().flush();

        // Also store for potential batch retrieval
        if let Ok(mut buffer) = self.json_output.lock() {
            buffer.push(json_str);
        }
    }

    /// Format a status prefix with optional color.
    fn format_status(&self, status: Status) -> String {
        let text = status.as_str();
        let width = status.width();

        if self.use_color {
            let color = status.color_code();
            format!("{}{:>width$}\x1b[0m", color, text, width = width)
        } else {
            format!("{:>width$}", text, width = width)
        }
    }

    /// Create a scoped span for timing operations.
    ///
    /// The span has a delayed start (200ms by default). The start message is only
    /// printed if the operation takes longer than the delay. The end message with
    /// timing is always printed (unless in quiet mode).
    pub fn span(self: &Arc<Self>, status: Status, msg: impl Display) -> Span {
        Span::new(Arc::clone(self), status, msg.to_string())
    }

    /// Create a progress bar.
    ///
    /// In quiet or verbose mode, returns a no-op progress bar.
    /// In JSON mode, progress updates are emitted as JSON events.
    pub fn progress(self: &Arc<Self>, total: u64, msg: impl Display) -> Progress {
        Progress::new(Arc::clone(self), total, msg.to_string(), false)
    }

    /// Create a byte-based progress bar for downloads.
    pub fn bytes_progress(self: &Arc<Self>, msg: impl Display, total_bytes: u64) -> Progress {
        Progress::new(Arc::clone(self), total_bytes, msg.to_string(), true)
    }
}

impl Default for Shell {
    fn default() -> Self {
        Shell::new(ShellMode::default())
    }
}

/// A scoped timing span with delayed start output.
///
/// The start message is only printed after a delay (default 200ms) or when
/// explicitly triggered. The end message with duration is always printed on drop.
pub struct Span {
    shell: Arc<Shell>,
    status: Status,
    message: String,
    start: Instant,
    start_printed: bool,
    delay: Duration,
    finished: bool,
}

impl Span {
    /// Default delay before printing start message.
    const DEFAULT_DELAY: Duration = Duration::from_millis(200);

    /// Create a new span.
    fn new(shell: Arc<Shell>, status: Status, message: String) -> Self {
        let start_printed = shell.is_verbose();

        // In verbose mode, print start immediately
        if start_printed && !shell.is_quiet() && !shell.is_json() {
            shell.status(status, &message);
        }

        Span {
            shell,
            status,
            message,
            start: Instant::now(),
            start_printed,
            delay: Self::DEFAULT_DELAY,
            finished: false,
        }
    }

    /// Check if we should print the start message and do so if needed.
    pub fn maybe_print_start(&mut self) {
        if !self.start_printed && self.start.elapsed() > self.delay {
            self.print_start();
        }
    }

    /// Force print the start message.
    fn print_start(&mut self) {
        if !self.start_printed && !self.shell.is_quiet() && !self.shell.is_json() {
            self.shell.status(self.status, &self.message);
            self.start_printed = true;
        }
    }

    /// Mark the span as finished with a custom message.
    pub fn finish_with_message(mut self, msg: impl Display) {
        self.finished = true;
        let elapsed = self.start.elapsed();

        if self.shell.is_json() {
            return;
        }

        if !self.shell.is_quiet() {
            let duration_str = format_duration(elapsed);
            self.shell.status(Status::Finished, format!("{} in {}", msg, duration_str));
        }
    }

    /// Get elapsed time.
    pub fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        if self.finished {
            return;
        }

        let elapsed = self.start.elapsed();

        if self.shell.is_json() {
            return;
        }

        if !self.shell.is_quiet() {
            let duration_str = format_duration(elapsed);
            // Only print if we started or took significant time
            if self.start_printed || elapsed > Self::DEFAULT_DELAY {
                self.shell.status(Status::Finished, format!("in {}", duration_str));
            }
        }
    }
}

/// Progress bar wrapper that respects shell mode.
pub struct Progress {
    shell: Arc<Shell>,
    pb: Option<ProgressBar>,
    total: u64,
    current: u64,
    message: String,
    is_bytes: bool,
}

impl Progress {
    /// Create a new progress bar.
    fn new(shell: Arc<Shell>, total: u64, message: String, is_bytes: bool) -> Self {
        let pb = if shell.is_quiet() || shell.is_verbose() || shell.is_json() {
            None
        } else if total > 1 {
            let pb = ProgressBar::new(total);
            let template = if is_bytes {
                "{spinner:.green} {msg} [{bar:40.cyan/blue}] {bytes}/{total_bytes}"
            } else {
                "{spinner:.green} {msg} [{bar:40.cyan/blue}] {pos}/{len}"
            };
            pb.set_style(
                ProgressStyle::default_bar()
                    .template(template)
                    .unwrap()
                    .progress_chars("#>-"),
            );
            pb.set_message(message.clone());
            Some(pb)
        } else {
            None
        };

        Progress {
            shell,
            pb,
            total,
            current: 0,
            message,
            is_bytes,
        }
    }

    /// Increment progress by one.
    pub fn inc(&mut self, delta: u64) {
        self.current += delta;

        if let Some(pb) = &self.pb {
            pb.inc(delta);
        }

        // In JSON mode, emit progress event
        if self.shell.is_json() {
            let event = serde_json::json!({
                "reason": "build-progress",
                "current": self.current,
                "total": self.total,
                "unit": if self.is_bytes { "bytes" } else { "items" },
                "message": self.message
            });
            self.shell.json_event(&event);
        }

        // In verbose mode, print raw lines
        if self.shell.is_verbose() && !self.shell.is_json() {
            eprintln!("  {} [{}/{}]", self.message, self.current, self.total);
        }
    }

    /// Set the current position.
    pub fn set_position(&mut self, pos: u64) {
        self.current = pos;
        if let Some(pb) = &self.pb {
            pb.set_position(pos);
        }
    }

    /// Finish the progress bar.
    pub fn finish(&self) {
        if let Some(pb) = &self.pb {
            pb.finish_and_clear();
        }
    }

    /// Finish with a message.
    pub fn finish_with_message(&self, msg: impl Display) {
        if let Some(pb) = &self.pb {
            pb.finish_with_message(msg.to_string());
        }
    }

    /// Get the current position.
    pub fn position(&self) -> u64 {
        self.current
    }

    /// Get the total.
    pub fn total(&self) -> u64 {
        self.total
    }
}

/// Format a duration in a human-readable way.
fn format_duration(duration: Duration) -> String {
    let secs = duration.as_secs_f64();
    if secs < 1.0 {
        format!("{:.2}s", secs)
    } else if secs < 60.0 {
        format!("{:.2}s", secs)
    } else {
        let mins = secs / 60.0;
        format!("{:.1}m", mins)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shell_modes() {
        let shell = Shell::new(ShellMode::Human {
            verbosity: Verbosity::Normal,
            color: ColorChoice::Never,
        });
        assert!(!shell.is_quiet());
        assert!(!shell.is_verbose());
        assert!(!shell.is_json());

        let quiet_shell = Shell::new(ShellMode::Human {
            verbosity: Verbosity::Quiet,
            color: ColorChoice::Never,
        });
        assert!(quiet_shell.is_quiet());

        let json_shell = Shell::new(ShellMode::Json);
        assert!(json_shell.is_json());
    }

    #[test]
    fn test_color_choice_parse() {
        assert_eq!("auto".parse::<ColorChoice>().unwrap(), ColorChoice::Auto);
        assert_eq!("always".parse::<ColorChoice>().unwrap(), ColorChoice::Always);
        assert_eq!("never".parse::<ColorChoice>().unwrap(), ColorChoice::Never);
        assert!("invalid".parse::<ColorChoice>().is_err());
    }

    #[test]
    fn test_format_duration() {
        assert_eq!(format_duration(Duration::from_millis(500)), "0.50s");
        assert_eq!(format_duration(Duration::from_secs(2)), "2.00s");
        assert_eq!(format_duration(Duration::from_secs(90)), "1.5m");
    }

    #[test]
    fn test_status_formatting() {
        let shell = Shell::new(ShellMode::Human {
            verbosity: Verbosity::Normal,
            color: ColorChoice::Never,
        });

        let formatted = shell.format_status(Status::Added);
        assert_eq!(formatted.trim(), "Added");
        assert_eq!(formatted.len(), 12); // Right-aligned to 12 chars
    }

    #[test]
    fn test_from_flags() {
        let shell = Shell::from_flags(false, false, ColorChoice::Auto, false);
        assert!(!shell.is_quiet());
        assert!(!shell.is_verbose());
        assert!(!shell.is_json());

        let shell = Shell::from_flags(true, false, ColorChoice::Auto, false);
        assert!(shell.is_quiet());

        let shell = Shell::from_flags(false, true, ColorChoice::Auto, false);
        assert!(shell.is_verbose());

        // JSON takes precedence
        let shell = Shell::from_flags(true, true, ColorChoice::Auto, true);
        assert!(shell.is_json());
        assert!(!shell.is_quiet()); // JSON mode, not quiet
    }
}
