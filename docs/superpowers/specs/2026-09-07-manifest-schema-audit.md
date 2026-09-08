# Audit: is the `Harbour.toml` manifest schema clean?

**Date:** 2026-09-07
**Status:** Audit — verdict is *the model is sound; the plumbing under it is not*
**Scope:** the whole `Harbour.toml` schema, with the six recent additions
(`when.include_dirs`, `when.prebuild`, `surface.when`'s private tables,
`freestanding`/`linker_script`/`entry`, `requires`/`supports`, assembly
sources, named-source validation and `exclude`) as the specific things under
suspicion.

Method: `MANIFEST.md` read against the code, and then **every claim in §2
reproduced by writing a manifest and running `target/debug/harbour`**. §5
separates what was proved by running from what is inferred from reading; the
inferred list is short and none of the headline findings are on it.

---

## Verdict up front

**The schema is coherent. It is not a pile of special cases.** Every one of
the six recent additions is in the right place for a defensible reason, the
two `when` mechanisms are a real distinction rather than an accident, and the
one asymmetry the brief suspected turns out to be deliberate and correctly
argued in the code. On the *design* question the answer is: this is fine, keep
it.

But the audit found ten defects, and the interesting thing about them is that
**not one is a schema-design problem.** They are all the same structural
problem wearing ten different hats:

> **Each schema field has between one and three independent consumers, and
> nothing forces them to agree.** `resolve_compile_surface` and
> `resolve_compile_surface_with_provenance` are two hand-maintained copies of
> one algorithm. `compile_commands.json` is a third. `BuildConfig::default()`
> was a fourth copy of the serde defaults. Every defect below is one of those
> copies having drifted.

Two of those defects are severe enough to name here:

1. **`cflags` are ASCII-sorted before they reach the compiler**
   (`surface_resolver.rs:600`), so a manifest writing
   `cflags = ["-Wall", "-Wno-error", "-Werror"]` compiles *without* warnings
   as errors — the exact opposite of what it says. A separate review concluded
   that ordering "becomes semantically load-bearing *if* flag removal is ever
   added". That is too generous: ordering is load-bearing **now**, in C, and
   the sort is already silently inverting it. §2.1.
2. **The same build, run twice, compiles with different flags** when a
   dependency has more than one library target, because "the default target"
   means "the first library target" and the target table is a `HashMap`. §2.2.

And one that had reached the compiler and is now fixed in this branch:
**`[build] exceptions` and `rtti` defaulted to `false`**, not the documented
`true`, for any manifest with no `[build]` section, so C++ packages were
compiled `-fno-exceptions -fno-rtti`. §2.3.

Recommendation, in order:

1. **Now (small, done in this branch):** the two fixes in §4 — reject dead
   keys in `[[targets.X.when]]`, restore the `exceptions`/`rtti` default — and
   correct `MANIFEST.md` where it describes behaviour the code does not have.
2. **Now (small, not done):** delete or reject the four confirmed-dead schema
   fields in §2.7. Each is a manifest a user can write today that does
   nothing.
3. **Next (medium, the real fix):** unify the plain and provenance folds
   (§3.1). This is prerequisite to *both* fixing the sort and adding flag
   removal, and it is what stops the next four defects of this shape.
4. **Not recommended:** restructuring the `when` split, merging
   `surface.when` into `targets.when`, or adding a third conditional
   mechanism. §3.2 argues the current split is right.

The distinction the brief asked for, applied:

| | |
|---|---|
| **Genuinely broken** | §2.1 flag sort, §2.2 nondeterministic default target, §2.3 exceptions/rtti default, §2.4 `harbour flags` disagrees with the build, §2.5 `compile_commands.json` omits C++ flags |
| **Ugly but sound** | §3.2 the two `when` blocks, §3.3 the target-level/surface-level asymmetry, three-ways-to-say-one-thing (§2.9) |
| **Documented gap** | §2.6 `compiler = "clang"` on macOS, §2.7 dead fields, §2.8 validation holes, `lto`, `kind = "path"` anchoring |

---

## 1. What the schema actually is

Nine top-level tables (`package`, `workspace`, `build`, `dependencies`,
`features`, `targets`, `profile`) and, per target, four ways to say something
about compilation:

```
[targets.X]                        kind, sources, exclude, public_headers,
                                   lang, c_std, cpp_std, freestanding,
                                   linker_script, entry, prebuild, recipe,
                                   backend, ffi
[targets.X.public]                 shorthand -> surface.compile.public
[targets.X.private]                             + surface.link.public
[targets.X.surface.compile.{public,private}]
[targets.X.surface.link.{public,private}]
[targets.X.surface.abi]
[targets.X.deps.<pkg>]
[[targets.X.when]]                 os/arch/env/compiler/feature +
                                   sources, exclude, defines, cflags,
                                   include_dirs, prebuild
[[targets.X.surface.when]]         os/arch/env/compiler/feature +
                                   compile.public, compile.private,
                                   link.public, link.private
```

The organising idea is a good one and it holds throughout: **public
propagates to dependents, private does not.** `Visibility` on a target dep
gates it per edge, `resolve_compile_surface` walks the graph adding only
`compile_public` from dependencies, and `warn_private_defines_in_public_headers`
exists specifically to catch the case where an author violates the invariant
in C rather than in TOML. That is a designed model, not an accretion.

`MANIFEST.md` covers most of it. Implemented and undocumented: `[features]`,
`surface.compile.requires_cpp`, `surface.abi`, `link.*.groups`,
`[targets.X.ffi]`, `libs` in the shorthand tables, `lang = "asm"`. Documented
and not implemented: `{ kind = "package", ... }` (§2.7), `lto` (§2.7). The
`Known Gaps` section added to `MANIFEST.md` in this branch records the ones
that are not going to be fixed immediately.

---

## 2. Defects

Every subsection gives a reproduction that runs. `$H` is
`target/debug/harbour`.

### 2.1 `cflags` are sorted, which silently inverts last-wins semantics

**Genuinely broken. Highest severity of the ten.**

`src/builder/surface_resolver.rs:598-601`:

```rust
effective.include_dirs.sort();
effective.include_dirs.dedup();
effective.cflags.sort();
effective.cflags.dedup();
```

C compiler flags are last-wins. Sorting them destroys the only mechanism the
schema has for overriding a flag.

```toml
[package]
name = "t1"
version = "0.1.0"

[targets.t1]
kind = "exe"
sources = ["src/a.c"]

[targets.t1.private]
cflags = ["-Wall", "-Wno-error", "-Werror"]
```

`src/a.c` is `int main(void){int x; return 0;}`.

```
$ cc -c -Wall -Wno-error -Werror src/a.c -o /dev/null
src/a.c:1:20: error: unused variable 'x' [-Werror,-Wunused-variable]
1 error generated.

$ $H build
    Finished debug [native] .../bin/t1 in 0.14s
```

The flags the author wrote, handed to `cc` in that order, fail the compile.
Harbour reorders them to `-Wall -Werror -Wno-error` and the build succeeds.
Confirmed in `compile_commands.json`, which lists them sorted.

The brief's framing was that "if flag removal is ever added, ordering becomes
semantically load-bearing". The sort means it already is, and already wrong.
Note also that this is *why* `CompileRequirements::merge`
(`src/core/surface.rs:594`) looks harmlessly commutative: order is thrown away
downstream regardless of what the merge does, so the commutativity is not a
property of the merge, it is a property of the sink.

There is a related consequence recorded honestly in the code:
`Target::link_control_flags` (`src/core/target/core.rs:461-466`) explains that
`linker_script` must be emitted as the single token `-Wl,-T,PATH` rather than
`["-T", "layout.ld"]` *because* the surface is sorted and a two-token flag
would be split apart. That is a schema constraint imposed by an accident of
the resolver, and it will bite the next multi-token flag someone needs.

**Fix:** remove the sort; dedupe order-preservingly (keep first occurrence for
`include_dirs`, keep *last* for `cflags`, or do not dedupe `cflags` at all).
Do it after §3.1, because doing it in one fold and not the other makes
`harbour flags` disagree in a new way.

### 2.2 The same build produces different flags on different runs

**Genuinely broken.**

`Manifest::default_target` (`src/core/manifest.rs:1104`) is "the first library
target", and `RawManifest.targets` is a `HashMap<String, RawTarget>`
(`src/core/manifest.rs:428`) drained into a `Vec`. Iteration order of that
`HashMap` is randomised per process, so "first" is random. A dependency with
two library targets therefore contributes a randomly chosen one — its public
surface *and* its archive.

Dependency `mylib` declares two staticlibs, `mylib` (public define
`FROM_FIRST_TARGET=1`) and `second` (`FROM_SECOND_TARGET=1`). `app` depends on
it with no `[targets.app.deps]` entry.

```
$ for i in $(seq 1 8); do rm -rf .harbour; $H build >/dev/null 2>&1
    python3 -c "import json;d=json.load(open('.harbour/compile_commands.json'))
    print([a for a in [e for e in d if e['file'].endswith('m.c')][0]['arguments'] if a.startswith('-DFROM')])"
  done | sort | uniq -c
   6 ['-DFROM_FIRST_TARGET=1']
   2 ['-DFROM_SECOND_TARGET=1']
```

Same manifest, same sources, two different compile commands. There is also no
way in the schema for a package to *say* which of its targets is the default —
Cargo has singular `[lib]`; Harbour has an unordered map and a positional
rule. The warning at `surface_resolver.rs:679` tells you only one library is
linked; it cannot tell you the choice is a coin flip.

**Fix (small, schema-level):** make `Manifest.targets` order-preserving
(`IndexMap`, or sort by name) so "first" is at least *stable*, and warn or
error when a package with several library targets is depended on without an
explicit `target = "..."`. **Fix (correct, larger):** a `default = true` target
key, or link all library targets in declared order.

### 2.3 `[build] exceptions` / `rtti` defaulted to `false`

**Genuinely broken. Fixed in this branch (§4.2).**

`MANIFEST.md` documents both as `default: true`, and the fields carry
`#[serde(default = "default_true")]`. That attribute fills a key missing from
a table that is *present*; `RawManifest.build` is `#[serde(default)]`, so a
manifest with no `[build]` section fell to the derived
`BuildConfig::default()` — `false`. `CppConstraints::compute`
(`src/resolver/cpp_constraints.rs:175`) copies it straight through to the
compiler.

Before the fix, with a manifest that has no `[build]` section at all:

```
$ $H build
error: use of dynamic_cast requires -frtti
error: cannot use 'throw' with exceptions disabled
```

Appending `[build]` with any single unrelated key (`cpp_std = "17"`) made the
same source compile. That is the tell: the two defaults disagreed.

### 2.4 `harbour flags` disagrees with the build, three ways

**Genuinely broken** — the command exists to be authoritative, and
`MANIFEST.md` points at it for exactly that ("what the linker receives is
inspectable without building").

`resolve_compile_surface` (`:495`) and
`resolve_compile_surface_with_provenance` (`:923`) are two copies of one
algorithm, and they have drifted in four places. Likewise
`resolve_link_surface` (`:797`) and its twin (`:1003`).

**(a) `compile = "private"` on a target dep is ignored.** The plain fold
checks `get_compile_visibility` at `:575-580`; the provenance fold has no such
check.

```toml
[targets.app.deps]
mylib = { target = "mylib", compile = "private", link = "public" }
```

```
$ $H flags app
# Compile flags for `app`:
  -I.../lib/include    # from: mylib 0.1.0 (surface.compile.public)
  -DMYLIB_PUBLIC=1     # from: mylib 0.1.0 (surface.compile.public)
```

The actual compile of `app`'s own source receives none of them
(`compile_commands.json` for `m.c`: `-c -O0 -g -g3` only). `harbour flags`
reports three flags that do not exist.

**(b) `target = "..."` on a target dep is ignored,** so the answer is
nondeterministic. The plain fold calls `get_dep_target` (`:584`); the
provenance fold calls `dep_package.default_target()` (`:982`), which is the
`HashMap` coin flip from §2.2. With `mylib = { target = "second" }` pinned so
the *build* is deterministic:

```
$ for i in $(seq 1 10); do $H flags app 2>/dev/null | grep -o 'FROM_[A-Z]*_TARGET'; done | sort | uniq -c
   6 FROM_FIRST_TARGET
   4 FROM_SECOND_TARGET
```

The build used `FROM_SECOND_TARGET` every time. `harbour flags` was right 4
times out of 10.

**(c) It reports a `-L` the real link deliberately omits.** The plain fold
pushes only `dep_libs` and carries a 12-line comment
(`surface_resolver.rs:858-868`) explaining that a matching `-L` is *not*
inert: every `-L` also applies to the driver's implicit libraries, so a
package named `c` on the search path shadows the system libc. The provenance
fold pushes `lib_dirs` anyway (`:1081-1085`), and `EffectiveLinkSurfaceWithProvenance::to_flags`
turns each into `-L`. So `harbour flags` prints a flag whose absence is a
documented safety property. (`harbour linkplan`'s "Link line" does not show
it — the two inspection commands contradict each other.)

**(d) Ordering and the private-define ABI warning.** The plain fold sorts and
dedupes; the provenance fold does not, so `harbour flags` shows manifest order
and the compiler receives ASCII order (§2.1). The plain fold calls
`warn_private_defines_in_public_headers` (`:562`); the provenance fold does
not, so the ABI-trap warning never fires under `harbour flags`.

The provenance fold also has no `groups` field at all, and skips the
`target.deps`-exist validation the plain fold does at `:509-531`.

### 2.5 `compile_commands.json` omits every C++ language flag

**Genuinely broken.** `native.rs:723` passes the real `cxx_opts` to
`compile_command`; `plan.rs:895`, which writes the compile database, passes
`None`. `-std=`, `-fno-exceptions`, `-fno-rtti` and `-stdlib=` are all emitted
inside `if lang == Language::Cxx { if let Some(opts) = cxx_opts { ... } }`
(`toolchain/gcc.rs:159-182`), so none of them appear.

Same manifest as §2.3 with `exceptions = false`:

```
$ $H build
error: cannot use 'throw' with exceptions disabled     # the real compile got -fno-exceptions
$ python3 -c "..."                                     # what clangd will read
['-c', '-O0', '-g', '-g3', '-o']
```

`clangd` parses the file as exceptions-enabled C++ at the default standard,
while the build compiles it as `-std=c++17 -fno-exceptions -fno-rtti`. This is
the third independent implementation of "what flags does this file get".

### 2.6 `compiler = "clang"` never matches on macOS

**Documented gap** (now documented; the behaviour is arguably correct).

`compiler_family` (`src/builder/context.rs:298`) produces four values: `gcc`,
`clang`, `apple-clang`, `msvc`. `MANIFEST.md` said conditions support
`compiler` and `surface.rs:410` documents `"gcc", "clang", "msvc"`. Neither
mentioned `apple-clang`, which is the value on every Mac.

```toml
[[targets.t1.surface.when]]
compiler = "clang"
[targets.t1.surface.when."compile.private"]
defines = ["MATCHED_CLANG=1"]

[[targets.t1.surface.when]]
compiler = "apple-clang"
[targets.t1.surface.when."compile.private"]
defines = ["MATCHED_APPLE_CLANG=1"]
```

```
$ $H build && python3 -c "..."
['-DMATCHED_APPLE_CLANG=1']
```

`harbour new`'s scaffold already works around this by emitting four blocks
(`msvc`, `gcc`, `clang`, `apple-clang`) where three would do. That workaround
existing in the tool's own scaffold, undocumented, is the smell. A manifest
author guarding a clang-only flag will write `compiler = "clang"` and get
nothing on macOS, silently.

**Fix:** either document it (done) or add a family-group condition so
`compiler = "clang"` means "any clang". The latter is a schema addition and
should be a deliberate decision, not a drive-by.

### 2.7 Four schema fields parse and are never consumed

**Documented gap**, and this is the failure mode the brief correctly
identified as dominant. Searched systematically by grepping each schema field
for a non-test consumer, since the crate's root `pub mod`s suppress
`dead_code`.

| Field | Status | Evidence |
|---|---|---|
| `surface.link.*.groups` (`LinkGroup`, `src/core/surface.rs:294`) | parsed, merged, reaches `EffectiveLinkSurface.groups`, **never emitted** | the code says so at `surface_resolver.rs:737-739`; run below |
| `surface.abi.toggles` (`src/core/surface.rs:305`) | parsed, stored, propagated into `ResolvedSurface.abi`, **never read** | `AbiIdentity::with_surface` (`src/core/abi.rs:81`) is its only consumer and has zero non-test callers, so `toggles` *and* `public_defines` never enter any cache key |
| `libs = [{ kind = "package", ... }]` | parses, `to_flags()` returns `vec![]` with the comment "Resolved during build planning" | `LibRefObject::Package` is matched in exactly one place (`src/core/surface.rs:264`) — the site that discards it |
| `[profile.X] lto` | emitted at link only, so LTO is not enabled | `context.rs:251` adds `-flto` in `profile_ldflags`, nothing in `profile_cflags` |

`groups` and `kind = "package"` in one run:

```toml
[targets.t1.surface.link.private]
groups = [{ kind = "start_end_group", libs = ["foo", "bar"] }]
libs = [
    { kind = "package", name = "nonexistent_package", target = "nope" },
    { kind = "path", path = "vendor/libfoo.a" },
    { kind = "system", name = "m" },
]
```

```
$ $H linkplan t1
 WARN target `t1`: LinkGroup is parsed but platform support varies - may cause link errors on some platforms
Link line (what the linker receives, in order):
  .../obj/t1/src/a.o
  vendor/libfoo.a
  -lm
```

No `--start-group`. No error, and no output at all, for a `kind = "package"`
naming a package that does not exist. Note also the `LinkGroup` warning is
*misleading*: it says platform support varies, implying the flag is emitted
somewhere. It is emitted nowhere.

`vendor/libfoo.a` in that link line is a second, smaller finding:
`kind = "path"` is passed **verbatim**. `add_compile_requirements` takes a
`root: &Path` and anchors `include_dirs` to it (`:892-906`);
`add_link_requirements` takes no root at all (`:912`). So a relative library
path resolves against the process working directory — the *root* package's
directory when this package is a dependency — which is precisely the hazard
`MANIFEST.md` spends a paragraph warning about for `-I`. Same bug, link side,
undocumented until this branch.

### 2.8 Two more places where "unknown fields are rejected" is false

**Documented gap.** `MANIFEST.md`'s Validation section claims unknown fields
are rejected for typo detection. `#[serde(flatten)]` defeats
`deny_unknown_fields`, and there were three flatten sites, not one.

- `ConditionalSurface` (`src/core/surface.rs:330`) — guarded, by the
  hand-rolled `validate` at `:371`. This is the one the brief knew about.
- `ConditionalSources` (`src/core/target/core.rs:631`) — **was unguarded.**
  Fixed in this branch; see §4.1 for the reproduction, which is worth reading
  because the swallowed key is not a typo.
- `RawTargetDep::Detailed` (`src/core/manifest.rs`, `#[serde(untagged)]`) —
  **still unguarded**, and doubly so. An untagged struct variant ignores
  unknown keys, and `convert_target_dep` (`:1015`) compares the values against
  the literal `"private"`:

  ```rust
  compile: compile.map(|s| if s == "private" { Private } else { Public })
  ```

  So `compil = "private"` (misspelled key) and `compile = "PRIVATE"`
  (miscased value) both silently mean *public*. Proved: a manifest with
  `mylib = { target = "second", compil = "private" }` parses without
  complaint and the dep's public compile surface propagates.

There is a fourth, narrower hole: `Target::validate`'s `libs`-must-be-a-name
check (`src/core/target/core.rs:295`, the loop at `:398`) inspects only
`surface.link.public` and `surface.link.private`, never
`surface.conditionals`. So the check fires:

```
$ $H flags t1     # libs = ["libssl.a"] under [targets.t1.private]
error: target 't1' lists `libssl.a` in a link surface's `libs`, but `libs` takes link names
```

and does not fire for the same value one table deeper:

```
$ $H flags t1     # libs = ["libssl.a"] under [[targets.t1.surface.when]] link.private
# Link flags for `t1`:
  -llibssl.a    # from: t1 0.1.0 (surface.link.private)
```

`-llibssl.a` makes the linker look for `liblibssl.a.a`. Same class as
everything else here: the validation has one implementation and the schema has
two paths to the same field.

### 2.9 Three ways to say the same thing, resolved alphabetically

**Ugly but sound**, with a documentation gap.

A private cflag can be declared three ways: `[targets.X.private] cflags`,
`[[targets.X.when]] cflags`, and `[[targets.X.surface.when]]`'s
`compile.private.cflags`. All three land in the same `CompileRequirements` and
all three are additive, so the question "which wins" has no answer — they all
apply, and then §2.1's sort decides the order.

```
# Compile flags for `t1`:
  -I.../vendored/darwin        # [[targets.t1.when]] include_dirs
  -DVIA_TARGET_WHEN=1          # [[targets.t1.when]] defines
  -DVIA_SURFACE_WHEN_CFLAG     # [[targets.t1.surface.when]] compile.private.cflags
  -DVIA_TARGET_WHEN_CFLAG      # [[targets.t1.when]] cflags
```

This is *not* a defect on its own. Three spellings of "add a private compile
requirement" is redundancy, not incoherence, and the shorthand
(`[targets.X.private]`) genuinely earns its keep on ergonomics. But it is
undocumented that they compose rather than override, and it means a user
debugging "why is this flag here" has three tables to check and no ordering
guarantee between them. Documented in this branch.

### 2.10 Summary table

| # | Defect | Class | Location |
|---|---|---|---|
| 2.1 | `cflags` ASCII-sorted, inverts last-wins | broken | `surface_resolver.rs:598-601` |
| 2.2 | default target is a `HashMap` coin flip | broken | `manifest.rs:428`, `manifest.rs:1104` |
| 2.3 | `exceptions`/`rtti` default `false` | broken (fixed) | `manifest.rs` `BuildConfig` |
| 2.4 | `harbour flags` ≠ the build, 4 ways | broken | `surface_resolver.rs:923`, `:1003` |
| 2.5 | `compile_commands.json` has no C++ flags | broken | `plan.rs:895` |
| 2.6 | `compiler = "clang"` dead on macOS | doc gap | `context.rs:298` |
| 2.7 | `groups`, `abi.toggles`, `kind="package"`, `lto` dead; `kind="path"` unanchored | doc gap | `surface.rs:294`, `:305`, `:264`; `surface_resolver.rs:912` |
| 2.8 | 3 unguarded flatten/untagged sites; `libs` check skips conditionals | doc gap (one fixed) | `target/core.rs:631`, `manifest.rs:1015`, `target/core.rs:398` |
| 2.9 | three additive spellings, alphabetical resolution | ugly but sound | — |

---

## 3. The design questions

### 3.1 The duplicated folds are the actual problem — confirmed, and worse than "risk"

The brief asked me to confirm the duplication and judge how much risk it
carries. Confirmed: `resolve_compile_surface` (`:495-604`) /
`resolve_compile_surface_with_provenance` (`:923-997`) and
`resolve_link_surface` (`:797-890`) / `resolve_link_surface_with_provenance`
(`:1003-1102`) are four functions implementing two algorithms, with
`add_*_requirements` / `add_*_requirements_with_provenance` doubled underneath
them. Both copies carry a comment instructing the reader to keep them in sync
(`:959-960`, `:1021`) — which is the standard sign that the mechanism for
keeping them in sync is a human remembering.

The judgement: **this is not a risk, it is five realised bugs.** §2.4 is four
of them (visibility, target selection, `-L`, ordering + the missing ABI
warning) and §2.5 is the third copy diverging as well. The prior review's
conclusion — that unification must precede adding flag removal — is right, but
understated. Unification must precede fixing the sort in §2.1 too, and
`harbour flags` is wrong *today* in ways a user would reasonably call lying.

The unification is mechanical and the shape is obvious: make the fold generic
over the accumulator, or always compute with provenance and project it away.
`EffectiveCompileSurface` is literally `EffectiveCompileSurfaceWithProvenance`
with the `Provenance` field dropped; `to_flags` on each is the same function
twice (`:134-150` vs `:1164-1181`). `compile_commands.json` should then be
written from the same result the compile uses, not recomputed at `plan.rs:895`.

Size: roughly 670 lines of `surface_resolver.rs` collapse to about 350. It is
the single highest-value change identified by this audit, because it converts
four of the ten defects from "fix each" to "cannot recur".

### 3.2 Two `when` mechanisms: principled, not accidental

**Refuted as a suspicion.** The split is defensible and the code already
argues it, at length and correctly, in two places
(`src/core/target/core.rs:137-147` and `:556-571`).

The distinction is: `[[targets.X.when]]` patches **build inputs private to
this target** — which files compile, with which private flags, after which
generators. `[[targets.X.surface.when]]` patches **the contract exported to
dependents**. Those are different kinds of fact. Source selection is not a
thing a dependent can observe; a public define is.

Two concrete arguments the code makes that I checked and agree with:

- Collapsing them would let a change to *what files get compiled* be read as
  a change to *the dependency surface* (`core.rs:144-147`). `Surface` is the
  thing whose public half propagates transitively; putting `sources` inside
  it invites exactly that confusion.
- One feature toggle routinely needs to add a source file *and* the private
  define that makes it compile in — sqlite's `SQLITE_ENABLE_FTS5` is named as
  the case (`core.rs:560-567`). Splitting those two effects of one toggle
  across two `[[...]]` blocks with duplicated conditions would be worse
  ergonomics for no gain. This argument is why `defines`/`cflags` are on
  `targets.when` at all, and it is a good one.

**Could one absorb the other?** Technically yes — `surface.when` is the more
expressive of the two, so `targets.when` could become sugar for it plus a
`sources`/`exclude`/`prebuild` extension. But it would not be cleaner. The
merged block would accept ten keys of which four are meaningless for a
`header-only` target and four more are meaningless in the private direction,
and the conditions would have to be written twice as often for the sqlite
case. The current split has a one-sentence rule ("does a dependent see it?")
that a user can apply. Keep it.

What is *not* fine is that the two blocks were validated to different
standards — one guarded, one wide open (§2.8, fixed in §4.1). The split is
sound; the enforcement of the split was not.

### 3.3 The asymmetry: deliberate, and the right way round

**Refuted as a suspicion, with a caveat.** The brief asks whether
`targets.when` patching private *compile* but not private *link* is a reason
or an accident. It is a reason, and it follows directly from §3.2.

`targets.when` is about **this target's own translation units**: which files,
which private compile flags, which generated headers. There is no
"translation unit" for linking — a static library has no link step at all
(`plan.rs` only archives it), so a per-platform link flag is intrinsically a
property of whoever links, i.e. of the surface. Adding `ldflags` to
`targets.when` would either duplicate `link.private` or create a second,
subtly different notion of a private link requirement.

`MANIFEST.md` already relies on the split being this way round: the
freestanding section (`:366-368`) tells you to declare `-fuse-ld=lld` in
`[[targets.NAME.surface.when]]`'s `link.private`, which is correct.

The caveat, and it is the whole reason this asymmetry mattered: **until this
branch, writing `ldflags` in `targets.when` was silently accepted and
dropped.** The asymmetry was defensible; discovering it was not possible. A
user following the natural instinct — "I already have a `[[targets.X.when]]`
block for `os = "macos"`, I'll put the linker flag there" — got no flag and no
message. That is fixed in §4.1, and the error names the correct home.

### 3.4 Everything is additive: sound as a model, unsound as implemented

**Confirmed, with a correction to the framing.**
`CompileRequirements::merge` (`src/core/surface.rs:594`) and
`LinkRequirements::merge` (`:608`) are pure `.extend()`. There is no removal
operator for defines or flags; `exclude` exists for sources only.

Additive-only is a legitimate design. It is what CMake's
`target_compile_options` does, it composes across a graph without ordering
rules, and it means a dependency cannot break its dependent by *removing*
something. Choosing it is not a defect.

But two things are wrong with how it lands:

1. The commutativity is not actually coming from the merge. It is coming from
   the sort in §2.1, which throws order away *after* the merge. So the model
   is "additive and unordered" while the compiler's model is "additive and
   last-wins", and the mismatch is currently resolved by ASCII collation. That
   is not a design choice anyone made.
2. Additive-only has a real cost that is worth stating: the standard C
   escape hatch for "this platform needs `-Wno-x` because a dependency's
   header trips `-Wall`" is unavailable, and the workaround is to not add
   `-Wall` under that condition — which means duplicating the base flag list
   per platform.

**Recommendation:** keep additive-only. Fix the sort (§2.1) so that
last-wins works, which recovers most of what a removal operator would buy at
a fraction of the semantic cost. Revisit removal only if a real package needs
it after that. And do §3.1 first: with two folds, a removal operator would
have to be implemented twice and would diverge, which is precisely the prior
review's conclusion.

---

## 4. Fixes implemented in this branch

Both are small, self-contained, and have a test that fails without them.
Neither touches the design.

### 4.1 Reject unknown keys in `[[targets.X.when]]`

`ConditionalSources` flattens its `PlatformCondition`, so serde routed every
unrecognised key into the condition instead of rejecting it. Before:

```toml
[[targets.t1.when]]
os = "macos"
defines = ["FROM_TARGET_WHEN=1"]
ldflags = ["-Wl,-dead_strip"]
libs = ["m"]
totally_bogus_key = ["x"]
```

```
$ $H flags t1
# Compile flags for `t1`:
  -DFROM_TARGET_WHEN=1    # from: t1 0.1.0 (surface.compile.private)

# Link flags for `t1`:
```

Three keys accepted and discarded in silence, one of them a plain typo. The
same manifest against `surface.when` errored correctly, which is what made
this an inconsistency rather than just a hole.

After:

```
error: target `t1`: invalid `when` block: unknown key(s) in a `when` block: ldflags
hint: a target-level `when` block takes the conditions `os`, `arch`, `env`, `compiler`, `feature`, and the keys `sources`, `exclude`, `defines`, `cflags`, `include_dirs`, `prebuild`
note: `ldflags` belongs in `surface.when`'s `link.private`
```

Implementation mirrors `ConditionalSurface`: an `unknown` catch-all, the five
condition names filtered out, the rest rejected. The `note:` line is the part
worth keeping — `ldflags`/`libs`/`frameworks`/`groups` are not misspellings,
they are a user asking for §3.3's capability in the wrong block, and an error
that only says "unknown" makes the fix a guess.

Test: `core::manifest::tests::unknown_keys_in_a_target_level_when_block_are_rejected`.
Covers a typo, a misplaced key (asserting the error names `link.private`), and
all six legitimate keys still parsing alongside a condition.

### 4.2 Restore the `true` default for `[build] exceptions` / `rtti`

§2.3. `Default` for `BuildConfig` is now written by hand so it cannot drift
from the serde field defaults, and the test asserts they agree.

Test: `core::manifest::tests::exceptions_and_rtti_default_true_with_and_without_a_build_section`.

### 4.3 `MANIFEST.md` corrections

Behaviour that was proved by running and was either undocumented or
documented wrongly: the fourth `compiler` value, additive-and-sorted merge
semantics with the `-Werror` example, the two `when` blocks taking different
keys, `kind = "package"` emitting nothing, `kind = "path"` not being anchored,
`lto` being link-only, the validation that does exist, the flatten holes that
remain, and a `Known Gaps` section for §2.4, §2.5, §2.2 and §2.7.

Gate: `cargo fmt --all` clean, `cargo clippy --all-targets --all-features
--locked -- -D warnings` clean, `cargo test --all-features --locked` green at
**590** (588 + 2) unit, 194 bin, 54 integration.

---

## 5. Proved by running vs. inferred from reading

The brief asked for this distinction to be sharp, because this repo has
repeatedly had fields that parse and do nothing.

**Proved by running** (`target/debug/harbour build` / `flags` / `linkplan`,
with `.harbour/compile_commands.json` or a deliberately failing compile as the
witness):

- §2.1 flag sort inverting `-Werror` / `-Wno-error` — the strongest evidence
  in this audit, because `cc` with the author's own flag order *fails* and
  Harbour's build *succeeds*.
- §2.2 nondeterministic default target, 8 clean builds, 6/2 split.
- §2.3 `-fno-exceptions -fno-rtti` reaching the compiler, and disappearing
  when an unrelated `[build]` key is added.
- §2.4 (a) `compile = "private"` reported but not applied; (b) `harbour flags`
  flapping 6/4 across 10 runs; (c) the `-L` present in `flags` and absent from
  `linkplan`'s link line.
- §2.5 `compile_commands.json` lacking `-fno-exceptions` while the build
  errored on `throw`.
- §2.6 `MATCHED_APPLE_CLANG` present, `MATCHED_CLANG` absent, on macOS.
- §2.7 no `--start-group` in the link line; `kind = "package"` producing no
  output at all; `vendor/libfoo.a` emitted as a bare relative path.
- §2.8 all three: the target-level `when` swallowing `totally_bogus_key`, the
  `compil = "private"` typo parsing, `libs = ["libssl.a"]` rejected in the
  unconditional table and accepted inside `surface.when`.
- §2.9 all four spellings landing in one flag list.
- That the previously-reported `surface.when` `compile.private` bug is
  genuinely fixed: `harbour new`'s scaffold now puts `-Wall -Wextra` on the
  compile line.
- That the rest of the recent additions work: `.S` to `cc` and `.cpp` to
  `c++` per file, `exclude` dropping a named source, named-source existence
  validation erroring with the right filename, `supports` warning with the
  canonical triple named, `when.include_dirs` producing an anchored `-I`,
  every `[profile]` field reaching the command line except `lto`.

**Inferred from reading**, with the reasoning stated so it can be checked:

- `surface.abi.toggles` being dead. `AbiIdentity::with_surface`
  (`src/core/abi.rs:81`) is the only reader, and grep finds no non-test
  caller; the three `AbiIdentity::new` sites (`native.rs:442`, `:473`,
  `fingerprint.rs:770`) do not chain it. I did not construct a cache-key
  experiment, because a manifest edit invalidates the fingerprint by other
  means and the negative result would not be attributable.
- The MSVC-specific claims in `MANIFEST.md` (assembly rejected under MSVC,
  freestanding keys rejected under MSVC, `/MD` vs `/MT`). Not reachable on
  this machine; the code paths exist (`plan.rs:685-697`, `toolchain/msvc.rs`).
- `[package] requires` being enforced. `TargetEnvironment::is_satisfied_by`
  (`src/core/manifest.rs:281`) is correct — freestanding code satisfies any
  triple, hosted code fails on a bare-metal one — and `plan.rs:1031` calls
  it, but reaching the failing case needs `--target-triple
  thumbv7em-none-eabi` and toolchain detection errors out first on this
  machine ("no archiver here keeps its objects"). The advisory half,
  `supports`, was proved.
- The Apple-target freestanding link failure and the Windows path-separator
  mixing, both already recorded as unverified in `MANIFEST.md`.
- That `lto` is ineffective. `-flto` is absent from `profile_cflags` and
  present in `profile_ldflags` (`context.rs:238`, `:251`), and I confirmed by
  running that no `-flto` appears in the compile command with `lto = true`;
  what I did *not* do is verify the resulting binary is un-LTO'd.
- Line counts for the §3.1 unification estimate.

---

## Appendix: reproducing

`cargo build`, then for each case write the manifest shown and run from its
directory. The two-package cases put `app` and `lib` side by side with
`mylib = { path = "../lib" }`. Compile commands are read out of
`.harbour/compile_commands.json`; `rm -rf .harbour` between runs when testing
for nondeterminism, since a warm cache hides it.

Machine: Apple M4 Pro, macOS 25.6, Apple clang via `/usr/bin/cc`. §2.6 is
macOS-specific by construction. §2.2 and §2.4(b) depend on `HashMap`
randomisation and may need more than 8 runs to show both outcomes on another
platform.
