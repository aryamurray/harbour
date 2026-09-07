# Review: crate and module layout as Harbour grows

**Date:** 2026-09-07
**Status:** Recommendation — verdict is *do not split into a workspace now*
**Scope:** whether `harbour-cli`'s single-crate layout is the right direction,
and what to reshuffle if not.

---

## Verdict up front

**Keep the single crate. Do not do a workspace split — not now, and on the
current evidence not later either.** The cost the split is supposed to buy down
is compile time, and compile time is not a problem: the whole 54k-line library
type-checks from scratch in **2.9s**, and the actual edit-compile loop is
**2.1s** regardless of what you edit.

But the layout *does* have one real defect, and it is not the one that was
suspected. It is not file size and it is not crate granularity. It is that

> **all six top-level modules form a single strongly connected component,**
> **held together by only 10 production references.**

There is no layering today — `core`, `util`, `resolver`, `sources`, `builder`
and `ops` are mutually reachable. Ten `use` lines are the entire reason. Fixing
those ten is a small, safe, high-value change that gets the *architectural*
benefit people reach for a workspace to obtain (layering that cannot silently
rot) without any of the churn.

Recommendation, in order:

1. **Now (small):** break the 10 cycle-forming references so the module graph is
   a DAG, and add a `cargo deny`-style guard so it stays one. One is already
   done in this PR.
2. **Now (small):** delete the confirmed-dead code this review found, and fix
   the one genuinely unwired facility (`LinkGroup`).
3. **Later, only if a second binary or an external consumer appears:** revisit
   the workspace question. Splitting has one benefit that visibility discipline
   cannot replicate — it makes cycles *impossible* — but nothing today needs it.
4. **Not recommended:** splitting `surface_resolver.rs` and friends purely for
   line count; the `test-support` self-dependency removal as a standalone task.

---

## 1. Measurements

### 1.1 Machine and a measurement caveat that matters

Apple M4 Pro, 14 cores, `rustc 1.97.1`, macOS 25.6.

**`~/.cargo/config.toml` sets `rustc-wrapper = "kache"`** — a compilation cache.
Every timing below was taken with a *unique* probe comment appended to the edited
file so the wrapper cannot serve a hit for the crate under test. Dependency
compile time, however, *is* cache-assisted: a nominally cold
`cargo clean && cargo build` finished in **14.3s**, which is not a real
cold-build number for anyone without a warm `kache`. Treat dependency build time
as already solved on this machine and ignore it in the analysis — a workspace
split cannot improve it either way, since the dependency set is unchanged.

### 1.2 Size

| module | total lines | inline `#[cfg(test)]` | production | test % |
|---|---|---|---|---|
| `builder` | 18,799 | 4,524 | 14,275 | 24% |
| `core` | 9,266 | 2,890 | 6,376 | 31% |
| `ops` | 6,899 | 1,976 | 4,923 | 29% |
| `bin` | 6,184 | 2,233 | 3,951 | 36% |
| `sources` | 5,519 | 1,713 | 3,806 | 31% |
| `util` | 3,710 | 865 | 2,845 | 23% |
| `resolver` | 2,288 | 807 | 1,481 | 35% |
| `test_support` | 1,730 | 194 | 1,536 | 11% |
| **total** | **54,429** | **15,202 (27.9%)** | **39,227** | |

**28% of the crate is inline unit tests.** The production library is ~39k lines,
not 54k. This matters twice over: it shrinks the thing a split would shard, and
it means `cargo build` (which compiles none of it) and `cargo test` (which
compiles all of it) are very different measurements.

### 1.3 Compile time — the number that decides this

All figures are steady state (the first reading after switching feature sets is
discarded as it includes cross-feature rebuild churn), median of repeats, with a
unique edit each time.

| what you edit | `cargo build` (lib+bin) | `cargo check --all-targets --all-features` | `cargo test --all-features` (compile + run) |
|---|---|---|---|
| **leaf** (`ops/harbour_new.rs`, 4 crate-wide mentions) | **2.13s** | **1.82s** | **12.2s** |
| **hub** (`core/manifest.rs`, imported by 8 modules) | **2.18s** | **1.82s** | **13.2s** |
| crate root (`lib.rs`) | 2.14s | — | — |

And the ceiling — a full non-incremental compile of Harbour's own code, which is
precisely the work a workspace split would shard:

| | time |
|---|---|
| `cargo build` lib+bin, `CARGO_INCREMENTAL=0` | **9.5s** (9.37 / 9.55 / 9.68) |
| `cargo check --lib`, `CARGO_INCREMENTAL=0` | **2.9s** (2.95 / 2.91) |

Three conclusions, and they are the crux of the review:

1. **The hub/leaf asymmetry is real and exactly as predicted: there is none.**
   2.13s vs 2.18s, 1.82s vs 1.82s. In a single crate the compilation unit is the
   whole crate, so editing a leaf costs the same as editing `core/manifest.rs`.
   That is the theoretical case for splitting, confirmed.
2. **The theoretical case does not survive contact with the absolute numbers.**
   Sharding a 2.1s rebuild is not worth a repo-wide refactor. Even the
   worst case — a from-scratch type-check of all 39k production lines — is 2.9s.
   A split might take the common rebuild from 2.1s to perhaps 1.2s. Nobody's
   loop improves perceptibly.
3. **The dev loop is dominated by test *execution*, which a split does not
   fix.** `cargo test` is 12–13s, of which compilation is ~2s and the 588 unit
   tests plus 50 CLI integration tests are the rest (the integration suite alone
   runs 5.87s, since it shells out to the real binary). The one genuine win from
   a workspace here is `cargo test -p harbour-core` to run a subset — but
   `cargo test core::` already does that today for the unit tests.

**On compile-time grounds the single-crate layout does not hurt today, and a
workspace split is not justified.** This is the measured answer to the central
question, and it is a negative one.

---

## 2. Defects found

### 2.1 The real defect: the module graph is one cycle

Production-only module graph (`#[cfg(test)]` blocks excluded, all `crate::X`
paths counted — not just `use` lines):

```
builder    -> core, ops, resolver, sources, util
core       -> builder, sources, util
ops        -> builder, core, resolver, sources, util
resolver   -> core, sources, util
sources    -> core, util
util       -> builder, core
```

Tarjan gives **one** strongly connected component:
`['builder', 'core', 'ops', 'resolver', 'sources', 'util']`.

So the layering that the module names imply does not exist. `core` is not a
foundation — it reaches up into `builder` and `sources`. `util` is not a leaf —
it reaches into both `core` and `builder`. This is the finding that should drive
any reshuffling, and it was invisible to a `use crate::X` grep because two of
the edges are inline paths rather than imports.

The encouraging part: for the target layering
`util < core < sources < resolver < builder < ops`, only **10 production
references** violate it, across 6 files.

| edge | refs | locations | fix |
|---|---|---|---|
| `core -> sources` | 3 | `src/core/dependency.rs:263`, `:317`, `:396` — all `crate::sources::registry::validate_package_name(name)?` | move `validate_package_name` into `core` (it is a name-syntax rule, not a registry concern) |
| `core -> builder` | 2 | `src/core/manifest.rs:358` `pub backend: Option<crate::builder::shim::BackendId>`, `:371` the parse | move `BackendId` into `core` |
| `builder -> ops` | 2 | `src/builder/native.rs:22`, `src/builder/cmake.rs:8` — `use crate::ops::harbour_build::Artifact` | move `Artifact` into `builder` — **done in this PR** |
| `util -> core` | 2 | `src/util/vcpkg.rs:12` (`TargetTriple`), `src/util/context.rs:23` (`ManifestError`) | these two files are not really `util`; see 2.2 |
| `util -> builder` | 1 | `src/util/config.rs:19` `use crate::builder::shim::{BackendId, LinkagePreference}` | same `BackendId` move |

Two observations on these:

- **`core -> builder` is worse than the `util -> builder` edge that prompted
  this review.** `manifest.rs:358` embeds `BackendId` as a *field of a manifest
  struct*, so the crate's foundational data model depends on the build system.
  `util/config.rs:19` by contrast is one import feeding two four-line parse
  helpers (`config.rs:502-510`).
- **Both are fixed by the same move**, and it is nearly free. `BackendId`
  (`src/builder/shim/capabilities.rs:12`) is a 4-variant fieldless
  `Copy + Hash + Serialize` enum with `as_str`/`Display`/`FromStr` and **zero
  dependencies on any other builder type**. `LinkagePreference`
  (`src/builder/shim/intent.rs:13`) is nearly as simple, needing only
  `ArtifactKind` (`intent.rs:114`, another plain `Copy` enum). Because
  `src/builder/shim/mod.rs:75,86` *already* re-exports both, moving the
  definitions to `src/core/backend.rs` and leaving the re-exports in place means
  **all 17 referencing files keep compiling unchanged.** The real diff is two
  type definitions relocated plus two `use` lines.

### 2.2 `util` is not a utility module

`src/util/config.rs` (875 lines) is the global/project configuration loader:
`Config`, `BuildConfig`, `FfiConfig`, `NetConfig`, `VcpkgConfig`,
`ToolchainConfig`, the two-tier project-over-global merge (`load_config` at
`:519`, `load_toolchain_config` at `:180`) and `generate_vcpkg_configuration()`
at `:367`. That is a layer, not a utility.

It cannot simply be promoted above `builder`, though: `src/builder/context.rs:18`
and `src/builder/toolchain/detect.rs:8` legitimately import `VcpkgConfig` and
`ToolchainConfig` *downward* from it. Promoting the module wholesale converts
those two into new back-edges. The honest fix is to split it — toolchain/vcpkg
settings stay low, `Config` and the loaders move up — which is a ~20-file change
and **not worth doing now**. Recorded here so it is not rediscovered.

Same shape, smaller: `src/util/vcpkg.rs` and `src/util/context.rs` are the only
two files creating the `util -> core` edge. They are domain code parked in
`util`.

### 2.3 The five large files: three have one job, two do not

Size alone was not the defect. Test modules account for 27–35% of each:

| file | total | test mod starts | test lines | verdict |
|---|---|---|---|---|
| `src/builder/surface_resolver.rs` | 2,165 | 1,456 | 710 (33%) | **several jobs** |
| `src/core/manifest.rs` | 1,808 | 1,231 | 578 (32%) | **several jobs** |
| `src/builder/toolchain/detect.rs` | 1,798 | 1,172 | 627 (35%) | mostly one job |
| `src/builder/plan.rs` | 1,495 | 1,095 | 401 (27%) | one job, one 552-line function |
| `src/builder/native.rs` | 1,385 | 989 | 397 (29%) | mostly one job |

**`surface_resolver.rs`** — the one to look at hardest — is 1,455 production
lines doing seven things, of which two are merely co-located:

- `compute_feature_sets` (lines 300–425, plus **511 lines of tests** at
  1,654–2,164) is whole-graph feature unification. It takes
  `(&Resolve, &HashMap<PackageId, Package>)` as free arguments, touches no
  surface type, and has exactly one caller (`surface_resolver.rs:467`). It is
  `core` logic living in `builder` — and tellingly, `src/core/features.rs:27,72,138`
  and `src/core/target/core.rs:536` all carry doc comments pointing *up* at it.
  708 lines that belong in `core`.
- Lines 1,219–1,455 are a **C preprocessor mini-lexer and header lint**
  (`logical_lines`, `directive_of`, `is_directive_or_inert`,
  `defines_in_preprocessor_conditionals`, `warn_private_defines_in_public_headers`).
  General-purpose text scanning with no resolver knowledge, reached from one
  call site (`:562`) as a side-effecting `tracing::warn!`.

What is left (426–1,163) is a genuinely cohesive `SurfaceResolver` — except that
495–921 and 923–1,163 are two near-duplicate renderings of the *same* graph
walk, one plain and one provenance-tracking. Unifying them would remove ~240
lines of code that can drift; the comment at `:546-552` warns about exactly that
drift having already hidden a `frameworks` bug.

**`core/manifest.rs`** has three clearly co-located concerns: TOML error
pretty-printing (`format_toml_error` at 654, `byte_offset_to_line_col` at 722 —
87 generic lines belonging in `util`), manifest *text generation*
(1,135–1,230 — the inverse operation from parsing, consumed only by
`ops/harbour_new.rs`), and `triple_matches_pattern` (:278, target logic whose
home is the existing `core/target/triple.rs`).

These are worth doing, cheaply and opportunistically, when someone is next in
these files. They are not worth a dedicated sweep — see §5.

### 2.4 Confirmed dead and unwired code

Verified by targeted grep, not inferred:

| item | location | status |
|---|---|---|
| `generate_default_manifest` | `src/core/manifest.rs:1135` | **dead — zero references repo-wide.** Its siblings `generate_lib_manifest`/`generate_exe_manifest` are called from `ops/harbour_new.rs:56,58`; the `is_lib: bool` dispatcher that would wrap them was never wired. |
| `EffectiveCompileSurfaceWithProvenance::to_flags` | `src/builder/surface_resolver.rs:134` | **dead.** `bin/harbour/commands/flags.rs:107` iterates items and calls the per-item `LibRef::to_flags` instead. |
| `EffectiveLinkSurfaceWithProvenance::to_flags` | `src/builder/surface_resolver.rs:165` | **dead.** Same, via `linkplan.rs:90`. |
| `EffectiveCompileSurface::to_flags` | `src/builder/surface_resolver.rs:1166` | **test-only** — sole caller is `:1623`, its own test. |
| `EffectiveLinkSurface::to_flags` | `src/builder/surface_resolver.rs:1185` | **test-only** — sole caller `:1642`. Production hand-rolls the same flags at `src/builder/plan.rs:845`. |
| `LinkGroup` / `EffectiveLinkSurface.groups` | `src/core/surface.rs:294`, `surface_resolver.rs:223` | **unwired facility — see below.** |
| `SurfaceResolveError` | `src/builder/surface_resolver.rs:22` | over-exposed: `pub` with structured variants, absent from `builder/mod.rs`'s re-export list, never matched anywhere. Callers only ever see the `anyhow` string. |
| `compute_feature_sets`, `SurfaceKind`, `WithProvenance::new` | `surface_resolver.rs:300`, `:81`, `:113` | `pub` with no external consumer |

**The `LinkGroup` finding is a new instance of this project's dominant failure
mode, found during this review.** `LinkGroup` is parsed from the manifest
(`core/manifest.rs:809-820`), merged (`core/surface.rs:611`), and propagated
through the resolver into `EffectiveLinkSurface.groups`
(`surface_resolver.rs:916`) — and then **nothing reads it.** No `--start-group`
/ `--end-group` is ever emitted: `plan.rs` never reads `link_surface.groups`,
`EffectiveLinkSurface::to_flags` (:1185) never reads `self.groups`, and
`native.rs` never sees it. The comment at `surface_resolver.rs:737` says the
surface "already models" the feature, which is true and is exactly the trap —
it is modelled end to end and consumed nowhere.

Corroborating that this is a pattern rather than a one-off, `ops/harbour_build.rs`
already carries a comment (lines 202–206) recording a *previous* instance:
a `BuildIntent` was constructed here and discarded because `BackendValidator`,
its only production reader, is never called from that path.

---

## 3. Does the layout cause the unwired-code problem?

This was the most important question asked, and it deserves a precise answer
rather than a flattering one.

### What I can prove

**The single-crate-everything-`pub` layout disables rustc's own dead-code
detector across the entire library.** `src/lib.rs` declares six `pub mod`s, so
every `pub` item beneath them is reachable from the crate root, and `dead_code`
is switched off for all of them.

I verified this is the operative mechanism with a one-line experiment. Changing
the confirmed-dead `generate_default_manifest` from `pub fn` to `pub(crate) fn`
and running `cargo check`:

```
warning: function `generate_default_manifest` is never used
    --> src/core/manifest.rs:1135:15
     |
1135 | pub(crate) fn generate_default_manifest(name: &str, is_lib: bool) -> String {
     |               ^^^^^^^^^^^^^^^^^^^^^^^^^
     = note: `#[warn(dead_code)]` (part of `#[warn(unused)]`) on by default
```

CI runs `cargo clippy --all-targets --all-features -- -D warnings`
(`ci.yml:114`). **Had that function's visibility been honest, this would have
been a build failure rather than 18 lines of dead API.** So: yes, the layout
contributes, and the fix is a compiler lint that the project is currently
opting out of.

### What I cannot prove, and will not claim

**A workspace split would not have caught any of the eight-plus historical
bugs, and I can name the reason.** The failure mode is a *facility that is
constructed and then not consumed*, and crate boundaries do not detect that:

- `LinkGroup` would not be caught. I tested this directly — making
  `EffectiveLinkSurface.groups` a private field produced **no warning**, because
  `effective.groups.extend(...)` at `surface_resolver.rs:916` counts as a use.
  `dead_code` catches never-*touched* items, not written-but-never-consumed ones.
  No lint catches the latter.
- The `plan.rs:845` / `to_flags` duplication would not be caught: both files are
  in `builder` and would land in the *same* crate under any sensible split.
- The `BuildIntent` instance would not be caught, for the same reason.

I also ran a broader version of the visibility experiment — narrowing all six
top-level modules to `pub(crate)` and checking the lib alone — which produced
665 `dead_code` findings. **That number is not a dead-code count and should not
be quoted as one.** With the crate private, everything reachable only via
`src/bin` cascades to "unused"; spot-checking confirmed it, e.g. the entire
`builder/bindings` type model is flagged yet is genuinely wired through
`bin/harbour/commands/ffi.rs:10`. The only defensible statement is the
qualitative one: **rustc currently checks reachability for none of the
library's public surface, and would check all of it if the surface were
deliberate.**

### Conclusion for this section

Tightening visibility is worth doing, and it is worth doing *because* it turns
one class of unwired code into a CI failure — a proven benefit, at the cost of
adding `pub(crate)`. A workspace split buys the same lint benefit plus
cycle-impossibility, and does not buy detection of the project's actual dominant
bug shape. **The unwired-code history is an argument for visibility discipline
and for end-to-end tests. It is not an argument for a workspace.**

---

## 4. The workspace split, specified — and why not to do it

For the record, so this is a considered rejection rather than an unexamined one.
The split that would make sense:

| crate | owns | depends on |
|---|---|---|
| `harbour-util` | interning, hashing, fs, process, shell, diagnostics | — |
| `harbour-core` | manifest, package, target/triple, surface, workspace, dependency, `BackendId` | `harbour-util` |
| `harbour-sources` | registry, git, path, vcpkg sources, `SourceCache` | core, util |
| `harbour-resolver` | pubgrub integration, `Resolve` | core, sources, util |
| `harbour-builder` | toolchain detect, plan, native, shims, surface resolution | core, sources, resolver, util |
| `harbour-ops` | build/add/update/resolve/doctor orchestration | all of the above |
| `harbour-test-support` | mocks and fixtures | core |
| `harbour-cli` | `clap` layer, the `harbour` binary | ops |

Genuine benefits:

1. **Cycles become impossible**, not merely discouraged. Cargo rejects them.
   This is the strongest argument and the only one that a lint cannot replicate.
2. **`test_support` becomes an ordinary dev-dependency** and the
   `harbour-cli = { path = ".", features = ["test-support"] }` self-dependency
   plus the `test-support` feature both disappear. Worth noting the hack is
   currently load-bearing for exactly **three call sites**
   (`tests/cli_integration.rs:28`, `src/ops/resolve.rs:517`,
   `src/sources/registry/generate.rs:238`), all wanting only
   `fixtures::local_registry`.
3. Each crate's public API must be stated deliberately, which enables the
   `dead_code` benefit proven in §3.

Costs, weighed against 2.1s of rebuild:

- **Every `use crate::X` becomes `use harbour_x::X`** — **305 `use crate::`
  lines across 87 of the 119 files** in `src/`, plus the inline `crate::` paths.
  Mechanical, but it touches 73% of the source files.
- **The 10 cycle-forming references must be fixed first anyway.** The split
  cannot even be attempted until the graph is a DAG — so step 1 of the workspace
  plan is precisely the change recommended below, and after doing it, the
  remaining benefit is only #1 and #2 above.
- **Conflict surface.** A diff touching nearly every file collides with
  everything. The stated concern about PR #69 has in fact cleared — #69 merged
  at 2026-09-07T05:09:57Z and **there are currently zero open PRs** — so the
  window is unusually clean *right now*. That argues for doing the *small* fix
  now, not for doing the big one; the big one is not justified regardless of
  window.
- **CI cache behaviour gets worse before better.** Eight crates means eight sets
  of build artefacts; `Swatinem/rust-cache`-style keys churn on the first run,
  and `cargo test --all-features` at the workspace root recompiles the same
  amount of code. Per-crate caching only pays off when crates change
  independently, which for a project at this stage they mostly do not.

**Net: rejected.** The one benefit that survives scrutiny — cycle impossibility
— can be obtained at ~2% of the cost by fixing 10 references and adding a guard.

---

## 5. Sequencing

**Now, in this PR:**

- [x] Remove the `builder -> ops` back-edge by moving `Artifact` from
      `ops::harbour_build` to `builder::util`, re-exported for compatibility.
      1 of the 10 cycle references. `grep -rn 'crate::ops' src/builder/` is
      empty. Gate green: 588 + 194 + 50, identical to main.

**Next, as small independent PRs (each < 100 lines, no ordering constraint):**

1. **Move `BackendId` / `LinkagePreference` / `ArtifactKind` to
   `src/core/backend.rs`**, keeping the existing `builder::shim` re-exports so
   no importer changes. Kills `core -> builder` (2) and `util -> builder` (1).
2. **Move `validate_package_name` from `sources::registry` to `core`.** Kills
   `core -> sources` (3).
3. **Move `util::vcpkg` and the workspace-discovery part of `util::context`**
   out of `util`. Kills `util -> core` (2). Least mechanical of the three —
   decide the destination when doing it.
4. **After 1–3, add a CI guard** so the DAG cannot rot: a short test asserting
   the module graph is acyclic (the Tarjan check used for this review is ~40
   lines of Python over `grep 'crate::'` output, and can be a `#[test]`), or
   `cargo-modules`/`cargo-deny` equivalent.
5. **Delete the confirmed-dead items** from §2.4 and narrow the over-exposed
   ones to `pub(crate)`. This is where the `dead_code` benefit starts accruing;
   do it incrementally so `-D warnings` stays green.
6. **Fix or remove `LinkGroup`.** Either emit `--start-group`/`--end-group` from
   the link step and add an end-to-end test that a cyclic-dependency link
   actually succeeds, or delete the parsing and the surface field and reject the
   manifest key. Do not leave it modelled and unconsumed. Given this project's
   history, an end-to-end test is the only acceptable evidence of the former.

**Opportunistically, when next editing the file — not as a sweep:**

7. `compute_feature_sets` + its 511 test lines out of
   `builder/surface_resolver.rs` and into `core` (708 lines, one caller,
   currently on the wrong side of the layer boundary — this is the single
   highest-value extraction available).
8. The preprocessor lexer / header lint out of `surface_resolver.rs`
   (1,219–1,455) into `builder/header_lint.rs`.
9. `format_toml_error` out of `core/manifest.rs` into `util`; manifest text
   generation into `core/manifest/template.rs`; `triple_matches_pattern` into
   the existing `core/target/triple.rs`.
10. Unify the duplicated plain/provenance graph walks in `surface_resolver.rs`
    (495–921 vs 923–1,163), and make `plan.rs:845` call
    `EffectiveLinkSurface::to_flags` instead of reimplementing it.

**Reconsider the workspace only when one of these becomes true:**

- a second binary or an external consumer of the library appears (then a
  deliberate public API stops being optional);
- `cargo check --lib` from scratch exceeds ~15s (currently 2.9s — roughly 5x the
  present code volume);
- the acyclicity guard from step 4 starts being fought rather than respected.

---

## 6. Deliberately not recommended

- **A cargo workspace split.** Measured compile cost does not justify it (§1.3),
  and its one irreplaceable benefit is obtainable for ~10 line changes (§4).
- **Splitting files for line count.** `toolchain/detect.rs` (1,798) and
  `native.rs` (1,385) are large but coherent; `plan.rs`'s problem is one
  552-line function (`with_root_packages`, 327–878), which wants private helper
  extraction, not a module split. Only `surface_resolver.rs` and `manifest.rs`
  have genuinely separable concerns, and those are listed as opportunistic.
- **Removing the `test-support` self-dependency as its own task.** It is
  unusual-looking but functionally sound: the feature is not default, plain
  `cargo build` excludes it, and cargo unifies the cyclic dev-dependency into a
  single rlib (`cargo tree -e features -i harbour-cli` confirms). It buys three
  call sites; the churn to replace it is not worth paying outside a split.
- **Adding a default-features CI job on the strength of the feature-coverage
  gap.** It is true that every CI invocation passes `--all-features`
  (`ci.yml:114,152,171`, `macos.yml:54,59`) and no workflow runs a plain
  `cargo build`/`cargo check`. But the gap is narrower than it looks: the
  `[dev-dependencies]` self-dependency enables `test-support` unconditionally,
  so any `--all-targets` or `cargo test` invocation turns it on regardless of
  `--all-features`. The only genuinely unchecked configuration is
  default-features lib+bin — and I ran `cargo clippy --locked -- -D warnings`
  against it: **clean.** So this is latent, not live. Adding the step is cheap
  and mildly worthwhile, but it is a CI tidy-up, not a layout finding, and it
  should not be bundled here.
- **A "features exist but are not wired" audit as part of this work.** §2.4
  found one new instance (`LinkGroup`) as a side effect, and it should be fixed.
  But a systematic hunt is a different task with a different method — end-to-end
  execution, per this project's own testing discipline — and the layout review
  should not be stretched to cover it.

---

## Appendix: reproducing the measurements

Module graph and SCC — count *all* `crate::X` paths, not just `use` lines
(`core -> builder` is an inline path at `core/manifest.rs:358` and is invisible
to a `use`-only grep), and exclude `#[cfg(test)]` blocks to get the production
graph. Run Tarjan over the result.

Compile timings — append a **unique** comment to the file under test before each
run, or `kache` will serve a cache hit and report ~0.3s. Discard the first
reading after switching between `--all-features` and default features. Take the
non-incremental ceiling with `CARGO_INCREMENTAL=0`.
