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

### ABI Identity

ABI identity is a fingerprint ensuring binary compatibility. Two artifacts with matching ABI can be used interchangeably; different ABIs require recompilation.

The fingerprint captures:
- Target triple (architecture-vendor-os-environment)
- Compiler identity (family and version)
- Target kind and configuration (PIC, visibility, public defines)
- ABI toggles (C++ runtime, exception handling)

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
| `Fingerprint` | Enables incremental builds by tracking input changes |

### Toolchain Abstraction

The Toolchain trait provides compiler-agnostic command generation:

| Implementation | Platform |
|----------------|----------|
| `GccToolchain` | Unix-like systems (GCC, Clang, Apple Clang) |
| `MsvcToolchain` | Windows (cl.exe, lib.exe, link.exe) |

Toolchain detection happens automatically, respecting `CC`, `CXX`, and `AR` environment variables.

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

Fingerprints are stored as JSON and compared on each build to skip unchanged work.

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

## Key Design Principles

1. **Surface-Based Contracts**: Explicit control over what propagates between dependencies
2. **Content-Based Freshness**: Hash manifests rather than rely on timestamps
3. **Toolchain Abstraction**: Platform-independent build logic with trait-based command generation
4. **Incremental Builds**: Multi-level fingerprinting for minimal rebuilds
5. **Interned Identifiers**: O(1) comparison for frequently-used strings
6. **Lazy Initialization**: Sources and caches created on-demand
7. **Pure Resolution**: All I/O before resolution, making the algorithm deterministic
