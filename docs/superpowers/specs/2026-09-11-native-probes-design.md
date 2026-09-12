# Design: native configure-style probes

**Date:** 2026-09-11
**Status:** Design — Phase 1 of a staged build. Phase 2 (a vertical slice) lands
in the PR stacked on top of this one.
**Tracking:** replaces the vendored `harbour-config/<os>-<arch>/curl_config.h`
placeholder described at `src/core/target/core.rs:702-711`.

---

## The one-sentence version

Harbour learns to ask the *actual* target toolchain a small, fixed set of
questions — does this header exist, does this symbol link, does this type
exist, what is `sizeof(T)`, does the compiler accept this flag — by compiling
(and sometimes linking) tiny generated programs, and turns the answers into
defines or into a generated header. **The set of questions is exactly the set
that can be answered without running target code**, which is what makes it
work when cross-compiling, and which is the organising principle everything
else in this document follows from.

## Why this exists

To make curl build, someone vendored a 793-line `curl_config.h` per (os, arch)
pair, supporting exactly `macos-aarch64` and `linux-x86_64`. It was keyed on
`os` alone at first, so a 32-bit Linux target would have silently matched a
config asserting `SIZEOF_LONG 8`. The CI job that produces those files
(`.github/workflows/harvest.yml:76-86`) says so in its own comment: "The
generated config header is the reason this job exists: it is the part that
genuinely cannot be produced on another platform."

That is the artifact this design exists to delete. Two things follow from the
repo owner's constraints and are not up for discussion here:

- **Harbour replaces upstream build systems; it does not delegate to them.** A
  probe subsystem that shells out to curl's `configure`, or to CMake, is a
  non-answer. Probes compile programs Harbour itself writes, with the toolchain
  Harbour already detected.
- **CMake must not become a build dependency of Harbour.** It is used in this
  document only as an *oracle* for verification — the same role
  `harvest.yml` already gives it — never at build time.

## Scope check, because the scope is easy to overstate

A recent audit shimmed cJSON, zstd and libuv with *zero* vendored config
(`docs/superpowers/specs/2026-09-07-extensibility-audit.md`). None of those
ships a configure-generated `config.h`. That is evidence those three packages
are easy, not evidence that curl became easier. Probes are for packages that
genuinely need to interrogate the toolchain: curl, openssl, libpng's
`zlib`-version checks, anything autotools- or cmake-configured.

Concretely — measured, not estimated, by regenerating curl 8.22.0's
`curl_config.h` with the exact cmake options the committed shim used
(`harvest.yml:52-63`), which reproduces the 793-line file byte-for-byte in
shape: 253 answered questions (110 `#define`, 143 `/* #undef */`), of which

| | count | probe kind |
|---|---|---|
| `HAVE_<name>_H` | 41 | `header` |
| other `HAVE_<name>` (mostly functions) | 107 | `symbol` |
| `SIZEOF_<type>` | 7 | `sizeof` |
| `HAVE_STRUCT_<t>` / type checks | 2 | `type` |
| `CURL_DISABLE_*` / `CURL_CA_*` / project settings | 98 | **not a probe** — literal defines |

So 157 of curl's 253 lines are probe questions in the five kinds below, and the
remaining 98 are build *options* that were never measurements at all — they are
what the packager chose, and they belong in the manifest as literal `defines`.
Recognising that split is itself part of the win: a third of the vendored file
is configuration masquerading as discovered fact.

---

## 1. The probe kinds

Five. Each is declarative — a kind plus a couple of named fields — not a code
snippet. Each is answerable by compiling, or compiling and linking, and
therefore answerable when cross-compiling.

| kind | question | mechanism | needs a linker |
|---|---|---|---|
| `header` | does `#include <X>` work? | compile only | no |
| `symbol` | does `X` exist and resolve? | compile **and link** | yes |
| `type` | does type `T` (optionally, member `T.m`) exist? | compile only | no |
| `sizeof` | what is `sizeof(T)`? | compile only, bisection | no |
| `flag` | does the compiler accept flag `F`? | compile only | no |

### `header`

```c
#include <sys/socket.h>
int main(void) { return 0; }
```

Compiled with `-c` to a scratch object. Exit 0 → true. This is `AC_CHECK_HEADER`
with the *compile* semantics, not the preprocess-only semantics: autoconf
historically ran both and warned when they disagreed. Compiling is the useful
one, because a header that preprocesses but does not compile (missing
prerequisite, wrong architecture) is not a header you have.

A `prelude` field allows prerequisite headers, which BSD-derived headers need
(`sys/socket.h` before `netinet/in.h` on some platforms):

```toml
HAVE_NETINET_IN_H = { header = "netinet/in.h", prelude = ["sys/types.h", "sys/socket.h"] }
```

`prelude` is a list of *header names*, not arbitrary code. That distinction is
load-bearing — see §7.

### `symbol`

```c
/* prelude headers, if any */
#include <string.h>
int main(void) { (void) &strerror_r; return 0; }
```

Compiled **and linked**. `&name` rather than a call, so the probe does not have
to know the signature. If no declaring header is given, a fallback declaration
`char name(void);` is emitted instead — the autoconf trick that lets you check
a symbol whose real prototype you do not know. Taking the address of a
mis-declared function still forces the linker to resolve the name, which is the
question being asked.

Optional `libs = ["m"]` puts `-lm` on the probe's link line, subsuming
`AC_CHECK_LIB` (§7).

**This is the only kind that needs a working cross *linker*, not just a cross
compiler.** That is a real and stronger requirement, handled in §5.

### `type`

```c
#include <sys/time.h>
int main(void) { struct timeval v; (void) sizeof(v); return 0; }
```

With an optional `member`, it becomes the standard offset idiom, which
correctly rejects a type that exists without the member:

```c
int main(void) { struct sockaddr_storage s; (void) sizeof(s.ss_family); return 0; }
```

curl needs both shapes (`HAVE_STRUCT_TIMEVAL`,
`HAVE_STRUCT_SOCKADDR_STORAGE`). A separate `member` field rather than
accepting `"struct sockaddr_storage.ss_family"` as one string, because parsing
a C type expression out of a TOML string is the beginning of a language.

### `sizeof`

The interesting one, because it is the one everybody assumes needs to run a
program. It does not.

**Mechanism: binary search on a compile-time predicate.** Each trial compiles

```c
#include <stddef.h>
int main(void) { char probe[(sizeof(long) <= 8) ? 1 : -1]; (void) probe; return 0; }
```

A negative array bound is ill-formed in every C dialect, so the compile fails
iff `sizeof(long) > 8`. Binary search over `[0, 64]` converges in 7 compiles
and yields the *exact* value. No program is executed; no diagnostic text is
parsed.

**Correction, from running it.** An earlier draft of this section said
`prelude` was meaningless on a `sizeof` probe and should be rejected there.
The first real fixture refuted that in under a minute: `sizeof(time_t)` with
only `<stddef.h>` in scope fails with *use of undeclared identifier
'time_t'*, and `SIZEOF_TIME_T` and `SIZEOF_OFF_T` are two of curl's seven
`SIZEOF_*` values. A type's size is only askable where the type is visible, so
`sizeof` takes a `prelude` too. Additionally, the snippet gets `<stdint.h>`,
`<time.h>` and `<sys/types.h>` automatically, each behind
`#if __has_include(<...>)` so that a target lacking one still gets an answer
rather than an error about a prerequisite it never asked for. `<stddef.h>`
stays unguarded because a freestanding implementation must provide it (C §4).

Why this and not the alternatives:

- **Run the program and print the number.** Correct, simple, and useless when
  cross-compiling. Rejected — it is precisely the autoconf property this design
  exists to avoid.
- **Parse the compiler's error message.** `error: 'probe' declared as an array
  with a negative size` on clang, something else on GCC, something else again
  on MSVC, and all three change between releases. Parsing diagnostics makes the
  build depend on compiler *prose*. Rejected.
- **Encode the value in the object file and read it out.** CMake's
  `check_type_size` does this: emit `char info[] = "INFO:size[00000008]"`,
  compile, and `strings` the object. One compile instead of seven, and it is
  genuinely better — but it needs a per-format object reader (or a `strings`
  that finds the literal reliably across Mach-O, ELF, COFF and archive
  padding), and it is an optimisation of a step that is cached after its first
  run. **Recorded as the intended follow-up, not built now.**
- **`_Static_assert` instead of the negative array.** Cleaner to read, C11 (or
  `static_assert` in C++11), and MSVC supports it. But Harbour supports
  `c_std = "89"`, and the negative-array trick works in C89, C23 and C++ alike
  with identical semantics. Portability wins over elegance for a snippet nobody
  reads.

`alignof` would use the same predicate mechanism with `_Alignof(T)`
substituted, and is one match arm. It is **not** included: no package on the
roadmap asks for it, and adding it now would be a field with no consumer, which
is this repo's signature failure mode.

### `flag`

```c
int main(void) { return 0; }
```

compiled with the candidate flag present. Exit 0 → accepted.

This kind has a trap that must be handled per compiler family, and getting it
wrong makes the probe answer "yes" to everything:

- **Clang** warns rather than errors on an unknown *warning* flag
  (`-Wno-such-thing` → `-Wunknown-warning-option`, a warning). The probe
  therefore adds `-Werror=unknown-warning-option
  -Werror=unused-command-line-argument` where the family is clang or
  apple-clang. Without this, `HAVE_FLAG_WNO_NONSENSE` is true.
- **GCC** errors on an unknown `-f`/`--param` but accepts any `-Wno-*`
  silently, only diagnosing it if some *other* diagnostic fires. The probe adds
  `-Werror`. This is why `AX_CHECK_COMPILE_FLAG` in autoconf-archive is
  notoriously unreliable for `-Wno-` flags, and the honest answer is that a
  `-Wno-X` flag probe under GCC reports "accepted" for flags GCC does not know.
  **Documented as a known limitation of the kind rather than papered over.**
- **MSVC** emits `D9002: ignoring unknown option` as a *warning* and exits 0.
  The probe adds `/WX`. I have no Windows host; this is the design intent and
  is listed in §9 as unverified until the `windows-latest` CI job says
  otherwise.

---

## 2. What is deliberately not a probe kind

This list matters more than the list above, because every entry is something
autoconf has and Harbour is choosing not to have.

**Run probes.** Compile, link, *execute*, and read the exit code or stdout.
This is `AC_RUN_IFELSE`, and it is the single reason autoconf cross-compiles
badly: every `AC_RUN_IFELSE` needs a hand-written
`[cross-compiling fallback]` value, and packages that omitted one simply do not
cross. Excluded by the organising principle. What that costs, precisely: any
question about *runtime behaviour* is unanswerable. `malloc(0)` returning
non-NULL, `mmap` being usable for private fixed mappings, stack growth
direction, whether `snprintf` returns the truncated length, whether the target
libc's `printf` handles `%lld`. See §5 for what a manifest does instead.

**Arbitrary C-snippet probes** (`AC_COMPILE_IFELSE` with a user-supplied
program). This is the tempting one: it subsumes all five kinds above in about
forty lines of code. Rejected for three reasons, in order of weight:

1. It makes `Harbour.toml` a C file. A multi-line C program inside a TOML
   string, with its own escaping, is not something a manifest reviewer can
   audit, and it is not something Harbour can give a good error message about.
2. A declarative kind can be *validated*: `header = "sys/socket.h"` can be
   checked for being a plausible header name, and a failure can say "the header
   `sys/socket.h` was not found; the compiler said: ..." A snippet failure can
   only say "your program did not compile".
3. It is unbounded, which means the fingerprint cannot be reasoned about and
   the cache key becomes "the hash of some C". The five kinds have small,
   enumerable specs.

The cost is real and worth stating: a package needing a check outside the five
kinds has no probe for it. The escape hatch is a literal define under a
`[[targets.X.when]]` condition — a human asserting the answer per platform,
*visibly, in the manifest*, where a reviewer can see it is an assertion rather
than a measurement. That is strictly better than the status quo, which is 793
lines of assertion in a vendored header with no marker distinguishing the
measured from the guessed.

**Endianness.** The classic autoconf endian check is a run probe. It is also
unnecessary: the target triple already answers it, and
`__BYTE_ORDER__`/`__ORDER_LITTLE_ENDIAN__` answer it at preprocess time for
every compiler Harbour supports. **Do not probe what the triple already knows**
is a general rule here — it applies to word size (partly), OS, ABI and
architecture features.

**`pkg-config` invocation.** That is delegation to another build system's
metadata, and it belongs to dependency resolution, not to probing.

**Probes that depend on other probes' results.** Autoconf checks are ordered
and `AC_CHECK_FUNCS` routinely runs after an `AC_CHECK_HEADERS` that set up
`CPPFLAGS`. Harbour's probes are evaluated against one fixed pre-probe compile
surface and **cannot see each other's answers** (§4). This removes an entire
dependency graph, an entire class of ordering nondeterminism, and the need for
a probe-expression language. The cost: "check for `X` only if header `Y`
exists" is not expressible. In practice it degrades correctly — a `symbol`
probe naming a header that does not exist fails to compile and answers
*false*, which is the answer you wanted.

**A `configure_file`-style template.** curl ships `lib/curl_config.h.cmake`,
and substituting into it would be the fastest route to a 793-line header. It is
rejected for v1: the template dialects differ (`#cmakedefine` vs autoconf's
`#undef` vs `@VAR@`), consuming upstream's template re-couples Harbour to
upstream's build system in exactly the way the owner ruled out, and the header
Harbour generates from its own declared probe set is auditable line by line
against the manifest. §3 describes what is generated instead.

---

## 3. The manifest surface

### Declaring probes

A table under the target, keyed by **the name of the answer**:

```toml
[targets.curl.probes.HAVE_SYS_SOCKET_H]
header = "sys/socket.h"

[targets.curl.probes.HAVE_STRERROR_R]
symbol = "strerror_r"
prelude = ["string.h"]

[targets.curl.probes.SIZEOF_LONG]
sizeof = "long"
```

The key is the result name because that is the thing the package's C code
mentions, and because it forces every probe to have exactly one name. The value
is a one-of: exactly one of `header`, `symbol`, `type`, `sizeof`, `flag` must be
present, and supplying two is an error naming both. This is spelled as five
optional fields with a hand-rolled exactly-one check rather than as a serde
`untagged` enum, because §2.8 of the schema audit established that `untagged`
silently swallows unknown keys — three of this repo's ten audited defects were
`flatten`/`untagged` holes, and a new one is not being opened.

`[targets.X.probes]` is a target-level table, deliberately alongside `prebuild`
and not inside `surface`. A probe is a **build input** — a fact Harbour
measures in order to compile this target — and per the audit's §3.2
distinction, build inputs live on the target while the exported contract lives
in `surface`. A probe result that must reach *consumers* is a separate
question, answered under "Consuming" below.

### Bulk shorthand

Declaring curl's 91 header checks as 91 four-line tables is not defensible
ergonomics. So:

```toml
[targets.curl.probes]
check_headers  = ["sys/socket.h", "sys/ioctl.h", "netinet/in.h", "poll.h"]
check_symbols  = ["strerror_r", "gettimeofday", "sigaction"]
check_sizeof   = ["long", "size_t", "time_t", "off_t"]
```

with a fixed, documented naming rule: uppercase, every non-alphanumeric
character becomes `_`, runs collapse, and the prefix is `HAVE_` for
headers/symbols/types or `SIZEOF_` for sizes. `sys/socket.h` →
`HAVE_SYS_SOCKET_H`; `size_t` → `SIZEOF_SIZE_T`; `long long` →
`SIZEOF_LONG_LONG`. This is the universal convention and matches what curl's
own header expects.

One special case, discovered by writing the first fixture against a real
config header: **`*` becomes `P`, not `_`.** autoconf transliterates `*` to
`p` *before* uppercasing, which is why every config header in existence spells
this `SIZEOF_VOID_P`. Treating `*` as ordinary punctuation yields
`SIZEOF_VOID`, and the package's C code then reads a macro nobody defined —
a silent wrong answer rather than an error. Harbour also inserts the
separator itself, so `void*` and `void *` both give `SIZEOF_VOID_P`;
autoconf gives `SIZEOF_VOIDP` for the first, which makes a define name
depend on whitespace for no benefit. Consecutive stars still run together
(`char **` → `SIZEOF_CHAR_PP`), matching autoconf where autoconf is not
being accidental.

**These lists are sugar, and they desugar in the parser.** `check_headers`
expands into ordinary named entries in the same ordered map before anything
downstream sees it, so there is exactly one consumer of "a probe". This is not
a stylistic preference: the audit's headline finding was that *every* one of
its ten defects was one field with two independent consumers that had drifted.
A second execution path for the shorthand form would be the eleventh.

A shorthand entry whose generated name collides with an explicitly named one is
an error, not a silent override.

### Conditional probes

`[[targets.X.probes.when]]` is **not** provided, and the omission is
deliberate. A probe is already conditional on the platform in the only way that
matters: it asks the real toolchain. `check_headers = ["windows.h"]` is
correct on every platform — it answers false on Linux — and writing it under
`os = "windows"` would add a second mechanism for the thing probes exist to
replace. If a probe is genuinely meaningless on a platform (it names a library
that only exists there), the `symbol` probe's `libs` failing to resolve answers
false, which is again correct.

### Consuming: defines

```toml
[targets.curl.probes]
emit = "defines"           # the default
visibility = "private"     # or "public"
check_headers = ["sys/socket.h"]
```

Every true probe becomes a define on the target's compile surface at the chosen
visibility. A `header`/`symbol`/`type`/`flag` probe that answered true emits
`NAME=1`; one that answered false emits **nothing** (not `NAME=0`), because
`#ifdef HAVE_X` is what C code writes. A `sizeof` probe emits `NAME=<value>`
unconditionally — a size of 0 is a probe failure, not an answer (§6).

`visibility` defaults to `private`. Public is offered because a library whose
*public header* is `#ifdef`'d on a probe result genuinely needs the consumer to
see the same answer, and the alternative (the consumer re-probing) would be a
second source of truth. Public probe defines land in `AbiSurfaceKey`
(`src/core/abi.rs:47`) like any other public define, and therefore in the ABI
cache key, which is correct: they *are* part of the compiled interface.

> **Correction, from implementing it.** The paragraph above is wrong, and
> `visibility` is not in the shipped schema. It was built exactly as
> described — parsed, branched on in `BuildPlan`, folded into
> `AbiSurfaceKey` — and it propagates nothing. A dependent's compile surface
> is folded from each dependency's *declared* `surface.compile.public`
> (`surface_resolver.rs`, the `dep_resolved.compile_public` arm), and a
> measured answer exists in no manifest. The consumer fails to compile on an
> undefined `SIZEOF_LONG` while, from the library's side, the field looks
> correct.
>
> This is the most instructive thing the vertical slice found, because the
> field had *every* outward sign of being wired: a schema entry, a match arm,
> and a cache-key update. None of that is evidence — only a consumer that
> tries to use the define is. The key was removed rather than shipped, and
> `visibility = "public"` is now a hard error. Making it work means feeding
> answers back into the resolver's view of a dependency, which is a change to
> the surface fold and belongs in its own change.
>
> A `libs = [{ kind = "package" }]` entry reached exactly this state and sat
> there for months (§2.7 of the schema audit). The only thing that stopped it
> repeating here was writing a test with a real consumer in it.

### Consuming: a generated header

A flag list cannot express 793 lines, and curl and openssl both `#include` a
config header by name.

```toml
[targets.curl.probes]
emit = { header = "curl_config.h" }
defines = ["OS=\"harbour\"", "CURL_DISABLE_LDAP=1"]   # literal, not probed
check_headers = [...]
```

Harbour writes the header itself, into the build tree:

```
.harbour/<profile>/<triple>/probe/<package>/<target>/curl_config.h
```

and adds that directory to the target's **private** `include_dirs`, at the
front. Three properties of that path are each deliberate:

- **In the build tree, not the source tree.** Probing must not dirty a vendored
  checkout, and a git-sourced package's tree is shared across builds.
- **Under the triple and profile.** Two triples built from one checkout must not
  stomp each other's config. This is the 32-bit-Linux-gets-`SIZEOF_LONG 8` bug,
  structurally prevented rather than remembered.
- **Per package and target**, because `curl_config.h` is a name two packages
  can both want.

Content is generated deterministically, in declaration order:

```c
/* Generated by Harbour. Do not edit. */
/* target: curl/curl  triple: aarch64-apple-darwin  toolchain: apple-clang 17.0.0 */
#ifndef HARBOUR_PROBE_CURL_CONFIG_H
#define HARBOUR_PROBE_CURL_CONFIG_H
#define OS "harbour"
#define CURL_DISABLE_LDAP 1
#define HAVE_SYS_SOCKET_H 1
/* #undef HAVE_WINDOWS_H */
#define SIZEOF_LONG 8
#endif
```

A false probe is emitted as a commented-out `#undef`, matching what autoconf
and CMake produce, so a human diffing Harbour's output against the vendored
file sees the same shape. The `/* #undef */` line is *not* optional decoration:
it is the record that the question was asked and answered no, which is the
difference between a probe subsystem and a header that forgot something.

`emit` may be both at once (`emit = ["defines", { header = "..." }]`) for a
package that needs a config header *and* wants a couple of the answers on the
command line. That is one field with one consumer that happens to be a loop.

---

## 4. Where this sits in the build

Probes run **during planning**, per package per target, at
`src/builder/plan.rs` between the surface fold (`:520-525`) and the prebuild
generator loop (`:566-593`). The precise ordering, and why each edge is where
it is:

```
for pkg in topological_order:                     # plan.rs:373
  for target in targets:                          # plan.rs:392
    resolve_compile_surface / resolve_link_surface # :520-523   \
    merge_vcpkg_dirs                               # :525       | pre-probe surface
    >>> RUN PROBES <<<                             # new
    inject probe defines + generated-header -I     # new, mutates compile_surface
    run prebuild generators                        # :567-593
    resolve sources (glob expansion)               # :602-605
    construct CompileStep / fingerprints           # :776-800
```

- **After surface resolution**, because a probe must see the include path. The
  question "does `zlib.h` exist" is meaningless without the `-I` a dependency
  contributes. This is the one ordering constraint the brief did not state and
  it is the important one: probes need the resolved *compile surface*, which is
  a different and earlier thing than source resolution.
- **Before the prebuild generators**, so a generator can eventually be handed
  probe answers. (It is not handed them in v1 — see §8.)
- **Before source resolution**, so that a future probe-conditional source list
  is possible, and because the brief requires it.
- **Before compile-command construction**, which is the whole point: the
  defines must be on `compile_surface` before `CompileStep` is built at
  `:784-797`, and `EffectiveCompileSurface` is already `&mut` at this seam and
  already mutated in place by `merge_vcpkg_dirs` — so injecting there follows
  an existing precedent rather than inventing a mechanism.

The pre-probe surface is the surface *without* any probe results, which is what
makes "probes cannot see each other" (§2) true by construction rather than by
policy: there is no point in the pipeline at which a probe could observe another
probe's define.

**Generators were recently moved into planning for exactly this reason** — the
comment at `plan.rs:540-566` argues that a step producing build inputs must run
before the plan that consumes them can be computed. Probes inherit that
argument and its accepted consequence: `harbour build --plan`, `harbour flags`
and `harbour linkplan` all run probes, because none of them can report the
right answer without them. That is a feature for `harbour flags` — it is
supposed to be authoritative — and it means `harbour flags` is a first-class
way to inspect probe results without building.

Because the package walk is topological, a dependency's probes are answered
before any dependent is planned.

---

## 5. Cross-compilation semantics

**The invariant: every probe kind in §1 is answerable when cross-compiling.**
That is not a happy accident, it is the selection criterion. Four of the five
kinds need only a cross *compiler*; `symbol` needs a cross *linker* and a
sysroot with libraries in it.

Probes are invoked with the same toolchain and the same target flags as the
real build: `ctx.toolchain()`, `ctx.target`, `ctx.target_cflags` (which for a
cross target carries `-target`/`-mcpu`/`--sysroot` and is not optional —
`src/builder/context.rs:60-68` explains why), plus the target's resolved
`include_dirs`, `defines` and `cflags`. A probe compiled with different flags
from the build is a probe answering a different question, which is how
configure results and build results famously drift apart.

Probes are **not** given `profile_cflags`. `-O2` and `-g` do not change whether
a header exists, and including them would make the debug and release builds
maintain two probe caches with identical contents. Sanitizer flags are the
arguable exception (`-fsanitize=address` can change what links); they are
excluded and the exclusion is recorded here rather than discovered later.

### What happens to a question that cannot be answered without running

It is not a probe kind, so the manifest cannot ask it. There is no
"cross-compiling fallback value" field, because a field whose value is a guess
is the vendored `curl_config.h` with extra steps. The answer is:

- Express the fact as a literal `defines` entry under a
  `[[targets.X.when]]` condition. It is then visibly a human assertion keyed on
  a platform, in the manifest, where review can see it.
- If the probe kinds genuinely cannot cover a package, that package needs a
  feature, and it should be filed rather than smuggled in behind a snippet
  escape hatch.

### When the cross linker is missing

`symbol` probes cannot run without one. Three candidate policies, and the
choice:

1. *Answer false.* Rejected outright. This is the disaster case: a missing
   linker makes every `HAVE_<function>` false, the package configures itself as
   though it were running on a 1988 Unix, and it compiles. Exit 0, wrong
   output — the exact failure shape this repo keeps rediscovering.
2. *Hard error.* Correct, and the default.
3. *Degrade `symbol` to compile-only.* Answers "is it declared" instead of "does
   it link", which is a different question with the same name. Rejected: a
   silently different question is worse than a loud failure.

The error is produced by a **link baseline** (§6) rather than by inspecting the
answers, so it fires once with the linker's own diagnostics attached, before any
`symbol` probe reports anything.

---

## 6. Failure, determinism, and the baseline

### Three outcomes, not two

A probe compile can end three ways, and conflating the last two is the standard
way a configure system produces garbage:

| outcome | meaning |
|---|---|
| compiler exits 0 | **true** |
| compiler exits non-zero having produced diagnostics | **false** — a real answer |
| compiler could not be run, died on a signal, or timed out | **error** — not an answer |

The third case is `std::io::Error` from spawn, a signal-terminated child, or a
timeout, and it aborts the build naming the probe and attaching the captured
stderr. A probe never silently becomes false because the toolchain is broken.

A `sizeof` bisection whose *whole range* fails — `sizeof(T) <= 64` is
false — is an error, not a size of zero. So is a type that does not exist: you
asked for its size, and "0" would be a lie a `#define SIZEOF_FOO 0` could not
be distinguished from. Use a `type` probe to ask whether it exists.

### The baseline, which is the real safeguard

Before any probe runs, for each toolchain, Harbour compiles `int main(void)
{ return 0; }` — and, if any `symbol` probe exists, links it — with exactly the
flags probes will use. If the baseline fails, the build stops with the
compiler's output.

This one check is worth more than the rest of the error handling combined.
"Every probe is false because the compiler is broken / the sysroot is missing /
a `cflags` entry the manifest added is rejected" is autoconf's most common
catastrophic mode, and it produces a build that succeeds with a config
describing a machine that does not exist. The baseline turns it into a
one-line error.

### Determinism

The brief is blunt about this and it is justified: a measured 18 distinct link
orders across 40 clean runs of one manifest was fixed days ago, caused by
`HashMap` iteration leaking into build output.

- Probe declarations are stored in `DeclOrderMap` (`src/core/manifest.rs`),
  already used for `Target::deps` for exactly this reason and already
  documented there as chosen because the map is *walked*, not just looked up.
  **No `HashMap` anywhere in the probe path.**
- Emitted defines and generated-header lines are in declaration order —
  manifest order for named probes, list order for shorthand, literal `defines`
  first.
- Probe execution is **serial** in the implementation. Probes are independent
  by §2 and could be parallelised, and if they are, results must be collected
  by index rather than by completion order — but that is a future change, not
  a claim about today. Serially, curl's 199 spawns are a one-time cold cost
  measured in seconds, and the 67 spawns of the vertical-slice fixture take
  under three.
- Scratch files go in a per-probe unique directory, so parallel probes cannot
  race over a shared temp path. This repo has already shipped that bug once:
  `7fbdcc7 fix(toolchain): stop MSVC detection racing itself over a shared temp
  file`, which corrupted archives and forced full rebuilds.
- A regression test runs a probe-using fixture N times from clean and asserts
  the generated header is **byte-identical** and the captured compiler argv is
  identical.

Determinism has a boundary worth stating rather than overclaiming: probe
answers are as reproducible as the build itself and no more. `CPATH`,
`C_INCLUDE_PATH` and `SDKROOT` in the environment change what the compiler
finds, and they change it identically for the probe and for the real compile.
That is the right relationship — a probe that ignored the environment the build
respects would be *less* correct — but it means "same manifest plus same
toolchain" is precisely, not loosely, the guarantee.

---

## 7. Caching, and the fingerprint

Probes cost compiler invocations: curl's 150 compile/link probes plus 7 sizeof
bisections at 7 compiles each is 199 compiler spawns. That must happen once,
and it must not happen once too few times.

### The probe cache

One JSON file per (package, target), under the existing per-triple,
per-profile output directory:

```
.harbour/<profile>/<triple>/probe/<package>/<target>/probes.json
```

```json
{
  "toolchain_key": "a1b2c3d4e5f60718",
  "surface_key": "0f1e2d3c4b5a6978",
  "probes": { "HAVE_SYS_SOCKET_H": { "spec": "9f8e...", "value": true } }
}
```

- `toolchain_key` is **`ToolchainFingerprint::hash()`**
  (`src/builder/fingerprint.rs:149`), reused verbatim rather than reinvented.
  It already covers the target triple, compiler family, a hash of the compiler
  *path*, the compiler version string, the C++ standard and runtime, exceptions
  and RTTI, and the Harbour version. Reusing it means a toolchain change that
  invalidates compiles also invalidates probes, by construction, with no second
  definition of "the toolchain changed" to drift.
- `surface_key` hashes the pre-probe compile surface (include dirs, defines,
  cflags, in order) plus `target_cflags` and the target's `c_std`. A dependency
  that starts exporting a new `-I`, or a package that pins a stricter dialect,
  changes what `HAVE_FOO_H` answers, so it must invalidate. Note what this
  does **not** cover — see the correction below.
- `spec` is a hash of the individual probe's canonical spec. Per-probe rather
  than whole-file, so editing one probe in a 400-probe manifest re-runs one
  probe.

On load: if `toolchain_key` or `surface_key` differs, **discard everything**.
Otherwise keep only entries whose `spec` matches and whose name is still
declared. Nothing is ever merged from a file with a different key — the
failure mode being avoided is a cache holding two generations of answers at
once, which is the shape of `e860627` (`fix(fingerprint): key the cache stably
before the artifact's directory exists`, where two spellings of "canonical"
put two entries per artifact in the cache and every rebuild missed).

`harbour clean --probes` removes it. Probes re-run on cache miss only; there is
no "always re-run" mode, unlike prebuild generators, because a probe *is*
fingerprintable — its inputs are the toolchain and the spec, and both are
known. This is the substantive difference from `prebuild`, whose doc comment
(`plan.rs:172-180`) says plainly that its inputs "are not modeled ... so there
is nothing sound to fingerprint it against".

### Correction: the cache is keyed on declarations, not on the filesystem

The three keys above are everything Harbour *declares*. None of them is the
content of the filesystem, and the paragraph on `surface_key` overstated its
own coverage: it covers a dependency exporting a **new** `-I` — the path
changes, so the key changes — and it does **not** cover a file appearing or
disappearing behind a path that was already there.

Measured, with `vendored` on the include path from the first build so that
nothing in the manifest ever changes:

```
build 1, appears_later.h absent     answer=no
build 2, appears_later.h created    answer=no     <- stale
build 3, after `clean --probes`     answer=yes
build 4, appears_later.h removed    answer=yes    <- stale, and the worse direction
build 5, after `clean --probes`     answer=no
```

`0 → 0 → 1`. Installing a system header, or changing SDKs without changing the
compiler's version string, leaves the previous answer in place.

**This is not a bug to cache around, and the reason is worth stating once.**
The input to a *negative* answer is the **absence** of a file. There is no
finite set of paths to watch for invalidation — you would have to watch every
directory on every search path, for every name no probe found, which is the
whole include path. The only alternative to declaration-keyed caching is
re-running every probe on every build: 199 compiler spawns for curl, on every
`harbour build`, including `--plan` and `harbour flags`. CMake's
`CMakeCache.txt` and autoconf's `config.cache` make the same trade for the
same reason.

So it is a documented limitation with a targeted escape hatch.
`harbour clean --probes` re-measures probes and **keeps compiled objects**,
which is the point of its existing at all — "my probe answer is wrong and the
only fix is a full rebuild" is a bad failure mode for someone iterating on a
shim, and `clean --all` would discard every object to fix one `#define`.

Pinned by `probe_answers_are_cached_against_declarations_not_the_filesystem`,
a characterization test in both directions. It asserts the behaviour Harbour
actually has and says in its own message what to update if someone implements
filesystem-sensitive invalidation — the same shape as
`probe_answers_do_not_reach_a_dependent`, which is what stopped
`visibility = "public"` from shipping.

### Reaching the build fingerprint

This is where a probe subsystem gets stale answers into a cached artifact, so
it is worth being exact about the two paths.

**Defines.** A probe define lands on `EffectiveCompileSurface.defines` before
`CompileStep` is constructed, so it flows through
`NativeBuilder::compile_fingerprint_flags` (`src/builder/native.rs:181-192`)
into `CompileFingerprint.flags_hash`. Verified-by-reading claim that must be
verified-by-running in Phase 2: *change a probe answer, and every affected
object recompiles.* The audit's note on public defines
(commit `80bc2d5`) establishes that defines on the compile line do reach the
flags hash, so the mechanism exists; the wiring is what needs proving.

**The generated header.** Two independent reasons it invalidates, and both are
wanted:

1. `collect_header_deps` (`src/builder/fingerprint.rs:212`) textually scans
   `#include` directives against the include dirs and hashes every header it
   finds into `CompileFingerprint.header_hashes` (a `BTreeMap`). The generated
   config header is in an include dir and is `#include`d by name, so its
   *content* is hashed. Change a probe answer, the header changes, the objects
   recompile.
2. The header's own path is in `include_dirs`, hence in `flags_hash`.

Path (1) is the load-bearing one and it is also the one with a caveat:
`collect_header_deps` skips headers it cannot find on disk
(`fingerprint.rs:319-324`), so the generated header must exist before
fingerprints are taken. It does — probes run at `plan.rs:~525`, fingerprints at
execution. But this is a real ordering dependency and it gets a test.

**Public probe defines** additionally enter `AbiSurfaceKey`
(`src/core/abi.rs:67`) and therefore the archive/link ABI key, so a consumer
relinks. That path was dead until `80bc2d5` wired it; it is live now.

### What is honestly *not* covered

`ToolchainFingerprint` hashes the compiler's *path* and its *version string*.
Swap the binary at that path for a different build reporting the same version,
and neither changes. That is a pre-existing hole in this repo's caching, not
one probes introduce — but probes make it sharper, because a stale `HAVE_X`
is a wrong `#define` rather than a stale `.o`. Recorded, not fixed here;
hashing the compiler binary's content is the fix and it is a change to
`ToolchainFingerprint`, not to probes.

---

## 8. Overlap with what already exists

### `prebuild` / `CustomCommand` / `CustomStep`

Probes **do not subsume** `prebuild`, and the boundary is crisp:

| | `prebuild` | probes |
|---|---|---|
| what it does | runs an arbitrary program that writes files | asks the toolchain a fixed question |
| inputs modelled | no (`CustomCommand::inputs` is advisory, unused) | yes (toolchain key + spec) |
| fingerprinted | no — re-runs every build, by design | yes — cached |
| cross-safe | only if the generator is | by construction |
| answers `HAVE_X` | only by running `configure` (= delegation) | yes |

Probes subsume exactly one *use* of `prebuild`: the one nobody has actually
built, which the extensibility audit describes as "running the package's own
`configure` as a `prebuild` step (possible today in principle; nobody has done
it)". That route is the delegation the owner ruled out, so probes are not
taking anything away — they are the reason it never needs to be taken.

One field on `CustomCommand` is worth flagging as adjacent dead weight:
`inputs` is documented "for fingerprinting" and nothing fingerprints it
(`plan.rs:172-180` says so explicitly). It is not mine to delete in this stack,
but it is the same class as the eleven dead fields another agent is working on
under issue #102, and probes are the reason the comment's promise will stay
unkept.

### `tools/harvest/harvest.py`

Probes subsume **one step of the harvest pipeline and none of harvest.py
itself**. harvest.py reads *which files compile with which flags* out of an
already-configured upstream build (openssl's generated `Makefile`, or a cmake
`compile_commands.json`), intersects defines across objects, keeps openssl's
four product groups apart, and layers the differences into `[[when]]` blocks on
the coarsest condition that holds. No probe can answer any of that — it is
source enumeration, not toolchain interrogation.

What probes delete is `.github/workflows/harvest.yml:76-86`, the step whose own
comment says "The generated config header is the reason this job exists: it is
the part that genuinely cannot be produced on another platform" — and with it
the `harbour-config/<os>-<arch>/` directories and the need to run that job on
each target OS in order to obtain a header. harvest's cross-platform matrix
still earns its keep for *sources and flags*; it stops being needed for
*config*.

### Coordination with concurrent work

Two other agents are touching flag emission. Overlap, stated rather than pushed
through:

- **Issue #100 (MSVC profile-flag translation, `src/builder/context.rs` and the
  toolchain backends).** Probes call `ctx.toolchain()` to build their compile
  command and deliberately do *not* use `profile_cflags`, so the profile-flag
  translation surface is not shared. The `flag` probe kind does need MSVC's
  `/WX`-for-`D9002` behaviour, which is adjacent to #100's territory; whoever
  lands second should reconcile in `msvc.rs`.
- **Issue #102 (eleven dead manifest fields, possibly `c_std`).** Probes add a
  new manifest field and must not be one of the twelve. That is what Phase 2's
  argv evidence is for. If #102 implements `c_std`, the probe compile should
  pick it up automatically via the compile surface; that is a one-line check to
  re-run after #102 lands, not a conflict.

---

## 9. Staging, and what Phase 2 must prove

Not one PR. The stack:

1. **This design document.** (No code.)
2. **Probe engine + `header` and `sizeof` kinds + `emit = "defines"`**, wired
   through the manifest, into the fingerprint, into real compile flags, with the
   probe cache. The narrowest path that is genuinely end-to-end.
3. `symbol`, `type`, `flag` kinds; the link baseline.
4. `emit = { header = ... }` and the curl config replacement.

**Phase 2's success criterion is not a passing test.** It is:

- The captured compiler argv for a real build contains the probe-derived
  `-D` flags and nothing else changed.
- The probed values agree, question by question, with what curl's own
  configure output asserts, on `macos-aarch64` and `linux-x86_64` — the latter
  under Docker. Not "the 793-line file was reproduced": a dozen-odd specific
  questions whose right answers are known independently, including at least one
  that *differs* between the two platforms (`HAVE_LINUX_TCP_H`,
  `HAVE_SYS_EVENTFD_H` — both absent on macOS, present on Linux), because a
  probe subsystem that returns the same answer everywhere is indistinguishable
  from a constant.
- Changing the toolchain or a probe spec re-runs probes; changing neither does
  not.
- A probe answer change recompiles the affected objects, watched failing before
  the fix exists.
- The three regression fixtures (cJSON, zstd, libuv) still build and run to
  exit 0.

### Unverified at design time, and flagged as such

- **Every MSVC claim in this document.** `/WX` making `D9002` fatal, the
  negative-array trick's exact behaviour under `cl`, whether `cl /c` on a
  generated `.c` in a temp directory behaves under the MSVC detection logic's
  existing temp-file discipline. There is no Windows host here; the
  `windows-latest` CI job is the instrument and its log, not its tick, is the
  evidence. Guessing MSVC behaviour produced two wrong expectations in this
  repo last week.
- **Whether `collect_header_deps`'s textual `#include` scan finds the generated
  config header** in practice — it depends on the scan reaching a header behind
  a nested include, and it is a textual scanner, not a preprocessor. Phase 4's
  problem, listed here so it is not a surprise.
- The `-Wno-*`-under-GCC limitation of the `flag` kind is inferred from
  autoconf-archive's documented experience, not measured here.

### Corrections to the brief

**The curl shim is gone.** The brief points at
`scratchpad/rung3/src/curl-curl-8_22_0/Harbour.toml` and its vendored
`harbour-config/{macos-aarch64,linux-x86_64}/`. The directory skeleton is
present and every file in it has been deleted: `du -sh` on the tree reports
`0B`, `find` for `curl_config.h` across the scratchpad and the repo returns
nothing, and the scratchpad was never tracked, so there is no git recovery.
`/private/tmp` is purged by macOS after a few days and those files were written
on 2026-09-07.

The shim's *shape* is recoverable from documentation (60 manifest lines, two
793-line headers, the exact cmake options at `harvest.yml:52-63`), and the
config header is recoverable by re-running those options — which is what every
figure in this document was measured against, and which reproduces a 793-line
file, confirming both the provenance of the vendored one and that this is a
faithful oracle. The *manifest* is not recoverable and would have to be
rewritten. That does not change the design; it changes the Phase 4 estimate,
and it is why Phase 2 validates against specific known-answer questions rather
than against a file diff.

**The three regression fixtures are also gone**, for the same reason:
`scratchpad/audit/{cjtest,zstdtest,uvtest}` survive as directories with no
`Harbour.toml` and no sources, so a build there fails `no manifest found` —
which looks like a Harbour regression and is not one. The manifests are
embedded in `docs/superpowers/specs/2026-09-07-extensibility-audit.md` and must
be reconstructed from there before the regression check means anything. A
scratchpad under `/private/tmp` is not a regression suite; these three fixtures
build real third-party packages and belong in the repo's own test tree, which
is a recommendation this design makes but does not act on.

---

## 10. Corrections, from building curl

Added after Phases 3 and 4 landed — `type`, `constant` and `flag`, the
106-question oracle, and `ci/canary/curl/`, which builds curl 8.22.0 with no
vendored config header. Each entry below is something this document asserts
that turned out to be wrong or incomplete, kept here rather than silently
edited into the text above, because the estimates were quoted as measured.

**curl answers 252 questions, not 253.** 110 `#define` and 143
`/* #undef */` is 253 *lines*, but `CURL_EXTERN_SYMBOL` occupies two of them
— once with a value and once as an `#ifndef` fallback. Every count in §"Scope
check" is one too many for the same reason. Found by parsing the file into a
dictionary and counting keys.

**The kind table in §"Scope check" undercounts and overcounts.** Measured
against the same oracle: 35 `header`, 54 `symbol`, 7 `sizeof`, 6 `type`,
6 `constant`, and 93 — not 98 — project options. The `type` row said 2; the
six are `HAVE_BOOL_T`, `HAVE_SA_FAMILY_T`, `HAVE_SUSECONDS_T`,
`HAVE_STRUCT_TIMEVAL`, `HAVE_STRUCT_SOCKADDR_STORAGE` and
`HAVE_SOCKADDR_IN6_SIN6_SCOPE_ID`.

**There is a sixth kind: `constant`.** §1 says "Five." The `O_NONBLOCK` /
`FIONBIO` / `CLOCK_MONOTONIC` question is not any of them, and it is not a
variant of one:

- Not `symbol`. That kind's distinguishing act is that it *links*, and a
  macro or an enumerator has no linkage. A `link = false` knob would be the
  "silently different question under the same name" §5 rejects.
- Not `type`. A `type` probe declares a variable, so
  `type = "O_NONBLOCK"` is `O_NONBLOCK probe_value;`.

It meets the admission criterion in §1 on its own terms — one declarative
field, answered by compiling, therefore answerable when cross-compiling — and
unlike the `alignof` kind §"sizeof" declined to add, it has six consumers in
the package this design exists for. Its snippet uses the name where only an
integer constant expression is legal (an enumerator's initialiser), which is
what keeps it from degrading into a compile-only `symbol` check.

The measurement that settled it: **`CLOCK_MONOTONIC` is a macro on glibc and
an enumeration constant on macOS.** `symbol`'s macro branch is
`#if defined(...)`, which sees the first and not the second, so a `symbol`
probe answers yes on Linux and no on macOS for something both platforms have.

**The `-Wno-*`-under-GCC limitation of the `flag` kind is fixable, not just
documentable.** §1 records, following `AX_CHECK_COMPILE_FLAG`, that "a
`-Wno-X` flag probe under GCC reports 'accepted' for flags GCC does not
know". Reproduced under GCC 13:

```
gcc -Werror -Wno-harbour-nonsense   -> exit 0     the trap, confirmed
gcc -Werror -Wharbour-nonsense      -> rejected   the way out
gcc -Werror -Wunused                -> accepted
```

GCC is loud about the *positive* spelling and has no warning it can disable
but not enable, so the probe asks `-W<name>` and reports the answer for
`-Wno-<name>`. Only GCC is rewritten; clang diagnoses the negative form
directly under `-Werror=unknown-warning-option`.

Also measured, and worth recording because it constrains the implementation:
`gcc -Werror=unknown-warning-option` **fails** with *no option
`-Wunknown-warning-option`*. The guard flags must be per family; a unified
list would make every GCC flag probe answer `no`.

**The `flag` kind has no consumer, and the design does not notice.** Its
answer arrives as a `#define`. Nothing in curl's 793-line config header is a
compiler-flag question — curl *does* test flags, in
`CMake/PickyWarnings.cmake`, and uses the answers to build its own `CFLAGS`,
never to define anything. That is what a flag check is for everywhere else
too. So `flag` is implemented, correct and inspectable, and it is a kind
whose answer nobody wants in the form it is delivered. Making it useful needs
an `emit` mode that puts an accepted flag on the compile line, which is a
change to §3 and has not been made.

**There is a category §2 rejects by name but does not size: arbitrary
compile-time predicates.** Five of curl's questions are
`check_c_source_compiles` over programs with no declarative shape —
`HAVE_ATOMIC`, `HAVE_BUILTIN_AVAILABLE`, `HAVE_DECL_FSEEKO`,
`HAVE_TIME_T_UNSIGNED`, `HAVE_GETADDRINFO_THREADSAFE`. Together with the
seven arity questions (the canary's `regenerate.md` said five; seven is the
number of *defines*) they account for **ten literal assertions** in
`ci/canary/curl/Harbour.toml` — four that hold on both platforms and six
under `[[targets.curl.when]]` blocks — against 108 measured answers. Ten is
the honest price of refusing the snippet probe, and it is worth paying.

Ten rather than twelve because a false answer is the *absence* of a define:
`HAVE_TIME_T_UNSIGNED` and three of the five `gethostbyname_r` /
`fsetxattr` spellings are false on both platforms and so need no line at
all.

**§4's claim that the generated header reaches the compile fingerprint is
confirmed by construction rather than by the textual scan.** `curl_setup.h`
includes `curl_config.h` with quoted-include semantics from a nested header,
and §9 lists as unverified "whether `collect_header_deps`'s textual
`#include` scan finds the generated config header". It does not need to for
curl: the header's directory is in `include_dirs`, hence in `flags_hash`,
and a changed probe answer changes the defines Harbour computes — so the
rebuild happens by the path §7 calls the non-load-bearing one. The textual
scan through a nested include remains unverified, and remains a thing to
verify rather than a thing to assume.

**Probing a package's own headers works, and is more honest than restating a
choice upstream made.** `SIZEOF_CURL_OFF_T` and `SIZEOF_CURL_SOCKET_T` are
measured with `sizeof = "curl_off_t"`, `prelude = ["curl/curl.h"]`, because a
probe is compiled with the target's resolved `include_dirs` and `include/` is
on that list. curl's own cmake computes them by sizing whichever type it
picked; asking the typedef directly asks the same question of the same
headers without the manifest having to know the answer.

**One thing outside probes entirely, found by the link failing.**
`lib/macos.c` reads the system proxy configuration, so curl needs
`-framework CoreFoundation -framework SystemConfiguration` on Darwin.
Nothing in the config header mentions either, so no probe could have found
it, and no amount of correct probing would have. Worth recording because
"curl needs a config header" was the whole framing, and it needed one other
thing.
