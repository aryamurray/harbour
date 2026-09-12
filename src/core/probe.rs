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

use crate::core::manifest::DeclOrderMap;

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
            ProbeKind::Sizeof { .. } => "sizeof",
        }
    }

    /// What the probe is asking about, for diagnostics.
    pub fn subject(&self) -> &str {
        match self {
            ProbeKind::Header { header, .. } => header,
            ProbeKind::Sizeof { ty, .. } => ty,
        }
    }
}

/// Where a probe's answers go.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProbeEmit {
    /// Each answer becomes a `-D` on this target's compile surface.
    #[default]
    Defines,
}

/// Visibility of the emitted defines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProbeVisibility {
    /// This target's own translation units only.
    #[default]
    Private,
    /// Propagated to dependents, and therefore part of the ABI key.
    Public,
}

/// One named probe: the name of the answer, and the question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Probe {
    /// The define this probe's answer is reported as.
    pub name: String,
    /// The question.
    pub kind: ProbeKind,
}

/// A target's whole probe declaration, after desugaring.
///
/// `probes` is order-preserving (`DeclOrderMap`, i.e. `IndexMap`) rather than
/// a `HashMap`, and that is not a preference. A measured 18 distinct link
/// orders across 40 clean runs of one manifest was caused by `HashMap`
/// iteration reaching build output; probe answers reach build output as
/// defines, so the same mistake here would reproduce the same bug.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeSet {
    /// Where answers go.
    pub emit: ProbeEmit,
    /// Visibility of the emitted defines.
    pub visibility: ProbeVisibility,
    /// The probes, in declaration order.
    pub probes: DeclOrderMap<String, ProbeKind>,
}

impl ProbeSet {
    /// Is there nothing to do?
    pub fn is_empty(&self) -> bool {
        self.probes.is_empty()
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
    /// Where answers go. Only `"defines"` today.
    #[serde(default)]
    pub emit: ProbeEmit,

    /// Visibility of the emitted defines.
    #[serde(default)]
    pub visibility: ProbeVisibility,

    /// Bulk header checks, auto-named `HAVE_<SANITIZED>`.
    #[serde(default)]
    pub check_headers: Vec<String>,

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

    /// Ask the size of this type.
    #[serde(default)]
    pub sizeof: Option<String>,

    /// Prerequisite headers, for a `header` probe.
    #[serde(default)]
    pub prelude: Vec<String>,
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
    /// The bulk lists (`check_headers`, `check_sizeof`) are sugar and they
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

        Ok(ProbeSet {
            emit: self.emit,
            visibility: self.visibility,
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
            self.sizeof.as_ref().map(|_| "sizeof"),
        ]
        .into_iter()
        .flatten()
        .collect();

        match present.as_slice() {
            [] => bail!(
                "target `{}`: probe `{}` does not say what to ask\n\
                 hint: give it exactly one of `header` or `sizeof`",
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
    fn visibility_and_emit_default_to_private_defines() {
        let set = raw("check_headers = [\"poll.h\"]\n")
            .into_probe_set("t")
            .expect("should desugar");
        assert_eq!(set.visibility, ProbeVisibility::Private);
        assert_eq!(set.emit, ProbeEmit::Defines);
    }
}
