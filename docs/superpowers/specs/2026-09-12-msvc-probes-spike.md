# Spike: do native probes work under MSVC?

**Date:** 2026-09-12
**Status:** Spike — findings and a plan. **No production code changes.**
**Subject:** `src/builder/probe.rs`, `src/core/probe.rs`,
`src/builder/toolchain/msvc.rs`
**Preceding design:** `docs/superpowers/specs/2026-09-11-native-probes-design.md`

---

## The one-paragraph version

The probe subsystem works under MSVC far better than the design document
assumed. `header`, `sizeof` and `symbol` all answer correctly on a real
`cl.exe` today, with **no MSVC-specific code at all** — the `__has_include`
preamble fires, the negative-array `sizeof` bisection works, `SIZEOF_LONG`
comes back 4 (not 8), and `link.exe` resolves CRT symbols from the
`/DEFAULTLIB` directives `cl` embeds in the object, with nothing on the link
line. There is exactly **one** real defect, and it is the one the brief
predicted: a `libs` entry is rendered as `<name>.lib`, so the universal Unix
spelling `libs = ["m"]` becomes a nonexistent `m.lib`, `link.exe` fails, and
the probe answers `no` **silently** for a function that is right there. That
is a small, localised fix — a name-translation table — not a redesign of the
`symbol` kind. A second, cosmetic defect was found by accident: the MSVC probe
executable is named `probeexe`, missing its dot.

Every probe integration test in `tests/cli_integration.rs` is
`cfg(not(windows))`. **They do not have to be.** The generated config header is
a real artifact of a real probe run and is a perfectly good witness on Windows;
this spike ships a `cfg(target_env = "msvc")` test that asserts fifteen probe
answers on the `windows-latest` job, which converts every future MSVC probe
claim from inferred to measured.

---

## How to read the evidence labels

Per the brief, every claim below carries one of three labels. Nothing is
blended.

| Label | Means |
|---|---|
| **[MEASURED-MSVC]** | Observed on `windows-latest`, MSVC **14.51.36231** (VS 18 Enterprise), x64, via GitHub Actions run `34708538176` and its successor. The instrument is the *log*, read with `gh run view --job <id> --log-failed`. |
| **[MEASURED-ARGV]** | Observed on macOS by driving `MsvcToolchain` through `probe::run_probes` with a recording shell script standing in for `cl.exe`/`link.exe`. Proves what Harbour *emits*; proves nothing about what `cl` does with it. |
| **[INFERRED]** | Reasoned from documentation or from code reading. Not measured. Treat with suspicion — three inferred MSVC claims in this repo have already turned out wrong (`/EHsc-` vs `/EHs-c-`, `C4101` vs `C4189`, `cl` writing file-level diagnostics to stderr). |

The measurement technique for [MEASURED-MSVC] deserves a note, because it is
reusable and non-obvious: `cargo test` captures child output, so a *passing*
test prints nothing to the CI log. The exploratory run was therefore a test
that collected everything into a string and then `panic!`ed with it. That is
the only way to get a real MSVC transcript out of this repo's CI today.

---

## Verdict per probe kind

### `header` — **works. No caveats.** [MEASURED-MSVC]

`compile_command` is backend-agnostic and `cl /c` exits non-zero on a missing
include, which is all this kind needs.

Generated header from a real MSVC run, verbatim:

```c
#define HAVE_STDIO_H 1
#define HAVE_WINDOWS_H 1
#define HAVE_SYS_TYPES_H 1
/* #undef HAVE_UNISTD_H */
/* #undef HAVE_SYS_SOCKET_H */
/* #undef HAVE_DEFINITELY_NOT_A_REAL_HEADER_H */
```

All six independently known and all six correct. `<sys/types.h>` does exist in
the UCRT and `<sys/socket.h>` does not, exactly as the brief said.

### `sizeof` — **works. No caveats.** [MEASURED-MSVC]

The negative-array-bound bisection is fine under `cl`. Real MSVC answers:

```c
#define SIZEOF_INT 4
#define SIZEOF_LONG 4      /* not 8 -- MSVC keeps `long` 32-bit on x64 */
#define SIZEOF_SIZE_T 8
#define SIZEOF_VOID_P 8
#define SIZEOF_TIME_T 8
#define SIZEOF_OFF_T 4
```

Three separate worries are retired by that block:

- `char probe[(sizeof(T) <= N) ? 1 : -1]` — `cl` accepts the conditional
  operator in an array bound as a constant expression and rejects the negative
  case with a non-zero exit. Seven compiles, correct answer.
- `SIZEOF_LONG 4` is the answer that proves the subsystem is asking *this*
  toolchain and not inheriting a Unix constant.
- `SIZEOF_TIME_T` and `SIZEOF_OFF_T` are only reachable through the
  `__has_include` preamble (see below). A number here *is* the proof the
  preamble fired.

### `symbol` — **works, with one defect that is a silent wrong answer.** [MEASURED-MSVC]

The mechanism is sound on MSVC. Real answers, no `libs` on any of them:

```c
#define HAVE_PRINTF 1
#define HAVE_MEMCPY 1
#define HAVE_MALLOC 1
#define HAVE_STRDUP 1
/* #undef HAVE_STRERROR_R */
/* #undef HAVE_POLL */
/* #undef HAVE_DEFINITELY_NOT_A_SYMBOL */
```

Four points worth recording, because three of them contradict what I expected
before running it:

1. **`link.exe` needs no libraries named for CRT symbols.** `cl` embeds
   `/DEFAULTLIB` directives in the `.obj` (default `/MT`, since
   `compile_command` only emits a runtime flag for C++), and `link.exe` honours
   them. The probe link line is literally
   `link.exe /nologo /OUT:<path>\probeexe <path>\probe.obj` and that resolves
   `malloc`. [MEASURED-ARGV] for the argv, [MEASURED-MSVC] for the answer.
2. **Intrinsics and header-inline CRT functions are not a problem.** I
   predicted `HAVE_PRINTF` would answer `no`, on the grounds that the UCRT
   defines `printf` as a `_CRT_STDIO_INLINE` function in `<stdio.h>` rather
   than exporting it, so the no-prelude fallback declaration (`char
   printf(void);` then `&printf`) would hit LNK2019. **That prediction was
   wrong** — it answers `1`. `memcpy`, an unconditional MSVC intrinsic,
   likewise answers `1`; taking its address forces a real external reference.
   The autoconf fallback-declaration trick survives MSVC intact.
3. **Absent symbols answer `no` rather than erroring.** `poll` and
   `strerror_r` genuinely do not exist on Windows and are recorded as
   asked-and-answered-no. The `#if defined(symbol)` macro branch did not
   misfire on any of these.
4. **The defect.** `MsvcToolchain::link_exe_command` renders each `libs` entry
   as `format!("{}.lib", lib)`. So `libs = ["m"]` — how every Unix manifest on
   earth asks for the math library — becomes `m.lib`, which exists in no MSVC
   installation. Measured, both halves:

   | manifest | MSVC answer |
   |---|---|
   | `symbol = "sqrt"`, `prelude = ["math.h"]`, `libs = ["m"]` | `/* #undef HAVE_SQRT */` |
   | `symbol = "sqrt"`, `prelude = ["math.h"]`, no `libs` | `#define HAVE_SQRT 1` |
   | `symbol = "htonl"`, `prelude = ["winsock2.h"]`, `libs = ["ws2_32"]` | `#define HAVE_HTONL 1` |

   The third row matters as much as the first two: the `<name>.lib` mapping is
   **correct** when the name already is the Windows one. This is a
   name-translation problem, not a link-step problem.

   And the build **succeeded** in all three cases. The linker's `LNK1104` never
   reached the user; the package was simply told there is no `sqrt`. That is
   the catastrophic-quiet failure mode this whole subsystem was built to
   prevent, arriving through the one door nobody had opened.

   Note that the design document explicitly blesses this behaviour, at
   `2026-09-11-native-probes-design.md:384-387`:

   > If a probe is genuinely meaningless on a platform (it names a library that
   > only exists there), the `symbol` probe's `libs` failing to resolve answers
   > false, which is again correct.

   It is correct for "this platform has no `libfoo`". It is wrong for "this
   platform spells `libfoo` differently", and those two are indistinguishable
   from inside `compile_and_link`. The distinction has to be made *before* the
   link, by translating the name.

### The `__has_include` preamble — **works.** [MEASURED-MSVC]

`cl` 14.51 honours `#ifdef __has_include` and the nested
`#if __has_include(<...>)` guards. The exact snippet that ran, recovered from
the CI transcript:

```c
#include <stddef.h>
#ifdef __has_include
#  if __has_include(<stdint.h>)
#    include <stdint.h>
#  endif
#  if __has_include(<time.h>)
#    include <time.h>
#  endif
#  if __has_include(<sys/types.h>)
#    include <sys/types.h>
#  endif
#endif
int main(void) {
    char probe[(sizeof(off_t) <= 8) ? 1 : -1];
    (void) probe;
    return 0;
}
```

No diagnostic, no error, and `off_t` (a UCRT extension in `<sys/types.h>`,
32-bit) resolved. Had `#ifdef __has_include` been false under `cl`, the
preamble would have degraded to `<stddef.h>` alone, `time_t` and `off_t` would
have been invisible, and the probe would have failed the build with
`ProbeError::SizeOutOfRange`. It did not. **Nothing to do here.**

Untested: MSVC versions older than the runner's 14.51. `__has_include` arrived
in VS 2017 15.3 [INFERRED, from Microsoft's documentation], and the
`#ifdef __has_include` guard means an older `cl` degrades rather than errors —
but degrades into a hard build failure for `sizeof(time_t)`, not into a
recoverable state. Raising Harbour's minimum MSVC, or giving `sizeof` a
`prelude`, are the two answers; neither is urgent because no supported VS is
that old.

### `c_std` interaction — **a non-issue.** [MEASURED-MSVC]

Four builds of the same probe set under `c_std = "11"`, `"17"`, `"99"` and
`"89"` produced **byte-identical** generated headers. The two `cl` cannot
express warn exactly as designed:

```
WARN `probed` target `probed` pins `c_std = "c99"`, which `cl` has no `/std:`
option for (it has only `/std:c11` and `/std:c17`). These sources will compile
in cl's default C mode.
```

The reason this is harmless, and it is worth stating because the equivalent is
*not* harmless on GCC: the hazard `ProbeEnv::c_std`'s own doc comment
describes — `-std=c99` defining `__STRICT_ANSI__`, which makes glibc's
`features.h` stop exposing `_DEFAULT_SOURCE` declarations and whole families of
types disappear — has no MSVC analogue. The UCRT headers do not gate
declarations on the C dialect the way glibc and Apple's headers do. `/std:c11`
and `/std:c17` change language features, not header visibility.

One latent inconsistency, not a bug today: the probe cache's `surface_key`
includes `c_std` unconditionally, so on MSVC the cache is keyed on a dialect
that provably cannot change any answer. Editing `c_std` from `"89"` to `"99"`
on a Windows build re-measures every probe for nothing. Cheap, cached, and not
worth a fix that would make the key backend-dependent.

### Bonus defect found by accident — `probeexe` [MEASURED-ARGV]

Every extension accessor on `Toolchain` is dotless (`"obj"`, `"lib"`, `"exe"`,
`""` on Unix), and `TargetKind::output_filename` joins them with an explicit
`.` while special-casing the empty case. `src/builder/probe.rs:1075` does not:

```rust
let exe = dir.join(format!("probe{}", env.toolchain.exe_extension()));
```

On Unix `exe_extension()` is `""`, so this is `probe` and correct. On MSVC it
is `"exe"`, so the file is `probeexe`. The line directly above it gets the
object right (`format!("probe.{}", env.toolchain.object_extension())`), which
is what makes this a slip rather than a convention.

**Impact today: none.** A probe reads the link's exit code and never opens the
executable. Recorded so it is fixed deliberately rather than rediscovered.
Note that `format!("probe.{}", ...)` is *not* the fix — it yields a trailing
dot on Unix.

### Bonus observation — `_strdup` and `strdup` collide [MEASURED-MSVC]

`check_symbols = ["_strdup"]` produced `HAVE_STRDUP`, not `HAVE__STRDUP`:
`sanitize_name` skips a leading separator run. Since MSVC's spelling of many
POSIX names is the underscore-prefixed one, a manifest asking for both
`strdup` and `_strdup` — a plausible thing for a portable package to do —
gets a duplicate-name error from `insert_probe`. Loud, not silent, so it is a
wart rather than a defect. Worth a sentence in `MANIFEST.md`.

---

## Why the probe integration tests are `cfg(not(windows))`, and what to do

The stated reason is correct as far as it goes: the twelve probe tests witness
a compile through `install_cc_recorder`, a `#!/bin/sh` wrapper that dumps its
argv and `exec`s `cc`. That does not transplant:

- A `.bat` wrapper is not a workable substitute. `CreateProcessW` cannot
  execute a batch file directly; Rust's `Command` therefore cannot either
  without going through `cmd.exe /c`. [INFERRED, from Rust std's own
  documentation]
- Even if it could, **`CC` is ignored on Windows whenever MSVC is
  detectable.** `detect_host_toolchain` (`detect.rs:693-705`) tries
  `try_detect_msvc()` first and only reaches `try_detect_gcc()` — the function
  that reads `CC` — if that returns `None`. So a `CC` shim on
  `windows-latest` would not be consulted at all, and if it somehow were, the
  argv captured would be `GccToolchain`'s, not MSVC's. **The `CC` shim is the
  wrong instrument for MSVC twice over.**

  (That `CC` is silently ignored on Windows is arguably its own wart —
  `detect_toolchain`'s error message advertises `CC` as the escape hatch — but
  it is out of scope here and has nothing to do with probes.)

But argv is not the only witness, and for probes it is not the best one. Two
work on Windows today:

1. **`compile_commands.json`.** Already used by
   `profile_flags_reach_the_real_compiler_in_the_toolchains_own_syntax`
   via `recorded_compile_args`, which is ungated and passes on
   `windows-latest`. It records the *package's* compile line, so it witnesses
   probe answers arriving as `/D` flags — but not the probe compiles
   themselves, which happen during planning and are not build steps.
2. **The generated config header.** Written by the real probe run from real
   `cl.exe` and `link.exe` exit codes. This is strictly the better witness for
   probe *answers*, on every platform: it is the artifact, not a proxy for it,
   and it distinguishes `Absent` (`/* #undef X */`) from never-asked.

This spike ships (2) as
`msvc_answers_every_probe_kind_correctly` in `tests/cli_integration.rs`,
asserting eleven positive and four negative answers across all three kinds on
`windows-latest`. It is a live gate, not `#[ignore]`d, and it passed on the
verification run. **That is the highest-value item in this spike:** MSVC probe
behaviour is now measured on every push rather than inferred.

The twelve existing `cfg(not(windows))` tests should mostly *stay* gated —
they assert argv ordering and shim-recorded compile counts, which are genuinely
Unix-shaped questions. The plan below proposes lifting the two that are really
about answers rather than argv.

---

## Implementation plan

Ordered. Each step is independently reviewable, which makes this a natural
`gh stack`.

### 1. Translate `libs` names per backend — the only real fix

**Size: small.** One new function, one call site, a table. Not a redesign.

**Where.** A new `fn map_lib_name(&self, lib: &str) -> LibMapping` on the
`Toolchain` trait, defaulting to `LibMapping::Keep`, overridden in
`src/builder/toolchain/msvc.rs`. **Not** in `probe.rs` — the knowledge is
"what does this linker call this library", which is toolchain knowledge, and
putting it in `probe.rs` would give the build step and the probe step two
different answers to one question. (The build's `link_exe_command`/
`link_shared_command` have the identical bug for real `link` surfaces and
should consume the same mapping; see step 4.)

**The mapping.** Three outcomes, not two, and the third is the important one:

```rust
enum LibMapping {
    /// Use this name. `("ws2_32" -> ws2_32.lib)`
    Named(String),
    /// This library's contents are in the CRT / always linked. Contribute
    /// nothing to the link line -- and that is a *success*, not a silent drop.
    InDefaultLibs,
    /// No idea. Refuse rather than guess.
    Unknown,
}
```

| manifest `libs` entry | MSVC | why |
|---|---|---|
| `m` | `InDefaultLibs` | math is in the UCRT; `sqrt` measured resolving with nothing on the link line |
| `c` | `InDefaultLibs` | the CRT itself |
| `pthread`, `rt`, `dl` | `InDefaultLibs` | no MSVC equivalent exists; the functions they carry either are in the CRT or are absent, and absence is then correctly reported by the *symbol* failing to resolve rather than by a missing file |
| `ws2_32`, `advapi32`, `crypt32`, `user32`, `kernel32`, `bcrypt`, `iphlpapi`, … | `Named("<same>.lib")` | already Windows names; measured working for `ws2_32` |
| anything else | `Named("<same>.lib")` | the current behaviour, and right for a real third-party `.lib` |

Only the first three rows are new behaviour. `Unknown` is in the enum for
step 3 and is unused at first.

The `dl`/`pthread`/`rt` rows are the ones to argue about in review. The claim
is that mapping them to `InDefaultLibs` *improves* the answer: today
`libs = ["dl"]` makes `HAVE_DLOPEN` answer `no` because `dl.lib` is missing,
which is accidentally right for the wrong reason. After the change it answers
`no` because `dlopen` does not resolve — right for the right reason — and the
day someone probes a symbol that *is* in the CRT while naming `libs = ["rt"]`
(`clock_gettime` is the live example), the answer flips from wrong to right.

**Tests.** Unit tests in `msvc.rs` for the table. Then un-`#[ignore]` and
invert `msvc_libs_entry_named_for_unix_makes_a_symbol_probe_answer_no` (this
spike ships it asserting the *current wrong* answer, precisely so the fix has
something to flip).

### 2. Fix `probeexe`

**Size: one line.** `src/builder/probe.rs:1075`. Use the codebase's existing
convention rather than inventing a third one — either `Path::with_extension`
guarded on the empty case, or reuse whatever helper step 4 factors out of
`TargetKind::output_filename`. Add an assertion to the argv test this spike
ships (`msvc_probe_command_lines_are_msvc_shaped_and_libm_does_not_exist`),
which currently asserts the *presence* of `probeexe` for exactly this reason
and will fail loudly when fixed.

**Blocked on the concurrent probe-kinds work**, which owns `probe.rs`. Hand it
over rather than racing it.

### 3. Surface the linker's reason for a `no`

**Size: medium.** The deeper lesson of the `m.lib` finding is not the name
table; it is that a `symbol` probe cannot distinguish "the symbol is absent"
from "the link line was malformed", and reports both as a confident `no`.

`compile_and_link` already has the linker's stderr in hand and throws it away
on the `ok: false` path. Proposal: keep it, and classify. `LNK1104` (cannot
open input file) and `LNK1181` are *not* answers about the target — they are
the probe equivalent of the baseline check failing, and should be an error
quoting the linker, exactly as `check_baseline` does. `LNK2019` (unresolved
external) is a real `no`.

This is where `LibMapping::Unknown` earns its place: an unmappable `libs` entry
becomes a manifest error naming the entry, not a false `no`.

Deliberately **not** merged into step 1: step 1 is a table and lands in an
afternoon; this needs a decision about how much linker prose the design is
willing to depend on, and the design document argues at length
(`:186`) against parsing diagnostics. The counter-argument is that `LNK1104`
is a stable *code*, not prose, and that the alternative is the silent-`no`
class of bug. Worth its own PR and its own argument.

### 4. Reconcile the build's own `libs` handling

**Size: medium, and mostly not about probes.** `link_exe_command` and
`link_shared_command` map `libs` the same wrong way for real link steps, so a
manifest with `libs = ["m"]` in a surface fails to link on Windows with
`LNK1104` — loudly, at least. Same mapping, same function, different call
site. Belongs after step 1 so there is one table.

### 5. Lift the probe tests that are about answers, not argv

**Size: small.** Two of the twelve `cfg(not(windows))` probe tests assert
things that have nothing to do with the `CC` shim:

- `sizeof_probes_agree_with_the_compiler_without_running_a_program` — its real
  witness is the fixture's own `SAME(sizeof(long), SIZEOF_LONG)` negative-array
  assertions, which `cl` honours (measured: the identical construct in the
  `sizeof` probe works). Making `PROBE_FIXTURE_SOURCE` and its manifest
  ungated, and dropping only the `recorded_probe_defines` call, gets this
  running on Windows.
- `a_changed_probe_answer_rebuilds_and_changes_what_the_program_does` — witness
  is the built program's stdout.

`the_probe_cache_survives_a_rebuild_and_dies_with_the_toolchain` needs the
shim (it swaps two `CC` paths to move the toolchain fingerprint) and should
stay gated, or be rewritten around a `probes.json` read.

Do this **last**. `tests/cli_integration.rs` has produced the same rebase
conflict four times running and a split is deferred; touching the probe
fixture constants is exactly the kind of edit that collides with the
probe-kinds work.

### What is explicitly *not* in the plan

- **Nothing for `header`.** It works.
- **Nothing for `sizeof`.** It works, including the preamble.
- **Nothing for `__has_include`.** It works.
- **Nothing for `c_std`.** It works, and it already warns.
- **No `/WX` on probe compiles.** The design document
  (`:228-230`) proposes adding `/WX` so `D9002` becomes fatal, flagged as
  unverified. That is a `flag`-kind concern and it is in tension with
  `ProbeEnv`'s stated reason for excluding the package's `cflags` — that a
  `-Werror` would make every probe answer `no` on an incidental warning.
  Whoever implements the `flag` kind should scope `/WX` to that kind's own
  compile and not to `header`/`sizeof`/`symbol`. No evidence from this spike
  says the other three need it; measured, they emit no diagnostics at all
  under `cl`'s default warning level.

**Overall size: small fix, not a redesign.** Steps 1, 2 and 5 are an
afternoon each. Step 3 is the only genuinely open design question, and it is
about *diagnostics*, not about the `symbol` kind's mechanism — which, measured,
is sound on MSVC.

---

## What this spike ships

- This document.
- `tests/cli_integration.rs`, appended at the end, never restructured:
  - `msvc_answers_every_probe_kind_correctly` —
    `cfg(target_env = "msvc")`, **live**, asserts fifteen probe answers on real
    MSVC via the generated config header. Passed on the verification run.
  - `msvc_libs_entry_named_for_unix_makes_a_symbol_probe_answer_no` —
    `cfg(target_env = "msvc")`, **`#[ignore]`d**, asserts the defect's current
    wrong answer so step 1 has something to invert. Verified passing on
    `windows-latest` by running it un-`#[ignore]`d on the spike branch, then
    re-marking it; the body shipped is the body that was measured.
  - `msvc_probe_command_lines_are_msvc_shaped_and_libm_does_not_exist` —
    `cfg(unix)`, **live**, drives `MsvcToolchain` through `run_probes` with a
    recording shim, so the MSVC probe argv is pinned from any host. Also pins
    `probeexe` and `m.lib` so both defects fail loudly when fixed.

**No changes to `src/builder/probe.rs` or `src/core/probe.rs`.** Both were read
closely and one was edited locally to learn something; neither carries a
production change here. Everything is in the plan above.

---

## What I could not determine

- **The exact `link.exe` error code behind the `m.lib` false negative.**
  `LNK1104` is [INFERRED]. It is not in the CI transcript because
  `compile_and_link` discards the linker's stderr on the `ok: false` path,
  which is itself finding 3 above. The *mechanism* is nonetheless measured, and
  isolated to the library name rather than the symbol, by the three-row table:
  `sqrt` resolves with no `libs` and fails with `libs = ["m"]`, so the only
  variable is the nonexistent input file. Determining the code needs either
  step 3 (keep the stderr) or a throwaway test that shells out to `link.exe`
  under a vcvars environment.
- **MSVC versions other than 14.51.** `windows-latest` gives one compiler, the
  newest. Every claim above is about VS 18's `cl`. `__has_include` and
  `/std:c11` availability on older toolsets is [INFERRED] from Microsoft's
  documentation. A matrix over VS versions would need either
  `microsoft/setup-msbuild` with pinned toolsets or a self-hosted runner; I
  judged that out of scope for a spike and it is not blocking anything.
- **32-bit and ARM64 Windows.** `SIZEOF_VOID_P 8` was measured on x64 only.
  `SIZEOF_LONG 4` holds on all Windows [INFERRED]; the pointer size assertion
  in the shipped test is x64-specific and correct for `windows-latest`.
- **Whether `cl` writes probe diagnostics anywhere a human will see them.**
  Not investigated, and it is coupled to an existing open question in this repo
  (an earlier agent's wrong assumption that `cl` writes file-level diagnostics
  to stderr). Probes that answer correctly do not need it; step 3 does.
- **Anything requiring an interactive Windows session.** Nothing in this spike
  did, which was the pleasant surprise. `windows-latest` plus a
  `panic!`-with-a-transcript test was sufficient for all five questions. Stated
  plainly because the brief asked: **no interactive Windows machine is needed
  to execute the plan above either.** Step 1's table is testable by unit test
  plus the shipped MSVC integration test; step 3's classification is the only
  item where a real session would be faster than CI, and CI is adequate.

---

## Corrections to the brief

The brief was accurate on the important thing — the `libs = ["m"]` mapping is
wrong, and it is the one real defect. Four smaller corrections:

1. **"MSVC makes many CRT functions intrinsics, which may make a link-based
   probe answer differently than on GCC."** Measured: it does not.
   `HAVE_MEMCPY` and `HAVE_PRINTF` both answer `1`. I had independently
   predicted `HAVE_PRINTF` would answer `no` for a different reason (UCRT
   header-inline `printf`) and that prediction was also wrong. Taking the
   address defeats both intrinsic substitution and header-inlining.
2. **"Which linker, which default libraries."** Both already correct.
   `link.exe` is already the linker, and the default libraries arrive via the
   `/DEFAULTLIB` directives `cl` embeds in the object — nothing needs naming.
   The probe link line needs *fewer* libraries on Windows, not more.
3. **"Whether this is a small fix or a genuine redesign of the `symbol` kind
   on Windows."** Small fix. The kind's mechanism — fallback declaration,
   `volatile` reference, cast to `const void *`, `#if defined` macro branch —
   works unmodified on MSVC.
4. **"The single most valuable thing you can report [is whether] probe tests
   can run on Windows at all."** They can, but not the way the brief framed it
   (batch wrapper or `compile_commands.json`). The right witness is the
   generated config header, which is better than argv on every platform. The
   `CC`-shim framing is a dead end on Windows for a second reason the brief
   did not mention: setting `CC` switches Harbour to its GCC-shaped toolchain,
   so the argv recorded would not be an MSVC argv even if the shim ran.

And one correction to the *design document* rather than the brief:
`2026-09-11-native-probes-design.md:384-387` reasons that a `libs` entry
failing to resolve is a correct `false`. On Windows it is a correct `false` for
"the platform lacks the library" and an incorrect `false` for "the platform
spells it differently", and the two are indistinguishable after the fact. That
paragraph should be amended when step 1 lands.
