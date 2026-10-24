//! Rendering errors for the terminal.
//!
//! Harbour keeps `anyhow` internally: almost every fallible function returns
//! `anyhow::Result` and builds its message with `bail!`, including the
//! `hint:`/`help:`/`note:` lines that its tests assert on. This module is the
//! one place those errors become output.
//!
//! - An error anywhere in the chain that is a [`miette::Diagnostic`] with source
//!   (today: [`ManifestParseError`]) is rendered with miette's graphical
//!   handler: the offending text, underlined, with a label and help.
//! - Every other error prints `error: <message>` and, when it has causes, a
//!   `Caused by:` list in place of the `a: b: c` flattening `{:#}` produced.
//!
//! Output depends on where it is going. A terminal gets unicode box-drawing and
//! colour; a pipe, a file or a CI log gets neither, and is not word-wrapped, so
//! that a message a test or a grep looks for is never split across lines.

use std::error::Error as StdError;
use std::fmt::Write as _;
use std::io::IsTerminal;
use std::path::Path;
use std::sync::OnceLock;

use miette::{
    Diagnostic, GraphicalReportHandler, GraphicalTheme, LabeledSpan, NamedSource, SourceSpan,
};
use thiserror::Error;

use super::ColorChoice;

/// A manifest that is not valid TOML, or does not match the schema.
///
/// Carries the manifest's text and the byte range the `toml` crate blamed, so
/// the offending key or value is underlined rather than pointed at by a caret
/// at the start of its line.
#[derive(Debug, Error)]
#[error("failed to parse {display_path}:{line}:{column}: {message}")]
pub struct ManifestParseError {
    display_path: String,
    line: usize,
    column: usize,
    message: String,
    help: Option<String>,
    source_code: NamedSource<String>,
    span: SourceSpan,
}

impl ManifestParseError {
    /// Build the diagnostic for a `toml` error against `content`.
    ///
    /// Returns `None` when the parser gave no span: there is nothing to
    /// underline, and the caller falls back to a plain message.
    pub fn new(err: &toml::de::Error, content: &str, path: &Path) -> Option<Self> {
        let range = err.span()?;
        let display_path = display_path(path);

        // `unknown field `x`, expected one of `a`, `b`` is one fact and a list.
        // The fact is the message; the list is what to do about it.
        let full = err.message().to_string();
        let (message, help) = match full.split_once(", expected ") {
            Some((short, expected)) => (short.to_string(), Some(format!("expected {expected}"))),
            None => (full, None),
        };

        // 1-based, for the one-line form (`{}`) that has no snippet.
        let before = &content[..range.start.min(content.len())];
        let line = before.matches('\n').count() + 1;
        let column = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;

        // A zero-width span would underline nothing.
        let len = range.end.saturating_sub(range.start).max(1);
        Some(Self {
            line,
            column,
            source_code: NamedSource::new(display_path.clone(), content.to_string()),
            display_path,
            message,
            help,
            span: SourceSpan::new(range.start.into(), len),
        })
    }
}

impl Diagnostic for ManifestParseError {
    fn source_code(&self) -> Option<&dyn miette::SourceCode> {
        Some(&self.source_code)
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = LabeledSpan> + '_>> {
        // Underline only. The message is the headline, and saying it twice --
        // once above the snippet and once beside it -- is noise.
        Some(Box::new(std::iter::once(LabeledSpan::new_with_span(
            None, self.span,
        ))))
    }

    fn help<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        self.help
            .as_ref()
            .map(|h| Box::new(h) as Box<dyn std::fmt::Display + 'a>)
    }
}

/// A path as the user would type it: relative to the current directory when it
/// is under it, otherwise as given.
pub fn display_path(path: &Path) -> String {
    std::env::current_dir()
        .ok()
        .and_then(|cwd| path.strip_prefix(&cwd).ok().map(Path::to_path_buf))
        .filter(|rel| !rel.as_os_str().is_empty())
        .unwrap_or_else(|| path.to_path_buf())
        .display()
        .to_string()
}

/// How an error should be drawn, decided once from where stderr goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    /// ANSI colour.
    pub color: bool,
    /// Unicode box-drawing, instead of ASCII.
    pub unicode: bool,
    /// Word-wrap messages to the terminal width.
    pub wrap: bool,
}

impl Style {
    /// Nothing but text: no ANSI, ASCII only, never wrapped.
    pub const PLAIN: Style = Style {
        color: false,
        unicode: false,
        wrap: false,
    };

    /// Decide from the `--color` choice, `NO_COLOR`, and whether stderr is a
    /// terminal.
    pub fn detect(choice: ColorChoice) -> Style {
        let terminal = std::io::stderr().is_terminal();
        let dumb = std::env::var("TERM").is_ok_and(|t| t == "dumb");
        let no_color = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
        let color = match choice {
            ColorChoice::Always => true,
            ColorChoice::Never => false,
            ColorChoice::Auto => terminal && !dumb && !no_color,
        };
        // The legacy Windows console draws box characters as question marks;
        // Windows Terminal (which sets WT_SESSION) does not.
        let unicode_capable = cfg!(not(windows)) || std::env::var_os("WT_SESSION").is_some();
        Style {
            color,
            unicode: terminal && !dumb && unicode_capable,
            wrap: terminal,
        }
    }

    fn theme(self) -> GraphicalTheme {
        match (self.color, self.unicode) {
            (true, true) => GraphicalTheme::unicode(),
            (false, true) => GraphicalTheme::unicode_nocolor(),
            (true, false) => GraphicalTheme::ascii(),
            (false, false) => GraphicalTheme::none(),
        }
    }

    fn handler(self) -> GraphicalReportHandler {
        let mut handler = GraphicalReportHandler::new_themed(self.theme());
        if !self.wrap {
            handler = handler.with_wrap_lines(false);
        }
        handler
    }
}

static STYLE: OnceLock<Style> = OnceLock::new();

/// Decide the style once, from `--color`, and make miette's own formatting
/// (`{:?}` on a `miette::Report`) agree with it.
pub fn install(choice: ColorChoice) {
    let style = Style::detect(choice);
    if STYLE.set(style).is_ok() {
        // Only fails if a hook is already installed, in which case it stands.
        let _ = miette::set_hook(Box::new(move |_| Box::new(style.handler())));
    }
}

/// The style in force: what `install` decided, or a plain one before it ran.
pub fn style() -> Style {
    STYLE.get().copied().unwrap_or(Style::PLAIN)
}

/// The diagnostics Harbour defines. Extending this list is how another error
/// type gets rendered richly: `anyhow` cannot downcast to a trait object, so
/// the types are named.
fn as_diagnostic<'a>(err: &'a (dyn StdError + 'static)) -> Option<&'a dyn Diagnostic> {
    use crate::resolver::errors::{DependencyCycle, DuplicateSource, NoSolution, ResolutionFailed};

    fn upcast<T: Diagnostic>(d: &T) -> &dyn Diagnostic {
        d
    }

    if let Some(d) = err.downcast_ref::<ManifestParseError>() {
        return Some(upcast(d));
    }
    if let Some(d) = err.downcast_ref::<DependencyCycle>() {
        return Some(upcast(d));
    }
    if let Some(d) = err.downcast_ref::<DuplicateSource>() {
        return Some(upcast(d));
    }
    if let Some(d) = err.downcast_ref::<NoSolution>() {
        return Some(upcast(d));
    }
    err.downcast_ref::<ResolutionFailed>().map(upcast)
}

/// Render an error for stderr, in `style`.
pub fn render(err: &anyhow::Error, style: Style) -> String {
    let chain: Vec<&(dyn StdError + 'static)> = err.chain().collect();
    let found = chain
        .iter()
        .enumerate()
        .find_map(|(i, e)| as_diagnostic(*e).map(|d| (i, d)));

    let mut out = String::new();
    let paint = |code: &str, text: &str| {
        if style.color {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    };

    match found {
        // The diagnostic is the whole story: let miette draw it.
        Some((0, diagnostic)) => {
            let _ = style.handler().render_report(&mut out, diagnostic);
        }
        // Context sits above the diagnostic ("while loading X"): say it first,
        // then draw the diagnostic.
        Some((index, diagnostic)) => {
            let _ = writeln!(out, "{}: {}", paint("1;31", "error"), chain[0]);
            for (n, cause) in chain[1..index].iter().enumerate() {
                if n == 0 {
                    let _ = writeln!(out, "\n{}:", paint("1", "Caused by"));
                }
                let _ = writeln!(out, "    {n}: {cause}");
            }
            out.push('\n');
            let _ = style.handler().render_report(&mut out, diagnostic);
        }
        None => {
            let _ = writeln!(out, "{}: {}", paint("1;31", "error"), chain[0]);
            if chain.len() > 1 {
                let _ = writeln!(out, "\n{}:", paint("1", "Caused by"));
                for (n, cause) in chain[1..].iter().enumerate() {
                    let _ = writeln!(out, "    {n}: {cause}");
                }
            }
        }
    }

    out.truncate(out.trim_end().len());
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    // ---- resolver diagnostics -------------------------------------------------

    use crate::resolver::errors::{
        CycleEdge, DependencyCycle, DuplicateSource, NoSolution, ResolutionFailed,
    };

    fn cycle() -> anyhow::Error {
        anyhow::Error::new(DependencyCycle {
            loop_path: "a -> b -> a".to_string(),
            help: "turn off the feature `b` of `a`".to_string(),
            edges: vec![
                CycleEdge {
                    from: "a".to_string(),
                    to: "b".to_string(),
                    how: "optional dependency, enabled by feature `b` of `a`".to_string(),
                },
                CycleEdge {
                    from: "b".to_string(),
                    to: "a".to_string(),
                    how: "required dependency".to_string(),
                },
            ],
        })
    }

    #[test]
    fn a_cycle_is_drawn_with_its_code_its_edges_and_its_help() {
        let text = render(&cycle(), Style::PLAIN);
        assert!(text.contains("harbour::resolver::cycle"), "code: {text}");
        assert!(
            text.contains("x cycle detected in dependency graph: a -> b -> a"),
            "headline: {text}"
        );
        assert!(
            text.contains("a -> b  (optional dependency, enabled by feature `b` of `a`)"),
            "edge: {text}"
        );
        assert!(
            text.contains("b -> a  (required dependency)"),
            "edge: {text}"
        );
        assert!(
            text.contains("help: turn off the feature `b` of `a`"),
            "help is on its own line: {text}"
        );
        assert!(!text.contains('\x1b'), "plain output has no ANSI: {text:?}");
    }

    #[test]
    fn a_cycles_display_is_self_sufficient_without_the_renderer() {
        // Anything that prints the error without going through `render` (a test,
        // a log line) still gets the loop and every edge.
        let err = cycle();
        let shown = err.to_string();
        assert_eq!(
            shown.lines().next(),
            Some("cycle detected in dependency graph: a -> b -> a")
        );
        assert!(shown.contains("a -> b  (optional dependency"), "{shown}");
        assert!(shown.contains("b -> a  (required dependency)"), "{shown}");
    }

    #[test]
    fn a_duplicate_source_names_the_package_in_its_help() {
        let err = anyhow::Error::new(DuplicateSource {
            package: "shared".to_string(),
            sources: "path+../s1, path+../s2".to_string(),
        });
        let text = render(&err, Style::PLAIN);
        assert!(
            text.contains("harbour::resolver::duplicate_source"),
            "{text}"
        );
        assert!(
            text.contains(
                "x package `shared` is available from more than one source: path+../s1, path+../s2"
            ),
            "{text}"
        );
        assert!(
            text.contains("all dependents of `shared` must agree on a single source"),
            "the help interpolates the package: {text}"
        );
    }

    #[test]
    fn no_solution_keeps_the_solvers_reasoning_and_adds_help() {
        let err = anyhow::Error::new(NoSolution {
            report: "Because dep has no version in ^2, app is forbidden.".to_string(),
        });
        let text = render(&err, Style::PLAIN);
        assert!(text.contains("harbour::resolver::no_solution"), "{text}");
        assert!(text.contains("x dependency resolution failed:"), "{text}");
        assert!(
            text.contains("Because dep has no version in ^2, app is forbidden."),
            "{text}"
        );
        assert!(text.contains("help: every package is built once"), "{text}");
    }

    #[test]
    fn a_failed_resolution_states_its_cause_and_has_no_help_it_cannot_back_up() {
        let err = anyhow::Error::new(ResolutionFailed {
            message: "Choosing a version for ghost failed: no network".to_string(),
        });
        let text = render(&err, Style::PLAIN);
        assert!(text.contains("harbour::resolver::failed"), "{text}");
        assert!(
            text.contains("x dependency resolution failed: Choosing a version for ghost failed"),
            "{text}"
        );
        assert!(!text.contains("help:"), "no invented advice: {text}");
    }

    #[test]
    fn a_resolver_diagnostic_under_context_is_still_drawn() {
        let err = cycle().context("while resolving the workspace");
        let text = render(&err, Style::PLAIN);
        assert!(
            text.starts_with("error: while resolving the workspace\n"),
            "{text}"
        );
        assert!(text.contains("harbour::resolver::cycle"), "{text}");
        assert!(text.contains("help: turn off the feature"), "{text}");
    }

    use super::*;

    fn parse_error(content: &str) -> anyhow::Error {
        let err = toml::from_str::<toml::Table>(content).unwrap_err();
        ManifestParseError::new(&err, content, Path::new("Harbour.toml"))
            .map(anyhow::Error::new)
            .expect("the parser gives a span")
    }

    const BAD: &str = "[package]\nname = \"x\"\nversion = = \"1\"\n";

    #[test]
    fn a_plain_error_lists_its_causes_instead_of_flattening_them() {
        let err = anyhow::anyhow!("disk on fire")
            .context("failed to read Harbour.toml")
            .context("could not load the workspace");
        let text = render(&err, Style::PLAIN);
        assert!(
            text.starts_with("error: could not load the workspace\n"),
            "{text}"
        );
        assert!(text.contains("Caused by:"), "{text}");
        assert!(
            text.contains("    0: failed to read Harbour.toml"),
            "{text}"
        );
        assert!(text.contains("    1: disk on fire"), "{text}");
        assert!(
            !text.contains(": failed to read Harbour.toml: "),
            "no `a: b: c`: {text}"
        );
    }

    #[test]
    fn an_error_without_causes_is_one_line() {
        let text = render(&anyhow::anyhow!("nothing to build"), Style::PLAIN);
        assert_eq!(text, "error: nothing to build\n");
    }

    #[test]
    fn plain_output_has_no_ansi_and_no_box_drawing() {
        let text = render(&parse_error(BAD), Style::PLAIN);
        assert!(!text.contains('\x1b'), "no ANSI: {text:?}");
        assert!(
            !text.chars().any(|c| ('\u{2500}'..='\u{257f}').contains(&c)),
            "no box-drawing: {text}"
        );
    }

    #[test]
    fn a_manifest_error_underlines_what_the_parser_blamed() {
        let content =
            "[package]\nname = \"x\"\nversion = \"1.0.0\"\n[targets.x]\nkind = \"exe\"\nsorces = [\"a.c\"]\n";
        // The raw table accepts the key; it is the manifest schema that rejects it.
        let schema_err =
            crate::core::manifest::Manifest::parse(content, Path::new("Harbour.toml")).unwrap_err();
        let text = render(&schema_err, Style::PLAIN);
        assert!(text.contains("Harbour.toml:6:1"), "location: {text}");
        assert!(text.contains("sorces"), "source line: {text}");
        // The underline is as wide as the key (`sorces` is six characters), not
        // a single caret. miette puts the label's connector in the middle of
        // it (`^^^|^^`), so count the underline's cells rather than its glyphs.
        let underline = text
            .lines()
            .find(|l| l.contains('^'))
            .expect("an underline line");
        let width = underline.chars().filter(|c| matches!(c, '^' | '|')).count();
        assert_eq!(width, "sorces".len(), "key underlined: {text}");
        assert!(text.contains("unknown field `sorces`"), "message: {text}");
        assert!(
            text.contains("expected one of"),
            "help carries the list: {text}"
        );
    }

    #[test]
    fn a_diagnostic_under_context_is_still_found_and_the_context_is_kept() {
        let err = parse_error(BAD)
            .context("while loading the workspace")
            .context("failed to resolve dependencies");
        let text = render(&err, Style::PLAIN);
        assert!(
            text.starts_with("error: failed to resolve dependencies\n"),
            "{text}"
        );
        assert!(text.contains("0: while loading the workspace"), "{text}");
        assert!(
            text.contains("Harbour.toml:3"),
            "diagnostic still drawn: {text}"
        );
    }

    #[test]
    fn colour_adds_ansi_and_is_off_without_it() {
        let coloured = Style {
            color: true,
            ..Style::PLAIN
        };
        assert!(render(&anyhow::anyhow!("boom"), coloured).contains("\x1b[1;31merror\x1b[0m"));
        assert!(!render(&anyhow::anyhow!("boom"), Style::PLAIN).contains('\x1b'));
    }

    #[test]
    fn display_path_is_relative_under_the_cwd_and_absolute_elsewhere() {
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(display_path(&cwd.join("a").join("Harbour.toml")), {
            let mut p = std::path::PathBuf::from("a");
            p.push("Harbour.toml");
            p.display().to_string()
        });
        let elsewhere = std::env::temp_dir().join("elsewhere").join("Harbour.toml");
        if !elsewhere.starts_with(&cwd) {
            assert_eq!(display_path(&elsewhere), elsewhere.display().to_string());
        }
    }

    #[test]
    fn no_color_is_honoured_by_detection() {
        // Detection reads the environment, so test the pure part: an explicit
        // `never` is never coloured, `always` always is.
        assert!(!Style::detect(ColorChoice::Never).color);
        assert!(Style::detect(ColorChoice::Always).color);
    }
}
