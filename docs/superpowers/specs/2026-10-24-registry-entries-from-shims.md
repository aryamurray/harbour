# Registry entries from shims

Status: findings and a recommendation. The four small fixes marked **fixed** are in
the PR that adds this note; everything else is a proposal.

## Why this was done

Harbour's roadmap was to prove real third-party packages build before building
registry generation, because generating many entries against an unvalidated format
risks regenerating all of them. That bar is met: zlib, libpng, curl, openssl,
LuaJIT, FreeType with HarfBuzz and CPython all build through Harbour as
hand-written shims.

What had **never** been verified is the registry path itself. Only nested *git*
dependencies were tested, and no real package had been resolved from a registry
and built. This note records what happened when four of them were: zlib 1.3.1,
libpng 1.6.50, FreeType 2.14.3 and HarfBuzz 15.0.0, as real entries in a local
git-backed registry, built from the real upstream sources.

## What an entry is (verified by running, trusting the code over the design doc)

A registry is a git repository:

```
config.toml
index/<l>/<name>.idx                 tier 1: one JSON record per line, per version
index/<l>/<name>/<version>.toml      tier 2: the shim
index/<l>/<name>/<file>.patch        patches the shim lists (beside it)
```

**The shim** has `[package]` (name, version), exactly one source, and optional
`[[patches]]`, `[metadata]`, `[features]`, `[surface_override]`, `[build]`:

- `[source.git]` — `url` and a **40-character `rev`** (tags and branches are
  refused). No checksum field is used.
- `[source.tarball]` — `url` and a **`sha256`**, optionally `strip_prefix`.
- `[[patches]]` — `file` and `sha256`. **Git sources only** (see G4).

**Where the package's manifest comes from.** The source is fetched, patches are
applied, and then either (a) a `Harbour.toml` found in the tree is loaded, or (b)
if there is none, a *synthetic* manifest is built from `surface_override`: one
static library named after the package, default sources `*.c` and `src/*.c`, and
**no dependencies, features, probes, generators, `when` blocks or host targets**.
No real C library ships a `Harbour.toml`, so (b) can describe almost nothing.
**The way to give an upstream a real manifest is a patch that adds one.** That is
what every entry below does, and it was verified end to end.

**The tier-1 index** is derived, not written: `harbour registry index <dir>` fetches
each shim's source, applies its patches, loads the resulting `Harbour.toml` and
records its `[dependencies]` as `{name, version_req, optional, default_features,
kind, registry}`. `path` and `git` dependencies are rejected; vcpkg ones are
skipped. The `checksum` is the tarball `sha256`, or `null` for a git source.
Resolution reads only this file; it never fetches a source to learn dependencies.

**What `harbour add` and a build do.** `add libpng` writes `libpng = "*"`. A build
resolves from the index, then fetches the *selected* versions, applies their
patches and builds. The lockfile pins each as `registry+<url>` with the shim's
`source_hash` as its precise source.

## Results

All run on macOS arm64, from cold caches, against a local registry, with the real
upstream clones (full history) and the real shims:

| scenario | result | cold build |
|---|---|---|
| `add libpng`; round-trip a PNG | passes; lock: `app → libpng → zlib 1.3.1`, both `registry+` | 50 s |
| `add freetype`; render a glyph | passes; lock has **one** `zlib`, one `libz.a` | 71 s |
| HarfBuzz with `features = ["freetype"]`, shaping through FreeType | passes; lock has `harfbuzz → freetype 2.14.3`; `harfbuzz` is absent from the FreeType-only build and was never fetched | 114 s |

Hermetic versions of each are in `tests/cli_integration/registry.rs` and run in CI.

## Gaps

Each with the precise failing case, what was done, and what is recommended. In
priority order.

### G1 — a plain dependency resolved against the *public* registry — **fixed**

*Failing case:* `libpng`'s `Harbour.toml` says `zlib = "^1.3"`. Indexing it in any
registry other than the built-in default recorded
`"registry": "https://github.com/aryamurray/harbour-registry"`, so a build tried to
clone the public registry, found no `zlib`, and failed with *"there is no version of
zlib in >=1.3.0, <2.0.0 … libpng 1.6.50 is forbidden"* before compiling anything.
For the public registry itself the default and the registry being indexed are the
same, which is why nobody had hit it.

*Fix:* the generator now treats a plain version requirement as "from the registry
this package is published in" and records `registry: None`, so the index is
portable: the same bytes resolve against the registry wherever a consumer cloned it
from. A dependency that names a registry explicitly is unchanged. The stale comment
that called the registry "irrelevant" was wrong (it is recorded three lines later).

### G2 — a failed patch poisoned the cache and surfaced as a different error — **fixed**

*Failing case:* a shim whose patch does not match its recorded `sha256`. The source
was cloned into the cache *before* the patch was verified; when verification failed
the half-prepared directory stayed, and the cache hit rule is "the directory exists
and has a `.git`". The next load served the **unpatched** clone and failed with
*"package 'zlib' has no Harbour.toml and no shim surface override"* — true, and about
the wrong thing. The first error never reached the user at all: `collect_packages`
(`ops/harbour_build.rs`) discards load failures with `if let Ok(..)`, so the patch
error was dropped on that path.

*Fix:* fetch and patch are one step; on failure the cache directory is removed and
the real error returned. *Not fixed:* `collect_packages` still swallows errors. It
should return them, or at least log them; with the cache no longer poisoned the
later, strict load reports the real cause, but one pass is silent for no reason.

### G3 — a package's patches were not part of its identity — **fixed**

*Failing case:* edit a patch (and its `sha256`) without bumping the version.
`source_hash()` is `sha256(url + rev)`, so the source-cache path and the lockfile's
pinned source were unchanged: every existing cache kept serving the tree patched the
old way and no lockfile moved.

*Fix:* the patch files and hashes are folded into `source_hash()`. A shim with no
patches hashes exactly as before, pinned by a unit test, so nothing already cached or
locked moves.

### G4 — patches cannot be applied to tarball sources — not fixed

*Failing case:* `apply_patches` bails with *"patches can only be applied to git
sources, not tarballs"*. A tarball shim therefore cannot carry a `Harbour.toml`, and
is limited to the synthetic one-library manifest (above).

*Consequence:* **every committed canary fetches a tarball** (`zlib`, `cjson`,
`libuv`, `zstd`, `curl`, `openssl`, `cpython`), so none of them can become a registry
entry as it stands. They are all convertible by pinning the upstream git tag's
commit instead, which is what was done for the four here.

*Recommendation:* allow patches on tarballs, applied after extraction, with the same
clean-up on failure. A tarball's `sha256` is a *stronger* pin than a git rev for
upstreams whose release tarball differs from the tag (generated `configure`, bundled
submodules), and it avoids cloning history (G5). Not done here because a tarball
cannot be fetched hermetically (`reqwest` does not read `file://`), so the change
would ship untested.

### G5 — every git source is a full clone — not fixed

`fetch_git_source` clones the whole repository and resets to the rev. The cold builds
above (71 s and 114 s for FreeType and HarfBuzz) include those clones; the split between
clone and compile was not measured. CPython and OpenSSL would be far larger. *Recommendation:* fetch the pinned
rev alone (`git fetch --depth 1 origin <rev>`); GitHub and GitLab both serve a commit
by SHA. A git source also has `checksum: null` in the index; the rev is itself a
content hash, so this is acceptable, but it means the lockfile records no digest of
the *patched* tree. G3's fix covers the patch; nothing covers the combination.

### G6 — `harbour add` cannot add a registry dependency with features — not fixed

`harbour add harfbuzz --features freetype` wrote
`harfbuzz = { vcpkg = true, vcpkg_features = ["freetype"] }`: `--features` silently
selects vcpkg. It also writes `"*"` rather than the version it resolved, unlike
Cargo. Both are in `ops/harbour_add.rs`. The manifest was edited by hand for the
HarfBuzz scenario.

### G7 — the index does not carry a dependency's `features`, and does not need to — finding

The suspicion was that `IndexDependency` (no `features` field) would drop
`features = ["extra"]`, so `mid`'s request for `base`'s `extra` would be lost. A field
was added and the test written; removing the field changed nothing, so it was **not
kept**. The build re-reads each fetched package's own manifest to unify features and
activate optional dependencies, so the index is not consulted.

That has a consequence worth stating against the design doc's claim that
"resolution is metadata-only": when any dependency in the graph is optional, the
activation pass (`ops/resolve.rs`, `activated_optional_dependencies`) loads the
manifest of **every package already selected**, which means their sources are fetched
during resolution. Not every *candidate* version, as before, but not zero either. If
resolution must be possible from the index alone (the sparse-HTTP goal), the index has
to carry what the activation pass reads: `[features]` and which optional dependency
each enables. That is the real format change to make, and it should be driven by a
test that fails today, which this finding does not yet have.

### G8 — nothing records what a package needs on the build machine — fixed (`[package] build_tools`)

`openssl`'s shim runs `perl`; `cpython`'s runs `sh`. A registry entry carries no
statement of that, so a missing tool is discovered as a generator failing partway
through a build. *Recommendation:* `[metadata] build_tools = ["perl"]`, checked before
the build starts. Observation only; not reproduced here.

*Fix:* `[package] build_tools = ["perl"]` (names only, validated at load), checked on
`PATH` before any generator of the build runs, for every planned package including
dependencies, and only for a package with a generator or recipe that will actually run
in this build (a feature that is off does not demand its tool). The home is
`[package]`, not a registry-only field, because it is a fact about the package: the same
list serves a path dependency, a git dependency and a registry entry, and travels in the
manifest a shim carries as a patch. See MANIFEST.md, "Build Tools".

### G9 — patches are hashed by bytes, and git may change the bytes — fixed for the fixture, convention needed

*Failing case (seen in Windows CI, not reproducible on macOS):* a shim's patch is
verified by the sha256 of the file as checked out. Harbour clones the registry with
git, and Git for Windows converts line endings on checkout by default
(`core.autocrlf=true`), so the patch's bytes differ from the hash recorded when it was
written and verification fails: *"patch file hash mismatch … expected 246d3c66…"*.
That would make **every patch in every registry fail on a default Windows setup**.

*Fix here:* the fixture and `tools/registry-from-shims` write a `.gitattributes` with
`*.patch -text`, which stops git converting them. That is a registry convention and it
must be part of the format: `harbour registry index` should refuse (or write it for)
a registry without it. Related and not yet addressed: the *upstream* clone is also
subject to conversion, so a patch that **modifies** an existing upstream text file has
context lines that no longer match its CRLF checkout. Patches that only add files, as
every entry here does, are immune; a generator should prefer them, or Harbour should
clone upstream sources with conversion off.

### G10 — two Harbour processes on one cold cache collide — not fixed

*Failing case (reproduced twice, by hand, macOS):* two projects depending on the
same registry package, built **at the same time** against one fresh cache. One build
succeeds; the other fails, at one of two places depending on how warm the cache is:

- cold registry index: `error: failed to clone registry index from file://…` caused by
  `failed to create locked file '…/Library/Caches/com.harbour.harbour/cache/registry/…'`;
- warm index, cold source cache: `error: the index is locked; this might be due to a
  concurrent or crashed process; class=Index (10); code=Locked (-14)`, from the source
  clone/checkout of the package.

OpenSSL made it easy to hit because the tree is 140 MB and the clone and checkout take
seconds, but it is not OpenSSL's: any two processes populating the same cache entry
race. Realistic triggers are two terminals, an editor's background build next to a CLI
build, and parallel CI jobs sharing a cache directory. Nothing serialises cache
population: `GitTransport::clone_index` and the source clones and extractions in
`src/sources/registry/` clone straight into their final directory.

*Proposed fix:* populate into a sibling temporary directory
(`<dir>.tmp-<pid>-<nonce>`), then `rename` it into place; if the rename fails because
the destination now exists, another process won, so discard ours and use theirs. That is
atomic on one filesystem, needs no new dependency and no lock to leak after a crash. It
applies to three sites: the registry index clone, the upstream source clone (git), and
the tarball extraction. A hermetic regression test cannot rely on timing with a tiny
fixture (the race window is milliseconds), so it needs an injected pause between "begin
populating" and "finish" (a test hook, or a fixture source that is large or slow to
serve) to be meaningful, and should be mutation-checked like the others.

### G11 — generators write into the shared, supposedly immutable source cache — not fixed

*Observed (by hand):* a registry package's source tree is cached once at
`<cache>/registry-src/<registry>/<name>/<version>/<hash>/`, and `prebuild` generators run
with the package root set to that directory, so OpenSSL's `configdata.pm`,
`include/openssl/crypto.h` and the generated `crypto/sha/sha256-armv8.S` were written
**into the shared cache tree** (confirmed: they exist there after the build, and the
tree is 140 MB). The `<hash>` is the identity of the source and its patches; it does not
include the target, features or platform.

*Why it matters:* two builds of one package for different targets share that tree. For
OpenSSL the per-architecture assembly has per-architecture file names, so an aarch64 and
an x86_64 build coexist (verified: a cross build to `x86_64-apple-darwin` after the
native one worked and produced the right objects), and `configdata.pm` is identical
for any two 64-bit targets. But `configdata.pm` and `bn_conf.h` encode `sizeof(long)`,
and perlasm output depends on the flavour (`ios64` vs `linux64` for the same
architecture), so a 32-bit and a 64-bit build, or the same architecture for two
operating systems, will overwrite each other's generated files. Combined with G10 it is
also a write race. A cache a user can delete and rebuild hides this; one shared by CI
jobs does not.

*Proposed fix:* run generators against a per-build copy or overlay (the artifact
directory already exists for exactly this: `HARBOUR_ARTIFACT_DIR`), keeping the cache
tree read-only. The cheapest honest version is to copy the cached tree into the target's
build directory when the package has `prebuild` steps (hardlinks or reflinks where the
filesystem has them) and point `HARBOUR_PACKAGE_ROOT` and the package root there. It
costs disk per target for generator-bearing packages only, and it makes the cache
content-addressed in fact, as its hash already claims.

### What already works, and was verified

| need | how it is carried |
|---|---|
| a manifest for an upstream with none | a patch adding `Harbour.toml` (G4 limits this to git) |
| a file upstream's `.gitignore` hides (libpng's `pnglibconf.h`) | the same patch; `git apply` ignores `.gitignore` |
| per-platform differences | `[[targets.X.when]]` blocks inside the one manifest |
| registry dependencies, nested and shared | plain requirements; one copy in lock and link (tested) |
| optional dependencies and features | `optional = true` plus `features`; the inactive one is never fetched (tested) |
| host-run generators and tools | `host = true` and `tools = [...]` in the same manifest |

## What a generator (human or LLM) must emit per package

A package is **correct** if it builds and behaves; it is **checkable** if a machine can
say so. Per package, under `index/<l>/<name>/`:

1. **`<version>.toml`, the shim.** A git `url` and the **full commit SHA** (resolve a
   tag to its commit: a tag object's SHA is not the commit's, and the format refuses a
   short rev). One `[[patches]]` entry whose `sha256` is the digest of the patch file's
   bytes.
2. **`<version>.patch`, produced by `git diff` against the pinned tree**, never written
   by hand, so that `git apply --check` is the validity test. It adds `Harbour.toml` and
   every overlay file (generated headers, per-platform files). Use `git add -f -N` so
   files upstream ignores are included.
3. **The manifest**, with:
   - source files **named individually** (a glob can sweep in `example.c`, `test/`;
     a named list makes a bad extraction a hard error instead of a smaller library);
   - dependencies as **plain registry requirements**, no `path`, `git` or `registry =`;
   - platform facts in `when` blocks, and for large configure-style headers see below;
   - a generator step (`prebuild`) only with `inputs` and `outputs` declared, and `tools`
     if it runs a program the package builds;
   - features and `optional` dependencies where upstream has them.
4. **A proof**, in the style of `ci/canary`: a consumer that *runs* the library and
   asserts behaviour, not exit status (a PNG round-trips; a glyph renders; a loop is
   JIT-compiled), plus a translation-unit count so a silently shrinking source list is
   visible, plus, for anything with platform-specific code, a check of which object
   defines which symbol.
5. **The checks a pipeline runs**, in order: `git apply --check` clean →
   `harbour registry index` succeeds (this fetches, patches, and rejects `path`/`git`
   dependencies) → the proof builds and passes on every platform the entry claims →
   the lockfile of an app that reaches the package twice has **one** entry for it.

`tools/registry-from-shims/make_registry.py` does steps 1, 2 and the index for the four
packages here, from the manifests beside it, and is the worked example.

### Probes and literals, for the generated parts

For a large configure-style header, use the CPython result (PR #178,
`ci/canary/cpython-config`) as the guide: of the 654 names in CPython's `pyconfig.h`,
about 470 (72%) are *measured* by the five probe kinds (headers, functions, sizes,
types, members, declarations) and are **machine-derivable**: a generator can emit them
as bulk lists and have them checked against configure's own answers. The remaining
~170 are *asserted* and need a human or a measurement the probes cannot make: run-time
behaviour checks (`AC_RUN_IFELSE`), alignments, library-dependent feature tests
(curses, readline, sqlite), choices (`WITH_*`) and package metadata. Assertions belong
in the manifest visibly separate from measurements, so a reviewer can see which is
which. The counts are as reported by #178, not re-measured here.

## OpenSSL: a worked example with a declared tool

OpenSSL 3.5.4 is the first entry that needs both a tarball source (G4, fixed in #192)
and a declared build tool (G8, `[package] build_tools = ["perl"]`). The pieces:

- **Source:** the release tarball the CI canary pins (url, sha256, `strip_prefix`),
  fetched, checked and extracted by the transport.
- **Manifest as a patch:** the canary's own `ci/canary/openssl/Harbour.toml`, produced
  into the entry by `git diff` against the extracted tree (22 KB), never written by hand.
  The manifest declares `build_tools = ["perl"]`; the CI canary uses the same file, so
  the declaration is exercised on every CI run, not only by a registry consumer.
- **Run by hand, macOS arm64, local registry:** `harbour add openssl`, then a build of an
  app that never mentions perl: 14.7 s cold, including Harbour's own download and
  extraction of the 55 MB tarball and the seven perl generators. The canary's consumer
  passed all 19 known-answer checks; the 15 expected objects exist; the C fallback is
  compiled out of `sha256.o` (`sha256_block_data_order` is *undefined* there and defined
  in `sha256-armv8.o`); the ARMv8 SHA-2 path was taken. A cross build to
  `x86_64-apple-darwin` (4.4 s with the tree cached) selected the x86_64 assembly,
  correctly excluded `aes_core.o`, and its consumer passed 17 checks under Rosetta.
- **What the check buys:** with `perl` removed from `PATH`, the dependency is named and
  the build stops before any generator ran (hermetic test
  `a_registry_dependencys_missing_tool_is_reported_before_its_generator_runs`, with a
  control proving the same entry runs its generator when the tool is present).

What this exposed is G10 and G11 above; neither is specific to OpenSSL.

## Which committed shims could become entries

By inspection, not conversion; only the four above were converted.

| shim | blocker | note |
|---|---|---|
| zlib, libpng, FreeType, HarfBuzz | none | done |
| cjson, libuv, zstd | tarball (G4) | upstream git tags exist; pin the commit |
| curl | tarball (G4); needs `sh`, probes in the manifest | probes fit in the patch |
| openssl | none | **done** (tarball + patch + `build_tools`); see the worked example, G10 and G11 |
| LuaJIT | none for source (already a commit pin) | `host = true` tools fit |
| CPython | tarball (G4); `sh`; per-platform `pyconfig.h` overlays are ~2 000 lines each | prefer the probe-generated header (#178) over carrying three copies in a patch |

## Recommended changes, in priority order

1. **Already done:** G1, G2, G3.
2. **Make the `.gitattributes` convention part of the format (G9)** and have
   `harbour registry index` enforce it; clone upstream sources with line-ending
   conversion off.
3. **Allow patches on tarball sources (G4).** It unblocks the seven tarball-based canary
   shims and is the single largest gap. Needs a way to test a tarball fetch offline.
4. **Shallow fetch by rev (G5).**
5. **Make `collect_packages` return errors (G2's remainder)** and fix `harbour add`
   with features (G6).
6. **Decide the resolution contract (G7):** either accept that resolution fetches the
   selected packages when any dependency is optional and update the design doc, or move
   `[features]` and optional-dependency activation into tier 1, driven by a failing test.
7. **`build_tools` metadata (G8).**
