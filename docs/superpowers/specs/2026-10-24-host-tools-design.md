# Host tools: programs Harbour builds for the build machine

Status: proposal, for review. Nothing here is implemented.

## Why this exists

Two real packages generate their own sources by compiling a program and running
it, and the program has to run on the machine doing the build:

- **LuaJIT** compiles `minilua`, runs it to produce `buildvm_arch.h`, compiles
  `buildvm` against that, and runs `buildvm` to emit the VM assembly and seven
  tables.
- **CPython** compiles `_freeze_module`, freezes four modules, compiles
  `_bootstrap_python` from the *same* objects, freezes twenty more, and only
  then compiles the interpreter. The Linux kernel's `fixdep` and `asm-offsets`
  have the same shape (see `2026-09-07-crate-layout-review.md` §6.1).

Harbour can build both today, because of two small pieces already merged:
`HARBOUR_HOST_CC` (#160) gives a generator a compiler for the build machine, and
`inputs` fingerprinting (#163) stops the generator re-running when nothing
changed. Both packages were built through Harbour as scratch shims and the
frozen headers are byte-identical to CPython's own build. This document is about
what those shims cost, and what replaces them.

### What the shims cost (CPython 3.14.9, macOS arm64, measured)

| | |
|---|---|
| host programs compiled by the generator script | 3 (`_freeze_module`, `_bootstrap_python`, plus the 187-object core they share) |
| sources compiled for the host by the script | 187, then the same 187 again by Harbour for the target |
| shell bootstrap, cold | ~12 s of wall clock at `-O0`, 27 s of CPU |
| the script's compile logic | duplicated from the manifest: flags, defines, include dirs |
| incrementality inside the script | none: change one source and all 187 recompile |
| correct for a real cross build | no (see "Cross-compiling" below) |

The duplication is the part that matters. Every flag the manifest knows, the
script knows a second time, and nothing keeps them in agreement.

## What Harbour can and cannot express today

Verified by running, not by reading:

1. **A target cannot depend on another target in the same package.**
   `[targets.tool.deps] core = "core"` fails with
   `dependency core not found, add it to [dependencies]`. `[targets.X.deps]`
   names *packages*. CPython wants one `core` library and three programs
   (`_freeze_module`, `_bootstrap_python`, `python`) over it. Today that is only
   possible by splitting one source tree into several packages.
2. **A generator cannot depend on a program Harbour built.** A `prebuild` step
   runs an external `program` by name. There is no way to say "run the `buildvm`
   target of this package".
3. **Planning runs generators.** `BuildPlan` runs each `prebuild` step while it
   is being computed, because a target's sources come from globbing its
   generators' output (`plan.rs`, `PrebuildStep`). A tool must therefore be
   *compiled and linked* during planning, before the plan it belongs to exists.
4. **One toolchain, one target per build.** `BuildContext` carries a single
   toolchain, triple, compiler identity and platform (§6.1 of the layout
   review accepted this as a constraint). A tool needs a second context: the
   host's.

## Goals

- A package declares its host programs as targets, builds them with its own
  manifest's flags, and generators run them. No second copy of the flags.
- The `core` objects shared by a tool and the final program compile **once**
  when host and target are the same machine.
- A tool is rebuilt only when its sources, flags or toolchain change, as any
  target is.
- Cross-compiling builds tools for the host and the program for the target.

## Non-goals

- **Not Cargo's full build-dependency model on the first pass.** Cargo gives a
  package separate *dependency graphs* for host and target (so a build script's
  dependencies can use different features and versions than the library's).
  That is the right end state if the kernel is ever attempted, and it is a much
  larger change to the resolver. Phase 4 below.
- Not running target binaries (emulators, `qemu-user`) to execute tools.
- Not tracing what a tool reads. `inputs` stays the author's promise.

## Decisions that are yours

These change what gets built; each has a recommendation.

1. **Spelling of a host target.** Recommended: `host = true` on a target.
   `[targets.freeze_module] kind = "exe", host = true`. Alternative: a new
   `kind = "tool"`. A flag keeps `kind` about *what* is produced and `host`
   about *for whom*; a new kind would duplicate `exe`.
2. **Spelling of a same-package dependency.** Recommended:
   `[targets.tool.deps] core = { local = true }`, because the existing string
   form is taken (`core = "core"` means "package `core`'s target `core`") and
   silently preferring a same-named local target would change what existing
   manifests mean. Alternative: allow a bare target name when no package of
   that name exists, which is shorter and ambiguous.
3. **How a generator names a tool.** Recommended: `tools = ["freeze_module"]`
   on the prebuild step, which orders the build and sets
   `HARBOUR_TOOL_FREEZE_MODULE` to the built program's path. Alternative: let
   `program` itself name a target (`program = "target:freeze_module"`).
4. **Whether to share objects between host and target builds.** Recommended:
   yes, but as its own phase and only when the compile fingerprints are
   identical. Not doing it leaves CPython compiling 187 files twice on a
   native build, which is what the shim does now.

## Design

### Target dependencies inside a package (phase 1)

A `local` dependency resolves against the package's own targets. It
propagates surfaces exactly as a package dependency does (`compile`/`link`
visibility), is ordered before its dependent, and a cycle is an error with the
full path. This is useful on its own: any package with a library and a test
program or a command-line front end wants it, and today cannot have it.

### Host targets and the host context (phase 2)

`host = true` marks a target built for the build machine:

- It is planned with a **host `BuildContext`**: the host toolchain, host triple
  and host platform. `when` blocks on it, and on every target it depends on
  within the package, evaluate against the host platform. For CPython that is
  what selects the *host's* `pyconfig.h` for the tools and the target's for the
  interpreter, with no new mechanism.
- It is built **during planning**, immediately before the first generator that
  names it, by planning and executing a sub-plan for that target and its local
  dependencies, then returning its path. This follows how generators already
  work (planning owns the side effect, `plan.rs`) rather than splitting
  planning from execution, which would be a far larger change.
- Its output directory is separate from the target build's
  (`.harbour/target/<profile>/host/…`) so a native build cannot confuse the two.
- It is **not** part of the package's artifacts for the target: it is never
  linked into anything, never installed, and does not appear in the lockfile's
  link graph.

### Generators use tools (phase 2)

```toml
[targets.core]            # shared by the tools and the interpreter
kind = "staticlib"
sources = [ ...187... ]

[targets.freeze_module]
kind = "exe"
host = true
sources = ["Programs/_freeze_module.c", "Modules/getpath_noop.c"]
[targets.freeze_module.deps]
core = { local = true }

[[targets.python.prebuild]]
tools = ["freeze_module"]
program = "sh"
args = ["harbour/freeze.sh"]       # runs $HARBOUR_TOOL_FREEZE_MODULE
inputs = ["Lib/os.py", ...]
outputs = ["Python/frozen_modules/os.h", ...]
```

The staged bootstrap (`_freeze_module` → `_bootstrap_python`) is a chain: the
second tool's sources include headers the first tool generates. That falls out
of phase 2 without more mechanism if a host target may itself have `prebuild`
steps that name earlier host tools, so that is a requirement on the
implementation, not a stretch goal.

### Sharing objects (phase 3)

On a native build the tool's `core` objects and the interpreter's are compiled
from the same sources with the same flags. Compile fingerprints are already
content-addressed (`fingerprint.rs`); the missing piece is an object cache keyed
by that fingerprint instead of by the target's own object directory, so a second
target with an identical fingerprint links the existing object. When host and
target differ the fingerprints differ and nothing is shared, correctly.

### Cross-compiling

The reason this needs a design and not another environment variable: in a real
cross build the tool must be compiled *and configured* for the host. CPython's
`pyconfig.h`, LuaJIT's `-DLUAJIT_TARGET=LUAJIT_ARCH_x64` and the kernel's
`ARCH` all describe a machine. A shim that compiles tools with the target's
configuration builds host programs that are wrong, or that cannot run at all.
Evaluating a host target's `when` blocks against the host platform is the
mechanism that makes the right file get chosen; it is the main reason the host
context in phase 2 is a context and not just a different compiler path.

## Phases

| | | |
|---|---|---|
| 0 | `HARBOUR_HOST_CC` (#160), `inputs` skipping (#163) | **done** |
| 1 | same-package target dependencies (`local = true`, as recommended in decision 2) | **done**; unblocks `core` + tools in one package |
| 2 | `host = true` targets, host context, `tools = [...]` on prebuild | removes the shell bootstrap; LuaJIT and CPython shims drop their scripts |
| 3 | object sharing across contexts by fingerprint | removes the double compile on a native build |
| 4 | separate host and target dependency graphs and feature sets | only if the kernel is attempted; larger resolver change |

Each phase leaves the tree working and has its own tests.

## Verification

- **Phase 1:** a package with a library and an executable depending on it
  builds, the executable links the library, a cycle is rejected with the full
  loop named, and a same-named package dependency still means what it meant.
- **Phase 2:** the LuaJIT shim with no `sh` bootstrap builds natively and
  cross to the other architecture. The CPython frozen headers remain
  byte-identical to the reference build. A host target's `when` blocks evaluate
  against the host: asserted with a tool that prints the platform it was built
  for, under `--target-triple` for another architecture. The cross test on an
  architecture the host cannot run (armhf from arm64) builds the tool for the
  host and the program for the target.
- **Phase 3:** a native CPython build compiles each core source once, asserted
  by counting object files and compile invocations; a cross build still
  compiles them twice.

## Open questions

- Does a host target's output belong in `compile_commands.json`? Tools are
  something a developer might want to debug, but the file describes the target
  build; leaning no, behind a flag.
- Should `harbour build -p NAME` build a package's host targets when `NAME` is
  not the target that needs them? Leaning no: they are built on demand.
- What does `harbour clean` remove? Host outputs live under the same profile
  directory, so the same command; stated here so it is a decision and not an
  accident.
