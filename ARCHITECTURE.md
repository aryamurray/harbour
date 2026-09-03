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
9. [Target Model and Cross-Compilation Status](#target-model-and-cross-compilation-status)

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

Each target specifies sources (via glob patterns), public headers, language (C or C++), build recipe (native, CMake, or custom), and its own dependencies with visibility control.

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

**This fingerprint is not currently wired into the build.** `ToolchainFingerprint`,
`CompileFingerprint`, `LinkFingerprint`, and `AbiIdentity::fingerprint` have no
call sites outside `builder/fingerprint.rs` and `core/abi.rs` themselves — the
build plan, native builder, and executor never construct or consult one. The
types and their tests are real, but nothing in the build currently invalidates
a cache entry because an ABI changed.

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
| `Fingerprint` | Designed to enable incremental builds by tracking input changes; not yet called from the build path (see [Incremental Builds](#incremental-builds)) |

### Toolchain Abstraction

The Toolchain trait provides compiler-agnostic command generation:

| Implementation | Platform |
|----------------|----------|
| `GccToolchain` | Unix-like systems (GCC, Clang, Apple Clang) |
| `MsvcToolchain` | Windows (cl.exe, lib.exe, link.exe) |

Toolchain detection happens automatically, respecting `CC`, `CXX`, and `AR` environment variables. `detect_toolchain()` currently takes no target argument and always detects the host compiler; see [Target Model and Cross-Compilation Status](#target-model-and-cross-compilation-status).

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
discovery itself, and it is not yet called from `detect_toolchain()` or the
build path (see below).

### Build Recipes

| Recipe | Behavior |
|--------|----------|
| `Native` | Standard compile → archive/link pipeline |
| `CMake` | Invokes external CMake for configuration and build |
| `Custom` | Arbitrary shell commands |

### Incremental Builds

Fingerprinting operates at three levels:

1. **Toolchain fingerprint**: Compiler identity, version, build settings. Changes invalidate everything.
2. **Compile fingerprint**: Source content, compiler flags, header dependencies. Per-file granularity.
3. **Link fingerprint**: Object files, dependent libraries, linker flags. Per-target granularity.

Fingerprint types exist for all three levels and are unit-tested, but as
noted under [ABI Identity](#abi-identity) they have no call sites in the
actual build path today — `plan.rs`, `native.rs`, and `executor.rs` never
construct or compare one, and there is no separate mtime-based skip check
either. In practice every `harbour build` currently recompiles; "incremental
builds" describes designed-but-unwired machinery, not current behavior.

### Build Process Flow

1. **Context Setup**: Detect host toolchain and target platform
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

Registries are git repositories acting as package indices. Structure:

```
registry/
├── config.toml          # Registry metadata
└── <letter>/
    └── <package>/
        └── <version>.toml   # Shim file
```

**Shim files** contain:
- Package metadata (name, version)
- Source specification (git or tarball URL)
- Optional patches and surface overrides

This design decouples discovery (centralized) from hosting (distributed).

### Source Resolution Flow

1. Load shim file for requested package version
2. Extract source location (git repo or tarball)
3. Fetch source to local cache (hash-based path)
4. Apply any patches
5. Load package manifest

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

Harbour accepts a `--target-triple` flag on `build`, but **cross-compilation
does not work yet.** This section describes what the target-model unification
actually shipped versus what it left unwired, so the gap is explicit rather
than discovered by trying it.

### What is unified and working

- One canonical triple type, `core::target::TargetTriple` (see
  [Target Triple](#target-triple)), used consistently by the build path and
  the ABI identity. The three previous representations are gone.
- Parsing is recognition-based and infallible, so bare-metal and other
  irregular triples (`arm-none-eabi`, `avr`, `msp430-elf`, `xtensa-esp32-elf`)
  parse correctly instead of misreading an ABI or object-format token as the
  operating system.
- `builder::toolchain::spec::TargetSpec` and `toolchain_candidates()` (see
  [Target Specs and Toolchain Candidates](#target-specs-and-toolchain-candidates))
  compute, for a given triple, an ordered list of plausible compiler binaries
  and the flags derivable from the triple alone.
- FFI bundling (`ops/ffi_bundle.rs`) decides platform-specific behavior
  (shared library extension, RPATH-rewrite strategy, runtime-dependency
  collection tooling) from the *target* triple passed in, not from the host
  `cfg!`/`env::consts`. A bundle built for macOS from a Linux host now
  produces `.dylib`-flavored output instead of `.so`.
- `harbour alias` creates the `harbor` spelling as a filesystem symlink (a
  `.cmd` shim on Windows), rather than compiling and testing a second copy of
  the entire binary as a duplicate `[[bin]]` target.

### What is still disconnected

- **The requested target triple never reaches the builder.** In
  `ops/harbour_build.rs`, `--target-triple` is parsed, folded into a
  `BuildIntent`, used for exactly two checks (rejecting backends that lack
  cross-compile capability, and skipping harness *execution* in `verify`),
  and then discarded: `let _ = intent;`. Downstream of that line, the build
  plan, the native builder, and the executor never see the requested triple.
- **`detect_toolchain()` takes no target argument.** It always detects the
  host compiler. There is currently no code path that calls it with a target
  and no path that consults `TargetSpec`/`toolchain_candidates()` for an
  actual build — that layer is built and unit-tested, but not called from
  anywhere in the build pipeline yet.
- **`BuildContext::new` is host-only.** It unconditionally calls
  `detect_toolchain()` (no target) and sets `self.target = TargetTriple::host()`,
  regardless of what was requested on the command line.
- **Output paths do not vary by target.** The design called for
  `.harbour/target/<triple>/<profile>/`, mirroring Cargo, so that host and
  cross artifacts can't contaminate each other. That has not landed:
  `Workspace::output_dir()` is still `target_dir.join(profile)` with no
  triple component.
- **`toolchain.target` in project config is inert.** `harbour toolchain
  override --target <triple>` writes and echoes back a config value that
  `try_detect_from_config` never reads.

Net effect: passing `--target-triple` today changes log output and can reject
a backend for lacking cross-compile capability, but it does not change a
single compiler flag, select a different compiler, or change where output
lands. Treat `--target-triple` as not yet functional.

---

## Key Design Principles

1. **Surface-Based Contracts**: Explicit control over what propagates between dependencies
2. **Content-Based Freshness**: Hash manifests rather than rely on timestamps
3. **Toolchain Abstraction**: Platform-independent build logic with trait-based command generation
4. **Incremental Builds**: Multi-level fingerprinting for minimal rebuilds
5. **Interned Identifiers**: O(1) comparison for frequently-used strings
6. **Lazy Initialization**: Sources and caches created on-demand
7. **Pure Resolution**: All I/O before resolution, making the algorithm deterministic
