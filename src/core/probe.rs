//! Configure-style probe declarations.
//!
//! A probe asks the *actual* target toolchain a question -- "does this header
//! exist", "what is `sizeof(long)`" -- by compiling a program Harbour writes.
//! The answers become defines on the target's compile surface.
//!
//! Design document: `docs/superpowers/specs/2026-09-11-native-probes-design.md`.
//!
//! The organising principle, which decides what belongs here: **a probe kind
//! is admitted only if it can be answered without executing target code.**
//! That is what makes probes work when cross-compiling, and it is why there
//! is no "run this program and read its exit code" kind and no
//! cross-compilation fallback value -- a fallback value is a guess, and a
//! guess is the vendored `config.h` this subsystem exists to delete.
//!
//! This module is the *schema*. The engine that answers the questions is
//! `crate::builder::probe`.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use std::path::PathBuf;

use crate::core::manifest::DeclOrderMap;
use crate::core::surface::Define;

/// What a single probe asks.
///
/// Deliberately a closed enum rather than a snippet of C. A declarative kind
/// can be validated and can produce a useful error; an arbitrary snippet can
/// only report "your program did not compile", turns the manifest into a C
/// file, and makes the cache key the hash of some unbounded C.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProbeKind {
    /// Does `#include <header>` compile?
    ///
    /// Compile-only, so it is answerable when cross-compiling.
    Header {
        /// The header to test.
        header: String,
        /// Prerequisite headers to include first.
        ///
        /// A list of header *names*, never arbitrary code: BSD-derived
        /// headers routinely need `sys/types.h` and `sys/socket.h` ahead of
        /// them, and that is a real need, but accepting a code fragment here
        /// would smuggle in the snippet probe rejected in the design.
        prelude: Vec<String>,
    },

    /// Does `symbol` exist and resolve at link time?
    ///
    /// The only kind that **links** rather than merely compiling, and it has
    /// to. A header declaring something the libc does not provide is the
    /// classic `configure` trap, and a compile-only check answers `yes` to
    /// every one of them. Linking is what makes the answer mean "I can call
    /// this" rather than "somebody wrote a prototype".
    ///
    /// Still answerable when cross-compiling, but it is the kind that needs
    /// a cross *linker* and a sysroot with libraries in it, which is a
    /// stronger requirement than a cross compiler. When that is missing the
    /// link baseline fails and the build stops; it must never degrade to
    /// answering `no` for everything (see `builder::probe`).
    Symbol {
        /// The symbol to look for. A function, usually; a variable works
        /// too, because the linker does not type-check C.
        symbol: String,
        /// Headers to include first, so the symbol is declared the way the
        /// package will see it.
        ///
        /// With no prelude a fallback declaration is emitted instead, which
        /// is how you check a symbol whose real prototype you do not know.
        /// See `builder::probe::symbol_snippet`.
        prelude: Vec<String>,
        /// Libraries the symbol may live in, without the `-l`.
        ///
        /// Subsumes `AC_CHECK_LIB`: "is `dlopen` available, and if so does
        /// it need `-ldl`" is one question asked twice with different
        /// `libs`, not a separate probe kind.
        libs: Vec<String>,
    },

    /// What is `sizeof(type)`?
    ///
    /// Answered by binary search on a compile-time predicate (a negative
    /// array bound), so no program is run and no diagnostic text is parsed.
    /// See `crate::builder::probe` for the mechanism.
    Sizeof {
        /// The type whose size is wanted, as written in C.
        #[serde(rename = "type")]
        ty: String,
        /// Additional headers to include before asking.
        ///
        /// Needed more often than it looks. `sizeof(time_t)` and
        /// `sizeof(off_t)` -- two of curl's seven `SIZEOF_*` values -- are
        /// not answerable from `<stddef.h>` alone, and the first run of this
        /// subsystem failed on exactly that. A small set of standard headers
        /// is included automatically (see `builder::probe`), so this is for
        /// types from a package's own headers or from a non-standard one.
        prelude: Vec<String>,
    },
}

impl ProbeKind {
    /// A short human name for diagnostics.
    pub fn kind_name(&self) -> &'static str {
        match self {
            ProbeKind::Header { .. } => "header",
            ProbeKind::Symbol { .. } => "symbol",
            ProbeKind::Sizeof { .. } => "sizeof",
        }
    }

    /// What the probe is asking about, for diagnostics.
    pub fn subject(&self) -> &str {
        match self {
            ProbeKind::Header { header, .. } => header,
            ProbeKind::Symbol { symbol, .. } => symbol,
            ProbeKind::Sizeof { ty, .. } => ty,
        }
    }
}

/// Where a target's probe answers go.
///
/// This key was removed in the first probe PR and is back now, which is the
/// point: while `defines` was the only option it was a knob that did nothing,
/// so `emit = "defines"` was a hard error rather than a no-op. A single-valued
/// selector is indistinguishable from no selector, and the 2026-09-07 audit's
/// section 2.7 is four schema fields that parsed and were never consumed.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ProbeEmit {
    /// Each answer becomes a `-D` on this target's compile surface.
    #[default]
    Defines,

    /// Answers are written into a generated header the package `#include`s.
    ///
    /// Required for curl and openssl and for nothing smaller: a flag list
    /// cannot express 253 answers, and those packages `#include` a config
    /// header *by name*, so there is no arrangement of `-D` that satisfies
    /// them.
    Header {
        /// The name the package includes it by, e.g. `curl_config.h`.
        ///
        /// Relative, and resolved inside Harbour's build tree -- never in
        /// the source tree. Probing must not dirty a vendored checkout, and
        /// a git-sourced package's tree is shared between builds.
        header: PathBuf,
    },
}

/// The manifest form of [`ProbeEmit`].
///
/// Two spellings -- `emit = "defines"` and `emit = { header = "x.h" }` --
/// which is why this is an untagged enum rather than a plain one. The inner
/// struct carries `deny_unknown_fields` so that `emit = { headr = "x.h" }`
/// is an error: an untagged enum whose struct variant tolerated unknown keys
/// would silently fall through to "no variant matched", and the three
/// `flatten`/`untagged` holes in the 2026-09-07 audit were all of that shape.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum RawProbeEmit {
    /// `emit = "defines"`.
    Keyword(ProbeEmitKeyword),
    /// `emit = { header = "curl_config.h" }`.
    Header(RawProbeEmitHeader),
}

/// The only bare word `emit` accepts. `emit = "header"` is an error, because
/// a header needs a name.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProbeEmitKeyword {
    Defines,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawProbeEmitHeader {
    pub header: PathBuf,
}

impl RawProbeEmit {
    fn into_emit(self, target: &str) -> Result<ProbeEmit> {
        match self {
            RawProbeEmit::Keyword(ProbeEmitKeyword::Defines) => Ok(ProbeEmit::Defines),
            RawProbeEmit::Header(h) => {
                // The path is joined onto a directory inside the build tree
                // and then handed to the compiler as an `-I` plus an
                // `#include`. An absolute path or a `..` would escape that
                // directory, which is at best confusing and at worst writes
                // outside the build tree.
                if h.header.as_os_str().is_empty() {
                    bail!("target `{target}`: `probes.emit.header` is empty");
                }
                // `is_absolute()` alone is not enough, and this is the
                // Windows trap this repo keeps hitting: `/etc/passwd` is
                // *not* absolute on Windows, having no drive letter -- but
                // `Path::join` still resolves it to the current drive's
                // root, so it escapes the build tree just the same.
                // `has_root()` is what catches a leading separator on both
                // platforms. Found by `windows-latest` failing, not by
                // reasoning about it.
                if h.header.is_absolute() || h.header.has_root() {
                    bail!(
                        "target `{}`: `probes.emit.header` must be a relative \\
                         path, got `{}`\\n\\
                         hint: the header is generated inside Harbour's build \\
                         tree and put on the include path for you; give the \\
                         name the package includes it by, e.g. \\
                         `curl_config.h`",
                        target,
                        h.header.display()
                    );
                }
                if h.header
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
                {
                    bail!(
                        "target `{}`: `probes.emit.header` must not contain \\
                         `..`, got `{}`",
                        target,
                        h.header.display()
                    );
                }
                Ok(ProbeEmit::Header { header: h.header })
            }
        }
    }
}

/// A target's whole probe declaration, after desugaring.
///
/// `probes` is order-preserving (`DeclOrderMap`, i.e. `IndexMap`) rather than
/// a `HashMap`, and that is not a preference. A measured 18 distinct link
/// orders across 40 clean runs of one manifest was caused by `HashMap`
/// iteration reaching build output; probe answers reach build output as
/// defines, so the same mistake here would reproduce the same bug.
// No `PartialEq`: it would require it on `Define`, which is a shared type in
// `core::surface`, and nothing compares two whole `ProbeSet`s. The tests
// compare individual `ProbeKind`s, which do derive it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProbeSet {
    /// Where the answers go.
    pub emit: ProbeEmit,

    /// Literal defines to put in the generated header alongside the probed
    /// ones, in declaration order and before them.
    ///
    /// Not a duplicate of `[targets.X.private] defines`, and only accepted
    /// when emitting a header. Of curl's 253 config lines, 98 are
    /// `CURL_DISABLE_*` and `CURL_CA_*` -- build *options* that were never
    /// measurements at all. They have to be in the same file as the probed
    /// answers because the package includes one header, but they are what
    /// the packager chose rather than what the toolchain reported. Keeping
    /// them in a separate list is what makes the generated header's
    /// `/* #undef */` lines mean "asked and answered no" rather than
    /// "nobody mentioned it".
    pub defines: Vec<Define>,

    /// The probes, in declaration order.
    pub probes: DeclOrderMap<String, ProbeKind>,
}

impl ProbeSet {
    /// Is there nothing to do?
    pub fn is_empty(&self) -> bool {
        self.probes.is_empty()
    }

    /// Does answering this set require a working linker?
    ///
    /// Only `symbol` probes link. Asked so the baseline check can link too
    /// when it must and stay compile-only when it need not: a package with
    /// no `symbol` probes should not be refused on a target that can compile
    /// but not link, and a package *with* them must be.
    pub fn needs_linker(&self) -> bool {
        self.probes
            .values()
            .any(|k| matches!(k, ProbeKind::Symbol { .. }))
    }
}

/// The manifest form of [`ProbeSet`], before desugaring.
///
/// Named probes live under an explicit `named` sub-table rather than being
/// `#[serde(flatten)]`ed alongside `emit`/`check_headers`. Flattening would
/// read better in TOML and is refused on purpose: `deny_unknown_fields` does
/// not survive a `flatten`, and three of the ten defects in the 2026-09-07
/// schema audit were exactly that hole -- a key silently routed into a
/// flattened struct and dropped. A probe that parses and never runs is the
/// single most likely way this subsystem fails.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawProbeSet {
    /// Where the answers go. Defaults to `"defines"`.
    #[serde(default)]
    pub emit: Option<RawProbeEmit>,

    /// Literal defines for the generated header. Requires
    /// `emit = { header = ... }`.
    #[serde(default)]
    pub defines: Vec<Define>,

    /// Bulk header checks, auto-named `HAVE_<SANITIZED>`.
    #[serde(default)]
    pub check_headers: Vec<String>,

    /// Bulk symbol checks, auto-named `HAVE_<SANITIZED>`.
    ///
    /// No `libs` and no `prelude`: a bulk entry is for the common case,
    /// which is a libc function reachable with no extra library and no
    /// header (the fallback declaration covers it). Anything needing either
    /// goes in `named`.
    #[serde(default)]
    pub check_symbols: Vec<String>,

    /// Bulk size checks, auto-named `SIZEOF_<SANITIZED>`.
    #[serde(default)]
    pub check_sizeof: Vec<String>,

    /// Explicitly named probes, for anything needing a custom name or
    /// options the bulk lists cannot express.
    #[serde(default)]
    pub named: DeclOrderMap<String, RawProbe>,
}

/// The manifest form of one named probe.
///
/// Exactly one of `header` / `sizeof` must be present. Spelled as optional
/// fields plus a hand-rolled check rather than as a `#[serde(untagged)]`
/// enum, for the reason given on [`RawProbeSet`].
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawProbe {
    /// Ask whether this header can be included.
    #[serde(default)]
    pub header: Option<String>,

    /// Ask whether this symbol exists and links.
    #[serde(default)]
    pub symbol: Option<String>,

    /// Ask the size of this type.
    #[serde(default)]
    pub sizeof: Option<String>,

    /// Prerequisite headers. Meaningful for every kind.
    #[serde(default)]
    pub prelude: Vec<String>,

    /// Libraries to link when answering, without the `-l`. `symbol` only.
    #[serde(default)]
    pub libs: Vec<String>,
}

/// Derive the conventional define name for a probed header or type.
///
/// Uppercase, every character that is not ASCII alphanumeric becomes `_`.
/// `sys/socket.h` -> `SYS_SOCKET_H`, `long long` -> `LONG_LONG`,
/// `size_t` -> `SIZE_T`. This is the convention autoconf and CMake both use
/// and the one the packages' own headers expect, so it is not a choice so
/// much as a transcription.
pub fn sanitize_name(subject: &str) -> String {
    let mut out = String::with_capacity(subject.len());
    for c in subject.chars() {
        if c == '*' {
            // Pointer types. autoconf's `AC_CHECK_SIZEOF` transliterates `*`
            // to `p` *before* uppercasing, which is why the universally
            // recognised name is `SIZEOF_VOID_P`. Mapping `*` to `_` like
            // any other punctuation would produce `SIZEOF_VOID`, and the
            // package's C code would read a macro nobody defined.
            //
            // The separator is inserted here rather than relying on the
            // space in `void *`, so that `void*` and `void *` -- two
            // spellings of one type -- cannot produce two different probe
            // names. autoconf itself does not do this: it yields `VOIDP` for
            // `void*`, which is a whitespace-sensitive define name and a
            // trap with no upside. Consecutive stars still run together
            // (`char **` -> `CHAR_PP`), matching autoconf where autoconf is
            // not being accidental.
            if !out.is_empty() && !out.ends_with('_') && !out.ends_with('P') {
                out.push('_');
            }
            out.push('P');
            continue;
        }
        let mapped = if c.is_ascii_alphanumeric() {
            c.to_ascii_uppercase()
        } else {
            '_'
        };
        // Collapse runs of separators, so `long  long` and `long long` agree.
        if mapped == '_' && (out.is_empty() || out.ends_with('_')) {
            continue;
        }
        out.push(mapped);
    }
    out.trim_end_matches('_').to_string()
}

/// `HAVE_` + [`sanitize_name`].
pub fn have_name(subject: &str) -> String {
    format!("HAVE_{}", sanitize_name(subject))
}

/// `SIZEOF_` + [`sanitize_name`].
pub fn sizeof_name(subject: &str) -> String {
    format!("SIZEOF_{}", sanitize_name(subject))
}

/// A probe define name must be usable as a C identifier, because it becomes
/// one. `-DHAVE_FOO-BAR=1` is not a define, it is a syntax error the
/// compiler reports against a file the user never wrote.
fn validate_probe_name(target: &str, name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !ok {
        bail!(
            "target `{}`: probe name `{}` is not a valid C identifier\n\
             hint: probe names become `-D` defines, so they must be \
             letters, digits and underscores, not starting with a digit",
            target,
            name
        );
    }
    Ok(())
}

impl RawProbeSet {
    /// Desugar the bulk lists and validate, producing the single form
    /// everything downstream consumes.
    ///
    /// The bulk lists (`check_headers`, `check_symbols`, `check_sizeof`)
    /// are sugar and they
    /// are desugared **here, in the parser**, so that there is exactly one
    /// kind of probe for the engine, the cache and the fingerprint to know
    /// about. The 2026-09-07 audit's headline finding was that every one of
    /// its ten defects was one field with two independent consumers that had
    /// drifted; a second execution path for the shorthand form would be the
    /// eleventh.
    ///
    /// `target` is used only for error messages.
    pub fn into_probe_set(self, target: &str) -> Result<ProbeSet> {
        let mut probes: DeclOrderMap<String, ProbeKind> = DeclOrderMap::new();

        // Bulk lists first, in list order, then named entries in declaration
        // order. Fixed and documented, because it decides the order defines
        // reach the compiler.
        for header in &self.check_headers {
            let name = have_name(header);
            insert_probe(
                &mut probes,
                target,
                name,
                ProbeKind::Header {
                    header: header.clone(),
                    prelude: Vec::new(),
                },
            )?;
        }

        for symbol in &self.check_symbols {
            let name = have_name(symbol);
            insert_probe(
                &mut probes,
                target,
                name,
                ProbeKind::Symbol {
                    symbol: symbol.clone(),
                    prelude: Vec::new(),
                    libs: Vec::new(),
                },
            )?;
        }

        for ty in &self.check_sizeof {
            let name = sizeof_name(ty);
            insert_probe(
                &mut probes,
                target,
                name,
                ProbeKind::Sizeof {
                    ty: ty.clone(),
                    prelude: Vec::new(),
                },
            )?;
        }

        for (name, raw) in self.named {
            let kind = raw.into_kind(target, &name)?;
            insert_probe(&mut probes, target, name, kind)?;
        }

        for name in probes.keys() {
            validate_probe_name(target, name)?;
        }

        let emit = match self.emit {
            Some(raw) => raw.into_emit(target)?,
            None => ProbeEmit::Defines,
        };

        // `defines` has nowhere to go without a header: the same list
        // written as `-D` flags is what `[targets.X.private] defines`
        // already is, and offering two spellings of one thing invites the
        // reader to look for a difference. Refused rather than silently
        // merged.
        if !self.defines.is_empty() && matches!(emit, ProbeEmit::Defines) {
            bail!(
                "target `{}`: `probes.defines` requires \
                 `emit = {{ header = \"...\" }}`\n\
                 hint: without a generated header these are just compile \
                 defines; put them in `[targets.{}.private]` or \
                 `[targets.{}.public]` instead",
                target,
                target,
                target
            );
        }

        Ok(ProbeSet {
            emit,
            defines: self.defines,
            probes,
        })
    }
}

/// Insert, refusing a duplicate rather than letting the later one win.
///
/// A collision means two declarations disagree about what one define means.
/// Silently keeping one is how a manifest ends up with a probe that parses
/// and never runs.
fn insert_probe(
    probes: &mut DeclOrderMap<String, ProbeKind>,
    target: &str,
    name: String,
    kind: ProbeKind,
) -> Result<()> {
    if let Some(existing) = probes.get(&name) {
        bail!(
            "target `{}`: two probes both produce `{}` (a `{}` probe for `{}` \
             and a `{}` probe for `{}`)\n\
             hint: bulk `check_*` entries are auto-named `HAVE_<NAME>` / \
             `SIZEOF_<NAME>`; give one of them an explicit name under \
             `[targets.{}.probes.named.NAME]` instead",
            target,
            name,
            existing.kind_name(),
            existing.subject(),
            kind.kind_name(),
            kind.subject(),
            target
        );
    }
    probes.insert(name, kind);
    Ok(())
}

impl RawProbe {
    fn into_kind(self, target: &str, name: &str) -> Result<ProbeKind> {
        let present: Vec<&str> = [
            self.header.as_ref().map(|_| "header"),
            self.symbol.as_ref().map(|_| "symbol"),
            self.sizeof.as_ref().map(|_| "sizeof"),
        ]
        .into_iter()
        .flatten()
        .collect();

        // `libs` only means anything where there is a link step, which is
        // `symbol` alone. Refused rather than ignored elsewhere: a key that
        // parses and reaches no command line is how `frameworks` and
        // `groups` survived for months.
        if !self.libs.is_empty() && present.as_slice() != ["symbol"] {
            bail!(
                "target `{}`: probe `{}` sets `libs`, which only a `symbol` \
                 probe uses -- it is the only kind that links\n\
                 hint: a `header`, `sizeof` or `type` probe is answered by \
                 compiling, so there is no link line for `libs` to reach",
                target,
                name
            );
        }

        match present.as_slice() {
            [] => bail!(
                "target `{}`: probe `{}` does not say what to ask\n\
                 hint: give it exactly one of `header`, `symbol` or `sizeof`",
                target,
                name
            ),
            ["header"] => {
                if self.header.as_deref() == Some("") {
                    bail!(
                        "target `{}`: probe `{}` has an empty `header`",
                        target,
                        name
                    );
                }
                Ok(ProbeKind::Header {
                    header: self.header.expect("matched on header being present"),
                    prelude: self.prelude,
                })
            }
            ["symbol"] => {
                let symbol = self.symbol.expect("matched on symbol being present");
                // The symbol is pasted into C source and its address taken,
                // so anything that is not an identifier is a syntax error
                // reported against a file the author never wrote.
                let ok = !symbol.is_empty()
                    && !symbol.starts_with(|c: char| c.is_ascii_digit())
                    && symbol
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_');
                if !ok {
                    bail!(
                        "target `{}`: probe `{}` asks for symbol `{}`, which is \
                         not a C identifier\n\
                         hint: a `symbol` probe takes a bare name (`strerror_r`), \
                         not a declaration or an expression",
                        target,
                        name,
                        symbol
                    );
                }
                Ok(ProbeKind::Symbol {
                    symbol,
                    prelude: self.prelude,
                    libs: self.libs,
                })
            }
            ["sizeof"] => Ok(ProbeKind::Sizeof {
                ty: self.sizeof.expect("matched on sizeof being present"),
                prelude: self.prelude,
            }),
            multiple => bail!(
                "target `{}`: probe `{}` asks {} things at once ({})\n\
                 hint: a probe has exactly one question; split it into \
                 separate named probes",
                target,
                name,
                multiple.len(),
                multiple.join(", ")
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(toml_src: &str) -> RawProbeSet {
        toml::from_str(toml_src).expect("should parse")
    }

    #[test]
    fn sanitize_follows_the_autoconf_convention() {
        assert_eq!(sanitize_name("sys/socket.h"), "SYS_SOCKET_H");
        assert_eq!(sanitize_name("long long"), "LONG_LONG");
        assert_eq!(sanitize_name("size_t"), "SIZE_T");
        assert_eq!(sanitize_name("netinet/in.h"), "NETINET_IN_H");
        assert_eq!(have_name("sys/ioctl.h"), "HAVE_SYS_IOCTL_H");
        assert_eq!(sizeof_name("off_t"), "SIZEOF_OFF_T");
    }

    #[test]
    fn bulk_lists_desugar_into_named_probes_in_declaration_order() {
        let set = raw(r#"
            check_headers = ["sys/socket.h", "poll.h"]
            check_sizeof = ["long", "size_t"]
        "#)
        .into_probe_set("t")
        .expect("should desugar");

        let names: Vec<&str> = set.probes.keys().map(|s| s.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "HAVE_SYS_SOCKET_H",
                "HAVE_POLL_H",
                "SIZEOF_LONG",
                "SIZEOF_SIZE_T"
            ],
            "bulk lists must desugar in list order, headers before sizes"
        );
        assert_eq!(
            set.probes["HAVE_POLL_H"],
            ProbeKind::Header {
                header: "poll.h".to_string(),
                prelude: Vec::new()
            }
        );
        assert_eq!(
            set.probes["SIZEOF_LONG"],
            ProbeKind::Sizeof {
                ty: "long".to_string(),
                prelude: Vec::new(),
            }
        );
    }

    #[test]
    fn named_probes_keep_their_name_and_accept_a_prelude() {
        let set = raw(r#"
            [named.HAVE_NETINET_IN_H]
            header = "netinet/in.h"
            prelude = ["sys/types.h", "sys/socket.h"]

            [named.SIZEOF_CURL_OFF_T]
            sizeof = "long long"
        "#)
        .into_probe_set("t")
        .expect("should desugar");

        assert_eq!(
            set.probes["HAVE_NETINET_IN_H"],
            ProbeKind::Header {
                header: "netinet/in.h".to_string(),
                prelude: vec!["sys/types.h".to_string(), "sys/socket.h".to_string()],
            }
        );
        // The whole point of a custom name: SIZEOF_CURL_OFF_T is not
        // derivable from `long long`.
        assert_eq!(
            set.probes["SIZEOF_CURL_OFF_T"],
            ProbeKind::Sizeof {
                ty: "long long".to_string(),
                prelude: Vec::new(),
            }
        );
    }

    #[test]
    fn bulk_and_named_entries_colliding_on_one_name_is_an_error() {
        let err = raw(r#"
            check_headers = ["poll.h"]
            [named.HAVE_POLL_H]
            header = "sys/poll.h"
        "#)
        .into_probe_set("t")
        .expect_err("a name collision must not silently pick one");
        let msg = err.to_string();
        assert!(msg.contains("HAVE_POLL_H"), "error names the define: {msg}");
        assert!(msg.contains("poll.h"), "error names both subjects: {msg}");
    }

    #[test]
    fn a_probe_asking_nothing_or_two_things_is_an_error() {
        let err = raw("[named.EMPTY]\n")
            .into_probe_set("t")
            .expect_err("a probe with no question is an error");
        assert!(err.to_string().contains("does not say what to ask"));

        let err = raw(r#"
            [named.BOTH]
            header = "poll.h"
            sizeof = "long"
        "#)
        .into_probe_set("t")
        .expect_err("a probe with two questions is an error");
        let msg = err.to_string();
        assert!(msg.contains("asks 2 things at once"), "{msg}");
        assert!(msg.contains("header") && msg.contains("sizeof"), "{msg}");
    }

    #[test]
    fn unknown_keys_are_rejected_at_both_levels() {
        // The whole reason `named` is a sub-table instead of a flatten.
        let err = toml::from_str::<RawProbeSet>("check_headerz = [\"poll.h\"]\n")
            .expect_err("a typo in a probe-set key must be rejected");
        assert!(err.to_string().contains("check_headerz"), "{err}");

        let err = toml::from_str::<RawProbeSet>("[named.X]\nheaderr = \"poll.h\"\n")
            .expect_err("a typo in a probe key must be rejected");
        assert!(err.to_string().contains("headerr"), "{err}");
    }

    #[test]
    fn a_define_name_that_is_not_a_c_identifier_is_rejected() {
        // Reachable through an explicit name; the sanitizer makes it
        // unreachable through the bulk lists, which is why only `named`
        // needs the check.
        let err = raw("[named.\"HAVE-DASH\"]\nheader = \"poll.h\"\n")
            .into_probe_set("t")
            .expect_err("a name that is not a C identifier must be rejected");
        assert!(err.to_string().contains("valid C identifier"), "{err}");
    }

    #[test]
    fn a_sizeof_probe_accepts_a_prelude() {
        // The design document originally said `prelude` was meaningless on a
        // `sizeof` probe and rejected it. Running the first real fixture
        // refuted that within a minute: `sizeof(time_t)` with only
        // `<stddef.h>` in scope fails with "use of undeclared identifier
        // 'time_t'", and `SIZEOF_TIME_T` / `SIZEOF_OFF_T` are two of curl's
        // seven `SIZEOF_*` values. A type's size is only askable where the
        // type is visible.
        let set = raw("[named.SIZEOF_OFF_T]\nsizeof = \"off_t\"\nprelude = [\"sys/types.h\"]\n")
            .into_probe_set("t")
            .expect("a sizeof probe may name the header its type comes from");
        assert_eq!(
            set.probes["SIZEOF_OFF_T"],
            ProbeKind::Sizeof {
                ty: "off_t".to_string(),
                prelude: vec!["sys/types.h".to_string()],
            }
        );
    }

    #[test]
    fn pointer_types_are_named_the_way_every_config_header_spells_them() {
        // autoconf transliterates `*` to `p` before uppercasing, which is why
        // the universally recognised name is `SIZEOF_VOID_P`. Mapping `*` to
        // `_` like any other punctuation would produce `SIZEOF_VOID` and
        // silently fail to define the macro the package's C code reads.
        assert_eq!(sizeof_name("void *"), "SIZEOF_VOID_P");
        assert_eq!(
            sizeof_name("void*"),
            "SIZEOF_VOID_P",
            "both spellings of a pointer type must produce one name, or a \
             manifest gets two probes for one question depending on \
             whitespace"
        );
        assert_eq!(sizeof_name("char **"), "SIZEOF_CHAR_PP");
    }

    #[test]
    fn check_symbols_desugars_after_headers_and_before_sizes() {
        let set = raw(r#"
            check_headers = ["poll.h"]
            check_symbols = ["poll", "strerror_r"]
            check_sizeof = ["long"]
        "#)
        .into_probe_set("t")
        .expect("should desugar");

        let names: Vec<&str> = set.probes.keys().map(|s| s.as_str()).collect();
        assert_eq!(
            names,
            vec!["HAVE_POLL_H", "HAVE_POLL", "HAVE_STRERROR_R", "SIZEOF_LONG"],
            "the bulk lists have a fixed, documented order, because it decides \
             the order defines reach the compiler"
        );
        assert_eq!(
            set.probes["HAVE_POLL"],
            ProbeKind::Symbol {
                symbol: "poll".to_string(),
                prelude: Vec::new(),
                libs: Vec::new(),
            },
            "a bulk symbol entry gets no prelude and no libs: the common case \
             is a libc function reachable with neither"
        );
        // `poll.h` and `poll` are two different questions that must not
        // collide, which is why the header rule appends `_H` from the
        // filename rather than stripping it.
        assert_ne!(have_name("poll.h"), have_name("poll"));
    }

    #[test]
    fn only_a_symbol_probe_needs_a_linker() {
        let none = raw("check_headers = [\"poll.h\"]\ncheck_sizeof = [\"long\"]\n")
            .into_probe_set("t")
            .expect("should desugar");
        assert!(
            !none.needs_linker(),
            "`header` and `sizeof` are answered by compiling, so a target with \
             only those must not be refused on a toolchain that cannot link"
        );

        let some = raw("check_headers = [\"poll.h\"]\ncheck_symbols = [\"poll\"]\n")
            .into_probe_set("t")
            .expect("should desugar");
        assert!(
            some.needs_linker(),
            "one `symbol` probe is enough to require a working linker -- a \
             compile-only answer would be a different question under the same \
             name"
        );
    }

    #[test]
    fn a_named_symbol_probe_carries_its_prelude_and_libs() {
        let set = raw(r#"
            [named.HAVE_DLOPEN]
            symbol = "dlopen"
            prelude = ["dlfcn.h"]
            libs = ["dl"]
        "#)
        .into_probe_set("t")
        .expect("should desugar");
        assert_eq!(
            set.probes["HAVE_DLOPEN"],
            ProbeKind::Symbol {
                symbol: "dlopen".to_string(),
                prelude: vec!["dlfcn.h".to_string()],
                libs: vec!["dl".to_string()],
            }
        );
    }

    #[test]
    fn libs_on_a_kind_that_does_not_link_is_rejected() {
        // Accepting and dropping it is how `frameworks` and `groups` came to
        // parse, propagate and be reported for months without ever reaching
        // a linker.
        for kind in ["header = \"poll.h\"", "sizeof = \"long\""] {
            let err = raw(&format!("[named.X]\n{kind}\nlibs = [\"m\"]\n"))
                .into_probe_set("t")
                .expect_err("a key with no effect must be refused");
            let msg = err.to_string();
            assert!(msg.contains("only a `symbol` probe uses"), "{msg}");
        }
    }

    #[test]
    fn a_symbol_that_is_not_an_identifier_is_rejected() {
        // The name is pasted into C source and its address taken, so
        // anything else is a syntax error reported against a file the author
        // never wrote.
        for bad in ["poll()", "struct foo", "2poll", "a-b", ""] {
            let err = raw(&format!("[named.X]\nsymbol = \"{bad}\"\n"))
                .into_probe_set("t")
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("not a C identifier"),
                "`{bad}` must be refused as a symbol name, got: {err}"
            );
        }
    }

    #[test]
    fn a_probe_cannot_ask_for_a_header_and_a_symbol_at_once() {
        let err = raw("[named.X]\nheader = \"poll.h\"\nsymbol = \"poll\"\n")
            .into_probe_set("t")
            .expect_err("two questions in one probe is an error");
        let msg = err.to_string();
        assert!(msg.contains("asks 2 things at once"), "{msg}");
        assert!(msg.contains("header") && msg.contains("symbol"), "{msg}");
    }

    #[test]
    fn emit_accepts_its_two_real_spellings_and_nothing_else() {
        // `emit` was a hard error in the first probe PR, when `defines` was
        // its only value: a single-valued selector is indistinguishable from
        // no selector, and the 2026-09-07 audit's section 2.7 is four schema
        // fields that parsed and were never consumed. It is back because it
        // now selects between two genuinely different behaviours.
        let defines = raw("emit = \"defines\"\ncheck_headers = [\"poll.h\"]\n")
            .into_probe_set("t")
            .expect("`emit = \"defines\"` is the explicit form of the default");
        assert_eq!(defines.emit, ProbeEmit::Defines);

        let header = raw("emit = { header = \"curl_config.h\" }\n")
            .into_probe_set("t")
            .expect("the header form");
        assert_eq!(
            header.emit,
            ProbeEmit::Header {
                header: PathBuf::from("curl_config.h")
            }
        );

        // A bare `"header"` has no name to write to.
        assert!(
            toml::from_str::<RawProbeSet>("emit = \"header\"\n").is_err(),
            "`emit = \"header\"` must be refused: a header needs a name"
        );
        // A typo inside the table. This is why `RawProbeEmitHeader` carries
        // `deny_unknown_fields` -- an untagged enum whose struct variant
        // tolerated unknown keys would fall through to "no variant matched",
        // and all three `flatten`/`untagged` holes in the audit were that
        // shape.
        assert!(
            toml::from_str::<RawProbeSet>("emit = { headr = \"x.h\" }\n").is_err(),
            "a typo'd key inside `emit` must be refused"
        );
        // Escaping the build tree. `/etc/passwd` is in this list
        // *unguarded* on purpose: it is not `is_absolute()` on Windows,
        // having no drive letter, but `Path::join` still resolves it to the
        // current drive's root -- so it escapes there too and must be
        // refused on both platforms. This assertion failed on
        // `windows-latest` when the check was `is_absolute()` alone, which
        // is why it is `has_root()` now.
        for bad in ["/etc/passwd", "../../outside.h", "a/../../b.h"] {
            let err = raw(&format!("emit = {{ header = \"{bad}\" }}\n"))
                .into_probe_set("t")
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("relative") || err.contains(".."),
                "`{bad}` must be refused on every platform: {err}"
            );
        }
    }

    #[test]
    fn probe_defines_require_a_generated_header_to_go_into() {
        // Without a header these are just compile defines, which
        // `[targets.X.private] defines` already spells. Two spellings of one
        // thing invites the reader to look for a difference that is not
        // there.
        let err = raw("defines = [\"FOO=1\"]\ncheck_headers = [\"poll.h\"]\n")
            .into_probe_set("t")
            .expect_err("literals with nowhere to go must be refused")
            .to_string();
        assert!(err.contains("requires"), "{err}");

        // With a header they are accepted and kept in declaration order.
        let set = raw("emit = { header = \"c.h\" }\n\
             defines = [\"A=1\", \"B\"]\n")
        .into_probe_set("t")
        .expect("literals belong in a generated header");
        let names: Vec<&str> = set.defines.iter().map(|d| d.name()).collect();
        assert_eq!(names, vec!["A", "B"]);
    }

    #[test]
    fn visibility_is_still_rejected_because_it_still_does_not_work() {
        // Unchanged from the first probe PR, and worth keeping distinct from
        // `emit` above: `visibility = "public"` was implemented, branched on,
        // and reached the ABI cache key -- every outward sign of being wired
        // -- and it does not propagate. A dependent's surface is folded from
        // each dependency's *declared* `surface.compile.public`, and a
        // measured answer is in no manifest. The consumer failed to compile
        // on an undefined `SIZEOF_LONG`.
        //
        // `emit` came back because it gained a second value. This has not,
        // because nothing about the fold has changed.
        let err = toml::from_str::<RawProbeSet>("visibility = \"public\"\n")
            .expect_err("a key that does not do what it says must be refused")
            .to_string();
        assert!(err.contains("visibility"), "{err}");
    }
}
