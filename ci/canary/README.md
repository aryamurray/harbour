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
count, never on exit status alone.

## Running them

```sh
cargo build
ci/canary/run-all.sh              # all four
ci/canary/zstd/run.sh             # one
ci/canary/zstd/run.sh /tmp/mydir  # one, in a chosen work dir
```

`run-all.sh` runs every canary even after one fails, because *which* ones are
red is the diagnostic. cjson and zlib red together means something broad;
zstd alone points at the architecture-conditional assembly block; libuv alone
points at per-OS source selection.

## What each one covers

| canary | sources | what only it exercises |
|---|---|---|
| `cjson` | 2 | the control. No conditionals, no vendored config. Red here means something broad. Its float assertions are what make `system_libs = ["m"]` on a *public* surface load-bearing. |
| `zlib` | 15 | the original canary: a public header consumers must find, platform-conditional defines. |
| `libuv` | 31 + per-OS | `[[targets.X.when]]` keyed on `os` with **no portable fallback** — a stale block fails to link on `uv__platform_loop_init` rather than building something subtly wrong. The consumer drives a real TCP echo round trip through the selected event loop. |
| `zstd` | 37 + 1 `.S` on x86_64 | mixed C and assembly across five source directories; `ZSTD_MULTITHREAD` making `pthread` load-bearing on the public link surface; the dictionary builder, which is the directory a source list is most likely to drop. |

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
when it legitimately differs per platform (`"38|37"` for zstd).

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
