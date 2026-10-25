# Harbour Architecture

Harbour is a C/C++ package manager and build system written in Rust. It provides a Cargo-like developer experience for C and C++ projects, handling dependency resolution, build orchestration, and cross-platform toolchain abstraction.

---

## Table of Contents

1. [High-Level Overview](#high-level-overview)
2. [Core Concepts](#core-concepts)
3. [Builder System](#builder-system)
4. [Resolver System](#resolver-system)
5. [Sources System](#sources-system)
6. [Operations and CLI](#operations-and-cli)
7. [Utilities](#utilities)
8. [Data Flow](#data-flow)
9. [Package Build Strategy](#package-build-strategy)
10. [Target Model and Cross-Compilation Status](#target-model-and-cross-compilation-status)
11. [Generators and Host Tools](#generators-and-host-tools)
12. [CI and Canaries](#ci-and-canaries)

---

## High-Level Overview

Harbour is organized into six major modules:

| Module | Purpose |
|--------|---------|
| `core` | Fundamental data structures (manifests, packages, targets, surfaces) |
| `builder` | Compilation, linking, and toolchain abstraction |
| `resolver` | Dependency resolution using the PubGrub algorithm |
| `sources` | Package fetching from paths, git, and registries |
| `ops` | High-level operations (build, add, update) |
| `util` | Shared utilities (hashing, diagnostics, process execution) |

The CLI (`src/bin/harbour/`) acts as a thin adapter layer that parses arguments and delegates to operations.

---

## Core Concepts

### Manifest

The Manifest represents a `Harbour.toml` file and is the single source of truth for a package's configuration. It contains:

- **Package metadata**: name, version, description, license, authors
- **Targets**: buildable artifacts (executables, libraries)
- **Dependencies**: required packages with version constraints
- **Build configuration**: C++ standard, runtime selection, exceptions/RTTI toggles
- **Profiles**: debug and release build settings

### Workspace

The Workspace wraps the root package and provides centralized access to:

- Directory structure (`.harbour/target`, output directories)
- Profile selection (debug vs release)
- Path utilities (manifest path, lockfile path, target directory)
- Manifest file discovery

A Workspace is the entry point for all build operations.

### Target

A Target represents a single buildable artifact. Target kinds include:

| Kind | Description |
|------|-------------|
| `exe` | Executable binary |
| `staticlib` | Static library (.a, .lib) |
| `sharedlib` | Shared/dynamic library (.so, .dll, .dylib) |
| `headeronly` | Header-only library (no compilation) |

Each target specifies sources (via glob patterns), public headers, language (C or C++), build recipe (native, CMake, Meson, or custom), and its own dependencies with visibility control. A target can depend on a sibling target of the same package (`local = true`), and a target marked `host = true` is a program built for the build machine that a generator runs; see [Generators and Host Tools](#generators-and-host-tools).

### Dependency

Dependencies describe what a package needs. Supported source types:

- **Path**: Local filesystem reference
- **Git**: Remote repository with optional branch/tag/rev
- **Registry**: Centralized package index

Dependencies can be simple (version string only) or detailed (specifying source, optional status, and feature selection).

### Surface

The Surface is Harbour's core abstraction for C/C++ build contracts. It defines what a target exports publicly versus what it uses internally:

| Component | Purpose |
|-----------|---------|
| `CompileSurface` | Compile-time requirements (includes, defines, C++ standard) |
| `LinkSurface` | Link-time requirements (libraries, linker flags, frameworks) |
| `AbiToggles` | Platform-specific settings (PIC, visibility, CRT, stdlib) |
| `ConditionalSurface` | Platform-conditional patches |

**Key principle**: Public surfaces propagate to dependents; private surfaces do not. This separation enables clean dependency management where header visibility and linking requirements have explicit control.

### Target Triple

`core::target::TargetTriple` is the single, canonical representation of a
build target, used everywhere a triple is needed: the build path, toolchain
selection, and the ABI identity below. It replaces three representations that
used to disagree with each other (a positionally-parsed struct that lived in
`core::abi`, an opaque string wrapper in `builder::shim::intent`, and a
hand-rolled `cfg` table for the host triple in `ops::verify::harness`).

Parsing recognizes components by set membership rather than by position
(following `llvm::Triple`), so irregular real-world triples parse correctly:
`arm-none-eabi` (three components, no OS), `avr` (one component), and
`msp430-elf` (`elf` denotes the object format, not an OS) all parse as
bare metal rather than misreading `eabi`/`elf` as the operating system.
Parsing is infallible — an unrecognized triple still produces a usable value,
flagged via `fully_recognized()` rather than rejected.

The type exposes two string forms: `as_str()` returns the triple exactly as
written (used for display and for invoking the toolchain), while
`canonical()` returns a normalized four-component form used as a cache/ABI
key, so that equivalent spellings (`arm-none-eabi` and
`arm-unknown-none-eabi`) collapse to one entry instead of two. Structured
predicates (`is_bare_metal()`, `is_embedded_rtos()`, `is_windows()`,
`is_apple()`, `env_is(...)`) replace the substring matching the old types
relied on.

### ABI Identity

ABI identity is a fingerprint ensuring binary compatibility. Two artifacts with matching ABI can be used interchangeably; different ABIs require recompilation.

The fingerprint captures:
- Target triple (via the canonical `TargetTriple` above)
- Compiler identity (family and version)
- Target kind and configuration (PIC, visibility, public defines)
- ABI toggles (C++ runtime, exception handling)

This fingerprint is wired into the build. `BuildContext::toolchain_fingerprint`
builds the `ToolchainFingerprint`, which keys both the incremental-build cache
and the probe cache; `NativeBuilder` computes a `CompileFingerprint` per source
file and a `LinkFingerprint` per archive or link step, and the link fingerprint
folds in the `AbiIdentity` of the target being linked. A change to the target,
compiler, kind or ABI-relevant settings therefore invalidates the objects and
artifacts that depend on it rather than reusing them. See
[Incremental Builds](#incremental-builds).

---

## Builder System

### Components

| Component | Role |
|-----------|------|
| `BuildContext` | Central container for build configuration (toolchain, profile, output directories) |
| `Toolchain` | Trait abstraction over compiler families (GCC, Clang, MSVC) |
| `NativeBuilder` | Executes build plans by invoking the native toolchain |
| `CMakeBuilder` | Adapts CMake-based projects into the build graph |
| `BuildPlan` | Ordered sequence of build steps (compile, archive, link) |
| `SurfaceResolver` | Propagates surfaces through the dependency graph |
| `Fingerprint` | Content-addressed incremental builds: toolchain, compile and link fingerprints, consulted by `NativeBuilder` on every build (see [Incremental Builds](#incremental-builds)) |
| `PrebuildStep` | A generator run while the plan is computed; skipped when its declared `inputs` are unchanged (see [Generators and Host Tools](#generators-and-host-tools)) |

### Toolchain Abstraction

The Toolchain trait provides compiler-agnostic command generation:

| Implementation | Platform |
|----------------|----------|
| `GccToolchain` | Unix-like systems (GCC, Clang, Apple Clang) |
| `MsvcToolchain` | Windows (cl.exe, lib.exe, link.exe) |

Toolchain detection happens automatically, respecting `CC`, `CXX`, and `AR` environment variables. `detect_toolchain(target)` takes the requested target: a native build detects the host compiler, and a cross build probes the candidate binaries for that triple and fails with the list it probed if none is found. See [Target Model and Cross-Compilation Status](#target-model-and-cross-compilation-status).

### Target Specs and Toolchain Candidates

`builder::toolchain::spec::TargetSpec` maps a `TargetTriple` to what building
for it needs: an ordered list of plausible compiler binary names
(`ToolchainCandidate`), compile flags derivable purely from the triple, and a
libc flavour where one is knowable. It exists because `<triple>-gcc` is the
wrong binary name for most non-trivial targets, not a rare exception — e.g.
every `thumbv*` Cortex-M triple is served by one `arm-none-eabi-gcc`, Debian
cross packages drop the vendor component (`aarch64-linux-gnu-gcc`), mingw-w64
naming shares nothing with the triple (`x86_64-w64-mingw32-gcc`), and Apple
and MSVC have no prefixed binary at all (resolved via `xcrun` and `vswhere`
respectively). `toolchain_candidates()` generates candidates most-specific
first — a built-in table of researched family special cases, then
progressively more generic conventions — and never returns an error; an
unrecognized triple still yields a best-effort candidate list.

This module computes candidates and flags only; it does not perform `PATH`
discovery itself. `detect_toolchain` calls it for any non-host target and does
the probing, which is where the "probed: ..." list in a missing-toolchain error
comes from. The `vswhere` strategy is listed but not implemented, so MSVC has to
already be on `PATH`.

### Build Recipes

| Recipe | Behavior |
|--------|----------|
| `Native` | Standard compile → archive/link pipeline |
| `CMake` | Invokes external CMake for configuration and build |
| `Meson` | Invokes external Meson for configuration and build |
| `Custom` | Arbitrary shell commands |

### Incremental Builds

Fingerprinting operates at three levels:

1. **Toolchain fingerprint**: Compiler identity, version, build settings. Changes invalidate everything.
2. **Compile fingerprint**: Source content, compiler flags, header dependencies. Per-file granularity.
3. **Link fingerprint**: Object files, dependent libraries, linker flags. Per-target granularity.

All three levels are in use. Before compiling, `NativeBuilder` computes each
source's fingerprint and skips it when it matches the cached one and the object
still exists, so a second build with nothing changed reports "All N file(s) up to
date" and editing one source recompiles only that file. Header dependencies are
found by a textual scan of `#include` directives, not by asking the compiler, so
the scan is cheap and slightly conservative.

Three refinements matter for large packages:

- **Per-process memo.** Hashing and scanning the same few hundred headers once
  per translation unit dominated an unchanged rebuild of a large package, so each
  file's hash and parsed includes are remembered for the life of the process, and
  a compile step already verified in this process is not checked again. Entries
  are keyed on modification time and length and are never kept for a file
  modified in the last two seconds (a second edit of the same length inside one
  timestamp tick would be invisible); a generator or a probe rewriting a header
  drops the lot.
- **Generators.** A `prebuild` step that declares `inputs` is skipped when a
  digest of its command, environment, input contents and output contents is
  unchanged; one that does not runs on every build. See
  [Generators and Host Tools](#generators-and-host-tools).
- **What is still not fingerprinted.** A CMake or Meson recipe step rebuilds in
  full on every build, because its inputs are not modelled (see
  [Package Build Strategy](#package-build-strategy)).

### Build Process Flow

1. **Context Setup**: Detect the toolchain for the requested target (the host's by default) and the target platform
2. **Dependency Resolution**: Obtain topologically-ordered resolve graph
3. **Surface Computation**: Propagate public surfaces, respecting visibility rules
4. **Plan Generation**: Create ordered build steps based on target kinds and recipes
5. **Execution**: Run compilation in parallel (via rayon), then sequential archive/link
6. **Output**: Collect artifacts, optionally generate interop files (pkg-config, CMake config)

---

## Resolver System

### PubGrub-Based Resolution

Harbour uses the PubGrub algorithm for dependency resolution, providing:

- Deterministic, reproducible resolution
- Clear conflict explanations
- Efficient backtracking

All I/O happens before resolution begins, making the resolver pure.

### The Resolve Graph

The `Resolve` struct represents the immutable dependency graph after resolution:

- Stored as a directed acyclic graph (petgraph)
- Nodes are PackageIds, edges are dependency relationships
- Supports multiple packages with the same name from different sources
- Provides topological ordering for build sequencing

### Lockfile

The lockfile (`Harbour.lock`) captures the complete resolved state:

- Package versions and checksums
- Registry provenance (git commits or tarball URLs)
- Manifest content hash for freshness detection
- For fetched (git and registry) packages, the source files each target compiles, so a changed file set shows up as a lock diff and a warning (`--locked`: an error); see MANIFEST.md "Source Lists in the Lockfile"

Content-based freshness detection compares manifest hashes rather than timestamps, providing resilience against clock skew and git operations.

### C++ Constraints

The `CppConstraints` module validates C++ configuration across the dependency graph:

1. Collect minimum required C++ standards from all targets
2. Determine requested standard (CLI arg → workspace config → default)
3. Validate effective standard meets requirements
4. Extract workspace-wide settings (exceptions, RTTI, MSVC runtime)

---

## Sources System

### Source Types

| Source | Description |
|--------|-------------|
| `PathSource` | Local filesystem dependencies |
| `GitSource` | Remote git repositories with branch/tag/rev support |
| `RegistrySource` | Centralized package indices |

All sources implement a common `Source` trait providing: query, load_package, ensure_ready, get_package_path, and is_cached operations.

### Registry Architecture

Registries are git repositories acting as package indices, in two tiers:

```
registry/
├── config.toml                    # Registry metadata (layout = "letter/name/version")
├── index/
│   └── <letter>/<name>.idx        # Tier 1: one JSON record per line, one line per version
└── <letter>/<package>/<version>.toml   # Tier 2: the shim
```

**Tier 1** is machine-generated and append-only. Each record carries the
version, a `yanked` flag (yanked versions are hidden from new resolutions but stay
fetchable, so existing lockfiles keep working), the version's dependencies
(requirement, `optional`, `default_features`, and the registry it resolves against
if not this one), a checksum, and the path of its shim. Dependencies live here so
resolution can walk a package's dependencies without fetching a shim for every
candidate.

**Tier 2, the shim**, is hand-authored and reviewed as a pull request. It contains:
- Package metadata (name, version)
- Source specification (git or tarball URL)
- Optional patches and surface overrides

Only the git transport is implemented. A sparse HTTP transport
(`sparse+https://`) is designed (`docs/superpowers/specs/2026-09-03-http-registry-design.md`)
behind the `RegistryTransport` trait but does not exist yet.

### Private remotes

Registries and git sources are fetched by running the system `git` (`src/sources/git_cli.rs`), not by libgit2, so a private repository works with whatever `git` already does for it: credential helpers (`gh auth setup-git`, the macOS keychain, git-credential-manager), `~/.ssh/config` aliases and `ProxyJump`, `GIT_SSH_COMMAND`, the ssh-agent, hardware keys, `includeIf`, and `url.<base>.insteadOf`. If `git ls-remote <url>` works in your terminal, Harbour can use `<url>` as a registry or git dependency. A registry in `~/.harbour/config.toml`, and a `git` dependency in `Harbour.toml`, can be written as `https://host/path`, `ssh://[user@]host/path`, `file:///path`, or the scp form `[user@]host:path` that GitHub's Clone button shows (`git@github.com:owner/repo.git`). The scp form reaches `git` exactly as written, so `host:repo` stays relative to the login's home and `host:/srv/repo` stays absolute, a segment like `host:2222/repo` is a path and not a port, and `host:~alice/repo` is `alice`'s home; `host` may be a `Host` alias from your ssh config. Harbour identifies the source by the equivalent `ssh://` URL (the cache directory is the same as for the `https://` spelling of the repository). A string that is none of these is an error naming the registry and listing the forms, never a silently skipped registry.

Harbour sets only `GIT_TERMINAL_PROMPT=0` (a remote with no stored login is an error, never a hang) and removes the variables a surrounding `git` hook exports (`GIT_DIR` and friends). A `git` command that runs longer than 15 minutes is stopped (`HARBOUR_GIT_TIMEOUT=<seconds>`, `0` for no limit). Failures keep what `git` said, with logins removed, and add a `hint:` for the common causes (no stored login, a rejected key, an untrusted host key, a repository that is not there or not visible to the login).

The registry index is fetched with `--depth 1` (only its current tree is read), and a registry shim's pinned git commit is fetched alone, falling back to a full fetch when the server will not serve that commit. A git dependency in `Harbour.toml` is cloned in full, since a branch, tag or rev anywhere in its history may be asked for. libgit2 still does the local work after a fetch: reading the repository and writing the working tree (byte-exact for registry sources).

Without `git` on `PATH` Harbour falls back to libgit2 and warns once: public remotes work, private ones do not (libgit2 reads none of the above). `HARBOUR_GIT_BACKEND=libgit2` forces the fallback.

A login written into a URL (`https://TOKEN@host/repo`) works, but `Harbour.toml` keeps it as written and Harbour warns: prefer a credential helper. Harbour never prints it: logs, errors and `Harbour.lock` show the URL without the login.

This design decouples discovery (centralized) from hosting (distributed).

### Source Resolution Flow

1. Read the package's tier-1 index and choose a version (yanked versions are not offered to a new resolution)
2. Load the shim for the chosen version
3. Extract source location (git repo or tarball)
4. Fetch source to local cache (hash-based path)
5. Apply any patches
6. Load package manifest

### SourceCache

The `SourceCache` coordinates all sources:

- Maintains cache directory for downloaded packages
- Lazy source instantiation
- Uses interned SourceIds for efficient comparison

---

## Operations and CLI

### CLI Commands

| Command | Purpose |
|---------|---------|
| `new` | Create new project in fresh directory |
| `init` | Initialize project in existing directory |
| `build` | Compile the workspace |
| `test` | Build and run test targets |
| `add` | Add dependency to manifest |
| `remove` | Remove dependency from manifest |
| `update` | Re-resolve dependencies and update lockfile |
| `clean` | Remove build artifacts |
| `tree` | Display dependency graph |
| `flags` | Show compile/link flags for a target |
| `explain` | Show why a package is in the graph |
| `linkplan` | Display link order for a target |
| `toolchain` | Show/configure toolchain |
| `backend` | List build backends and show their capabilities |
| `ffi` | FFI bundling (`ffi bundle`) |
| `doctor` | Check environment and toolchain health |
| `verify` | Verify a package builds (CI-grade validation) |
| `search` | Search a local registry's index by package name (scans the older per-package directory layout, not the `.idx` files) |
| `registry` | Manage registries (`index`, `list`) |
| `cache` | Manage the cache (`list`, `clean`, `size`, `path`) |
| `completions` | Generate shell completions |
| `alias` | Create/remove the `harbor` spelling as a symlink to the `harbour` binary |

### Operations Layer

Operations (`src/ops/`) contain business logic:

| Operation | Responsibility |
|-----------|----------------|
| `harbour_build` | Build orchestration, artifact collection |
| `harbour_add` | Manifest manipulation for dependencies |
| `harbour_new` | Project scaffolding from templates |
| `harbour_update` | Force re-resolution and lockfile update |
| `resolve` | Coordinate resolution with freshness checking |
| `lockfile` | Lockfile I/O and manifest hashing |

### Command Flow Pattern

1. Parse CLI arguments (clap)
2. Create GlobalContext and Workspace
3. Initialize SourceCache
4. Call corresponding operation
5. Format and display output

---

## Utilities

### GlobalContext

Centralized configuration providing:

- Path resolution (cache, target, config directories)
- Registry URL configuration
- Manifest file discovery (walks directory tree upward)
- Verbose and color output settings

### Diagnostics

User-friendly error messaging with:

- Severity levels (Error, Warning, Note, Help)
- Actionable suggestions for common errors
- File location context
- ANSI color formatting

### Filesystem

Cross-platform operations:

- Recursive directory copy/remove
- Glob pattern matching
- Path normalization
- Platform-aware symlink creation

### Hashing

Cryptographic utilities:

- SHA256 for files and strings
- Fingerprint builder for combining multiple components
- Short fingerprint output for display

### Interning

Memory-efficient string handling:

- Global string pool with O(1) equality comparison
- Zero-cost cloning
- Used extensively for package names and identifiers

### Process

Subprocess execution:

- Builder pattern for command composition
- Tool discovery (compilers, archivers, CMake)
- Respects environment variables (CC, CXX, AR)

---

## Data Flow

### Build Command Flow

```
CLI (build command)
    │
    ▼
Workspace (load manifest, select profile)
    │
    ▼
SourceCache (initialize sources)
    │
    ▼
Resolver (resolve dependencies → Resolve graph)
    │
    ▼
CppConstraints (validate C++ requirements)
    │
    ▼
SurfaceResolver (propagate compile/link surfaces)
    │
    ▼
BuildPlan (generate ordered build steps)
    │
    ▼
BuildContext + Toolchain (detect compiler, generate commands)
    │
    ▼
NativeBuilder (execute compile/link steps)
    │
    ▼
Artifacts (output paths, optional interop files)
```

### Dependency Resolution Flow

```
Manifest (dependencies section)
    │
    ▼
SourceCache (query available versions)
    │
    ▼
HarbourResolver (PubGrub algorithm)
    │
    ▼
Resolve (immutable dependency graph)
    │
    ▼
Lockfile (serialize to Harbour.lock)
```

### Package Loading Flow

```
Dependency specification
    │
    ▼
SourceId (identify source type and location)
    │
    ▼
Source.query() (validate and match versions)
    │
    ▼
Source.load_package() (fetch manifest and metadata)
    │
    ▼
Package (name, version, manifest, source)
```

---

## Target Model and Cross-Compilation Status

Cross-compilation works. `--target-triple` (or `toolchain.target` in the project or
global config) selects a toolchain and threads the target through to compiler and
linker flags, output location, and the fingerprints. An earlier version of this
section recorded that the requested triple was discarded before it reached the
builder; that was true when it was written and has since been fixed, and the
statements below were each checked by running a build.

### What a cross build does

- **Output is separate.** A cross build writes to `.harbour/target/<triple>/<profile>/`,
  so host and cross artifacts cannot contaminate each other. A static library built
  with `--target-triple x86_64-apple-darwin` on an arm64 host is an x86_64 archive
  under that directory.
- **The toolchain is chosen for the target.** `BuildContext::new` resolves the
  effective target once and passes it to `detect_toolchain`. For a non-host target
  that probes, in order, the candidates `toolchain_candidates()` generates for the
  triple (PATH-prefixed cross compilers, Xcode's `clang` for Apple targets, the host
  `clang` with `-target`), and fails with the list it probed if none is usable. A
  cross target never falls back to the host compiler.
- **`toolchain.target` is read.** `harbour toolchain override --target <triple>`
  makes a plain `harbour build` cross-compile, and `harbour toolchain show` reports
  that the target came from config.
- **Parsing is recognition-based and infallible.** Bare-metal and other irregular
  triples (`arm-none-eabi`, `avr`, `msp430-elf`, `xtensa-esp32-elf`) parse correctly
  instead of misreading an ABI or object-format token as the operating system.
- **FFI bundling** (`ops/ffi_bundle.rs`) decides platform-specific behavior from the
  *target* triple passed in, not from the host.
- **Generators and host tools** run on the build machine whatever the target is. A
  generator is told the target (`HARBOUR_TARGET_*`) and the host (`HARBOUR_HOST_*`),
  and a `host = true` target is built with the host toolchain. See
  [Generators and Host Tools](#generators-and-host-tools).
- `harbour alias` creates the `harbor` spelling as a filesystem symlink (a `.cmd`
  shim on Windows), rather than compiling a second copy of the binary.

### What is still limited

- **MSVC discovery via `vswhere` is not implemented.** The strategy is listed in
  the candidate table and reported as "not yet supported"; `cl.exe` has to already
  be on `PATH`.
- **Feature sets are not separated between the host and target builds.** A host
  tool and the program see one feature set. This only matters if two sides truly
  want different features.
- **A cross build shares nothing with the host build of the same package.** Host
  tools get a directory of their own and their libraries are compiled separately,
  correctly, since they are a different build.

---

## Package Build Strategy

Harbour can build a package three ways, and they are **deliberately ranked**, not
equivalent alternatives.

### 1. Native shim (canonical)

A `Harbour.toml` describing sources, defines and surface. This is the registry's
real format and the only tier where Harbour's distinguishing features apply:

- Surface contracts -- controlled propagation of public and private include
  directories, defines and flags to dependents
- Content-addressed incremental builds at file granularity, including header
  dependency tracking
- Uniform target flags, cross-compilation, and per-target output separation
- A single ABI identity across the graph
- `compile_commands.json`

The cost is per-package authoring work, and measurement suggests that cost is
low for most libraries: the committed zlib shim is 34 non-comment lines (its 15
sources named individually, plus per-OS defines), cJSON's is 17. Most C libraries
are "compile these `.c` files with these defines." The hard end is real: curl,
openssl, LuaJIT and CPython are shimmed through generators, probes and host tools
(see [Generators and Host Tools](#generators-and-host-tools)), and each is a
canary that builds and runs the result.

### 2. vcpkg (the hard tail)

For packages whose build genuinely resists shimming -- extensive generated code,
large conditional matrices, dependence on their own build system's introspection
-- prefer consuming vcpkg's artifacts over reimplementing the build. vcpkg has
already absorbed that per-package mess; duplicating it here is not a good trade.

### 3. CMake / Meson recipes (escape hatch, second class)

Supported, but explicitly not a growth area, because **delegating the build
forfeits every item in tier 1's list**. Concretely, a CMake or Meson dependency:

- is never fingerprinted, so it **rebuilds in full on every build** -- the
  incremental builder excludes recipe steps because their inputs are not modelled
- cannot receive surface flags, since Harbour does not emit its compile commands
- has cross-compilation behaviour determined by that backend, not by Harbour
- contributes nothing to `compile_commands.json`

A recipe is declared on the target (`[targets.NAME.recipe]`), so a non-native
package is identifiable from its manifest rather than silently degraded. (An
earlier design recorded the backend in a `build.backend` field; that key no longer
exists, and `[targets.NAME.backend]` is rejected. See `MANIFEST.md`.)

**Why this ranking rather than broad build-system support:** Harbour's
differentiation is Cargo-like developer experience, which requires owning
compilation. Tools that wrap arbitrary upstream build systems (Conan, vcpkg)
are a different and legitimate product, already well served. Attempting both
yields neither, and tier 2 removes most of the pressure to expand tier 3.

## Generators and Host Tools

Some packages produce their own sources by running a program: openssl runs perl
scripts, LuaJIT compiles `buildvm` and runs it, CPython freezes its startup modules
with a two-stage bootstrap of host programs. The manifest's reference documentation
for all of this is in `MANIFEST.md`; this is how the pieces fit.

**Generators run while the plan is computed.** A `[[targets.X.prebuild]]` step runs
during planning, because a target's sources are found by expanding globs and a
generator's output cannot be globbed before it exists. `PrebuildStep` therefore
carries work that has already happened by the time a plan reaches the executor,
and the plan keeps it as a record. A generator is told its target and host in
`HARBOUR_TARGET_*` and `HARBOUR_HOST_*`, the active features in `HARBOUR_FEATURES`,
the answers to any probes in `HARBOUR_PROBE_*`, and the host compiler in
`HARBOUR_HOST_CC`/`_CXX`/`_AR`.

**A generator that declares `inputs` is skipped when nothing changed.** Its digest
covers the command, its whole environment, the contents of every input, the
contents of every output and of any host tool it ran. Without `inputs` it runs on
every build, as before.

**Targets can depend on siblings.** `[targets.X.deps] core = { local = true }`
names a target of the same package. Targets are built in dependency order, and
`package_output_dir` and `build_roots` are the single places that decide where a
package's artifacts live and which packages a build is rooted at, shared by the
plan, `harbour flags` and `harbour linkplan`.

**Host targets are programs built for the build machine.** A target with
`host = true` is never part of the package's own build. A `prebuild` step that lists
it under `tools` gets it built first, by a nested build rooted at the package in a
*host context* (`BuildContext::for_host_tools`): the host toolchain and platform,
with no target flags. Inside a host tool, `os`/`arch`/`env` conditions describe the
host and `target_os`/`target_arch`/`target_env` describe the overall build's
platform, so a tool can be compiled for the host and configured for the target
(LuaJIT's `buildvm`) or configured for the host (CPython's tools).

**On a native build the tool and the program share a build.** The nested build uses
the outer build's artifact layout, so a library both link is compiled once; a cross
build gives its tools a directory of their own, since they are a different build.
The design and its phases are in `docs/superpowers/specs/2026-10-24-host-tools-design.md`;
phases 1 to 3 are implemented and phase 4, separate feature sets for the host and
target builds, is not.

**Probes** (`[targets.X.probes]`) answer configure-style questions by compiling for
the real target and never by running anything. There are five kinds (`header`,
`symbol`, `type`, `constant`, `sizeof`). Their answers become defines or a generated
header, and reach generators as `HARBOUR_PROBE_*`.

---

## CI and Canaries

`.github/workflows/ci.yml` runs five jobs: `Format Check` (which also decides whether
a change touches buildable code), `Lint Check`, `Tests` (a matrix over Ubuntu and
Windows), `Canaries (real packages)`, and `CI result`. A docs-only change skips the
test, lint and canary jobs with a job-level `if`, and `CI result` always runs and
fails only if a job it depends on failed or was cancelled.

That last job exists because of how branch protection interacts with a skipped
matrix: GitHub reports a skipped matrix job as plain `Tests`, so requiring
`Tests (ubuntu-latest)` by name left docs-only pull requests waiting forever.
`main` is protected as of this writing (a repository setting, not something this
repo records) by requiring `Format Check` and `CI result` with branches up to date,
no force-pushes or deletions, and no required reviews; administrators are not
enforced.

The canaries (`ci/canary/`) each build a real upstream package with Harbour and
then run something against it, asserting on the program's output and on object and
symbol counts rather than on exit status. They are `cjson`, `zlib`, `curl-config`,
`libuv`, `zstd`, `luajit`, `openssl`, `curl` and `cpython`; `ci/canary/README.md`
says what each one covers. The job runs on a pinned `ubuntu-26.04` image, because
`curl-config` compares Harbour's probe answers with curl's own for that distro's
glibc and a floating image turned a runner upgrade into an unrelated red build.
`cargo test` also has a set of `#[ignore]`d tests that drive a real compiler; CI
runs them with `--ignored` on Linux, so run both locally.

---

## Key Design Principles

1. **Surface-Based Contracts**: Explicit control over what propagates between dependencies
2. **Content-Based Freshness**: Hash manifests rather than rely on timestamps
3. **Toolchain Abstraction**: Platform-independent build logic with trait-based command generation
4. **Incremental Builds**: Multi-level fingerprinting for minimal rebuilds
5. **Interned Identifiers**: O(1) comparison for frequently-used strings
6. **Lazy Initialization**: Sources and caches created on-demand
7. **Pure Resolution**: All I/O before resolution, making the algorithm deterministic
