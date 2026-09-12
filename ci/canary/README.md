# Canaries: real third-party packages, built and run

Each subdirectory builds a real upstream library with Harbour and then **runs
something against it**. That last part is the point. Every bug these exist to
catch produced a *successful build*:

- `ar r` matches archive members by name, so a renamed source left its stale
  object behind and won symbol resolution. The library reported its
  pre-change behaviour with no error anywhere.
- A `[[targets.X.when]]` block that stops matching drops a platform's event
  loop or an assembly fast path. zstd falls back to a C implementation, so
  the archive still links and the only witness is the decompressed bytes.
- A source list that silently shrinks still archives successfully.

So a canary asserts on the program's output and on the translation-unit
count, never on exit status alone — and where a package has an assembly fast
path, on **which object defines the symbol**, because the count is blind to
the two cases that matter most:

- a `.S` compiled with its body `#if`'d out is an *empty object*: the count
  is unchanged, the name is present, the output is correct;
- a `|`-separated count set (`"38|37"`) legitimises both values on every
  platform, so it cannot notice one platform getting the other's answer.

`canary_require_object`, `canary_defines_symbol` and friends in `lib.sh` are
what catch those. All four failures were reproduced by breaking real
manifests, not reasoned about; the table is in
`docs/superpowers/specs/2026-09-12-openssl-generated-sources.md`.

## Running them

```sh
cargo build
ci/canary/run-all.sh              # all six
ci/canary/zstd/run.sh             # one
ci/canary/zstd/run.sh /tmp/mydir  # one, in a chosen work dir
```

`run-all.sh` runs every canary even after one fails, because *which* ones are
red is the diagnostic. cjson and zlib red together means something broad;
zstd alone points at the architecture-conditional assembly block; libuv alone
points at per-OS source selection; `curl-config` alone points at the probe
subsystem, and its output names the individual question that disagreed.

## What each one covers

| canary | sources | what only it exercises |
|---|---|---|
| `cjson` | 2 | the control. No conditionals, no vendored config. Red here means something broad. Its float assertions are what make `system_libs = ["m"]` on a *public* surface load-bearing. |
| `zlib` | 15 | the original canary: a public header consumers must find, platform-conditional defines. |
| `curl-config` | 1 | **89 of curl 8.22.0's own configure questions**, answered by Harbour probes and compared against what curl's cmake concluded on the same platform. 11 of the 89 answers differ between macOS and Linux, in both directions — those are the rows that would catch the probe subsystem returning constants. Downloads nothing: curl's *questions* are what is under test. See `curl-config/regenerate.md`. |
| `libuv` | 31 + per-OS | `[[targets.X.when]]` keyed on `os` with **no portable fallback** — a stale block fails to link on `uv__platform_loop_init` rather than building something subtly wrong. The consumer drives a real TCP echo round trip through the selected event loop. |
| `zstd` | 37 + 1 `.S` on x86_64 | mixed C and assembly across five source directories; `ZSTD_MULTITHREAD` making `pthread` load-bearing on the public link surface; the dictionary builder, which is the directory a source list is most likely to drop. Also the **symbol-level** check on that `.S`: `ZSTD_DISABLE_ASM` compiles it to an empty object, which keeps the count at 38 and the round trip byte-exact. |
| `openssl` | 9 + 5 generated per arch | **sources that do not exist in the tarball.** The only canary that runs `[[targets.X.prebuild]]` generators, and the only one whose *headers* are generated: 31 `.h.in` templates plus per-architecture perlasm, all produced on the machine doing the build, nothing vendored. It also covers per-(os, arch) generator selection — the perlasm flavour (`ios64`/`linux64`/`macosx`/`elf`) is what a cross-build gets wrong — and `exclude`, since on x86_64 `aes-x86_64.s` replaces `aes_core.c` rather than adding to it. |

## How they are laid out

```
ci/canary/
  lib.sh            the shared machinery: fetch, verify, build, assert
  run-all.sh
  <pkg>/run.sh      ~10 lines: a pinned URL, a sha256, expected TU count
  <pkg>/Harbour.toml          the committed shim
  <pkg>/consumer/             a program that uses the library and prints OK
```

`lib.sh` exists rather than a copy of the same 100 lines per package. Four
copies of "assert the second build reused everything" would drift, and the
copy that drifted would be the one that stopped checking — which is the
finding the 2026-09-07 schema audit reached ten times over about this
codebase.

## Two deliberate choices

Two of the six do not follow the fetch-and-build shape. `openssl` has its own
`run.sh` body rather than `canary_standard_run`, because it has work to do
before the build (asserting the tarball does *not* already contain the headers
its manifest claims to generate) and a longer list of objects to check
afterwards. A canary that only needs the latter uses the
`canary_extra_assertions` hook instead, as zstd does.

And `curl-config` has
no upstream tarball, because what it tests is curl's list of *questions*
rather than its sources. It holds curl's answers as a golden file
(`expected.json`, produced by curl's own cmake — see `regenerate.md`) and
compares Harbour's generated `curl_config.h` against them question by
question.

**Manifests are committed; upstream sources are not.** A change to a shim
then shows up in review as a diff, while a vendored tarball would be a fork
nobody remembers taking. Each `run.sh` fetches a pinned tarball into a work
directory and copies the committed `Harbour.toml` into it.

These manifests were previously rebuilt from scratch three separate times
because they only ever lived in a temp directory, and were recoverable at all
only because `docs/superpowers/specs/2026-09-07-extensibility-audit.md`
happens to embed them — and zstd's source list is elided even there. They are
version-controlled now for that reason.

**Tarballs are pinned by sha256, and by a release asset where one exists.** A
canary that follows whatever upstream publishes turns a supply-chain change
into a Harbour bug report, and an interrupted download into a mystery compile
error. zlib, zstd and curl publish immutable release assets; libuv publishes
`dist.libuv.org` tarballs. cJSON has only GitHub's auto-generated archive,
which GitHub has changed the generation of once before — pinning it means
that shows up as a checksum mismatch naming the file.

## Adding one

Four files. The expected translation-unit count may be a `|`-separated set
when it legitimately differs per platform (`"38|37"` for zstd) — but know what
that costs: a set legitimises every value on every platform, so it cannot
catch one platform building another's source list. If the difference is an
assembly fast path, define `canary_extra_assertions` and name the object and
its symbol as well.

```sh
mkdir -p ci/canary/foo/consumer/src
# ci/canary/foo/Harbour.toml         the shim
# ci/canary/foo/consumer/Harbour.toml  path dep on "../upstream"
# ci/canary/foo/consumer/src/main.c    prints "OK ..." only after asserting
# ci/canary/foo/run.sh                 URL, SHA256, topdir, name, exe, count
```

Then add it to `CANARIES` in `run-all.sh`. Cheapest and broadest first, so a
systemic breakage is reported in seconds rather than after two minutes of
downloads.
