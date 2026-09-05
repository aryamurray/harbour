# Harbour

A Cargo-like package manager and build system for C and C++.

Harbour brings modern dependency management to C and C++ projects with a familiar
workflow inspired by Rust's Cargo.

## Features

- **Simple manifest format** - `Harbour.toml` defines your project and dependencies
- **Deterministic builds** - Lockfile ensures reproducible builds across machines
- **Incremental builds** - Content-addressed fingerprints, including header dependencies, so only what changed recompiles
- **Git and path dependencies** - Pull from git repositories, or use local packages during development
- **Cross-compilation** - `--target-triple` selects a toolchain and threads through to compiler and linker flags
- **Assembly alongside C** - `.S`, `.s` and `.asm` sources compile in the same target; `.S` runs through the preprocessor, so include dirs and defines apply
- **Features** - Optional functionality with union semantics across dependents, so a library is built once with a consistent configuration
- **Parallel builds** - Compile sources in parallel for faster builds
- **Surface contracts** - Fine-grained control over what headers and flags propagate to dependents

## Installation

```bash
cargo install --path .
```

Harbour also answers to the `harbor` spelling. That alias is a symlink (or, on
Windows, a small `.cmd` shim) rather than a second copy of the binary, so it
costs nothing to have:

```bash
harbour alias              # creates `harbor` next to the harbour executable
harbour alias --remove     # removes it
```

## Quick Start

### Create a new project

```bash
# Create a new executable project
harbour new myapp
cd myapp

# Or create a library
harbour new mylib --lib
```

### Project structure

```
myapp/
├── Harbour.toml     # Project manifest
├── src/
│   └── main.c       # Source files
└── include/         # Public headers (for libraries)
```

### Harbour.toml

```toml
[package]
name = "myapp"
version = "0.1.0"

[targets.myapp]
kind = "exe"
sources = ["src/**/*.c"]

[dependencies]
# Git dependency
zlib = { git = "https://github.com/example/zlib-harbour", tag = "v1.3.1" }

# Local path dependency
myutil = { path = "../myutil" }
```

### Build your project

```bash
# Debug build
harbour build

# Release build
harbour build --release
```

### Run tests

```bash
harbour test
```

Test targets are automatically discovered by name pattern: `*_test`, `*_tests`, `test_*`, `test`, `tests`.

Example test target in Harbour.toml:

```toml
[targets.unit_test]
kind = "exe"
sources = ["tests/**/*.c"]
```

## Commands

| Command | Description |
|---------|-------------|
| `harbour new <name>` | Create a new project |
| `harbour init` | Initialize project in current directory |
| `harbour build` | Build the project |
| `harbour test` | Build and run test targets |
| `harbour add <pkg> --git <url>` | Add a git dependency |
| `harbour add <pkg> --path <path>` | Add a local dependency |
| `harbour remove <pkg>` | Remove a dependency |
| `harbour update` | Update dependencies and lockfile |
| `harbour tree` | Show dependency tree |
| `harbour flags <target>` | Show compile/link flags with provenance |
| `harbour linkplan <target>` | Show link order and sources |
| `harbour explain <pkg>` | Explain why a package is in the graph |
| `harbour clean` | Remove build artifacts |
| `harbour toolchain show` | Show compiler configuration |
| `harbour backend list` | List available build backends |
| `harbour backend show <name>` | Show backend capabilities |
| `harbour ffi bundle` | Create portable FFI bundle |
| `harbour doctor` | Check environment and toolchain health |
| `harbour verify <pkg>` | Verify a package builds (CI-grade validation) |
| `harbour registry index` | Regenerate a registry's package index |
| `harbour registry list` | List configured registries |
| `harbour cache list` | List cached indices, sources and artifacts |
| `harbour cache clean` | Clear the cache (`size`, `path` also available) |
| `harbour alias` | Create or remove the `harbor` spelling |
| `harbour completions <shell>` | Generate shell completions |

## Dependency Management

### Adding dependencies

```bash
# From a git repository
harbour add zlib --git https://github.com/example/zlib-harbour

# With a specific tag
harbour add zlib --git https://github.com/example/zlib-harbour --tag v1.3.1

# With a specific branch
harbour add zlib --git https://github.com/example/zlib-harbour --branch main

# With a specific commit
harbour add zlib --git https://github.com/example/zlib-harbour --rev abc123

# From a local path
harbour add myutil --path ../myutil

# Vcpkg (auto-resolves when registry doesn't have the package)
harbour add glfw3
```

### Understanding the dependency graph

```bash
# Show the full dependency tree
harbour tree

# Explain why a package is included
harbour explain zlib
```

## Surface Contracts

Harbour uses "surfaces" to control what compile and link flags propagate between packages.

```toml
[targets.mylib]
kind = "staticlib"
sources = ["src/**/*.c"]

[targets.mylib.surface.compile.public]
include_dirs = ["include"]        # Propagates to dependents
defines = [{ name = "MYLIB_API" }]

[targets.mylib.surface.compile.private]
include_dirs = ["src"]            # Internal only
defines = [{ name = "MYLIB_INTERNAL" }]

[targets.mylib.surface.link.public]
libs = [{ kind = "system", name = "pthread" }]
```

### Viewing resolved flags

```bash
# See all flags with their source
harbour flags myapp

# Output:
# Compile flags for `myapp`:
#   -I/path/to/zlib/include    # from: zlib 1.3.1 (surface.compile.public)
#   -DZLIB_CONST               # from: zlib 1.3.1 (surface.compile.public)

# See link order
harbour linkplan myapp
```

## Cross-Compilation

```bash
harbour build --target-triple x86_64-unknown-linux-gnu
harbour build --target-triple aarch64-apple-darwin
```

Artifacts land under `.harbour/target/<triple>/`, so a host build and a cross build
coexist without invalidating each other. The triple is part of the compile
fingerprint, so switching targets does not reuse the other's objects.

Toolchains are found in a defined order per target: a PATH-prefixed cross compiler
(`aarch64-linux-gnu-gcc`, `arm-none-eabi-gcc`), Xcode's `clang` via `xcrun` for Apple
targets, or an explicit override. `harbour toolchain show` reports what was selected
and why it was chosen; if nothing is found, the error lists the binaries that were
probed.

MSVC is discovered via `vswhere` and that discovery is not implemented, so `cl.exe`
has to already be on `PATH`.

## Declaring Target Support

C has no equivalent of Rust's `core`/`std` split to lean on, so a manifest says what
it needs. The two declarations are enforced differently, because C guarantees
something at only one of these levels.

```toml
[package]
requires = "hosted"        # or "freestanding"
supports = ["*-*-linux-gnu", "*-apple-darwin", "x86_64-pc-windows-msvc"]
```

`requires` **fails the build** when unsatisfied. Freestanding versus hosted is the one
split the C standard defines (C §4): freestanding promises only `<float.h>`,
`<limits.h>`, `<stdarg.h>`, `<stddef.h>` and the C11 additions, while hosted adds the
rest of libc. A package needing libc on a bare-metal target is therefore definitely
broken, and the error names the package — including when it is a dependency several
levels down. Omitting the field means the package makes no claim and nothing is
enforced.

`supports` only **warns**. Above that line nothing is guaranteed — glibc, musl, MSVC
and newlib disagree on POSIX coverage, threads and sockets — so the list records
triples someone has built, not triples that can work. Patterns are globs over the
canonical triple.

## Build Profiles

```toml
[profile.debug]
opt_level = 0
debug = true

[profile.release]
opt_level = 3
debug = false
lto = true
```

## Build Backends

Harbour supports multiple build backends for different use cases:

```bash
# List available backends
harbour backend list

# Show backend capabilities
harbour backend show cmake

# Build with a specific backend
harbour build --backend=cmake
```

| Backend | Description |
|---------|-------------|
| `native` | Built-in compiler driver (default) |
| `cmake` | CMake-based builds for complex projects |
| `meson` | Meson build system support |
| `custom` | User-defined build commands |

## FFI Bundling

Create portable shared library bundles for FFI consumption by other languages:

```bash
# Build with FFI mode (shared libraries + runtime deps)
harbour build --ffi

# Create FFI bundle
harbour ffi bundle --output ./dist

# Preview what would be bundled
harbour ffi bundle --dry-run
```

The FFI bundle includes:
- Primary shared library
- Transitive runtime dependencies
- RPATH rewriting for portability (Linux/macOS)
- JSON manifest listing all bundled files

### Build Options

```bash
# Library linkage preference
harbour build --linkage=static   # Static libraries only
harbour build --linkage=shared   # Shared libraries only
harbour build --linkage=auto     # Backend decides (default)

# --target-triple is accepted but not yet wired into the build: it is
# validated against the backend's declared capabilities and logged, but it
# does not currently change compiler selection, flags, or output location.
# Cross-compilation is not functional yet.
```

## Configuration Files

Harbour supports configuration files for persistent settings:

- **Global config**: `~/.harbour/config.toml` - User-wide defaults
- **Project config**: `.harbour/config.toml` - Project-specific overrides

Project config takes precedence over global config. CLI flags override both.

Vcpkg integration is optional. Set `VCPKG_ROOT` (and optionally `VCPKG_DEFAULT_TRIPLET`) or configure the `[vcpkg]` section to inject vcpkg include/lib paths into native builds.

### Example Configuration

```toml
# ~/.harbour/config.toml or .harbour/config.toml

[build]
# Default build backend (native, cmake, meson, custom)
backend = "native"

# Default linkage preference (static, shared, auto)
linkage = "auto"

# Default parallel jobs (omit for auto-detect)
jobs = 8

# Always emit compile_commands.json
emit_compile_commands = true

# Default C++ standard
cpp_std = "17"

[ffi]
# Default FFI bundle output directory
bundle_dir = "./dist"

# Include transitive runtime dependencies
include_transitive = true

# Rewrite RPATH for portability
rpath_rewrite = true

[net]
# Offline mode (don't fetch from network)
offline = false

[vcpkg]
# Enable vcpkg integration (defaults to VCPKG_ROOT if set)
enabled = true

# Optional overrides for vcpkg
root = "C:/vcpkg"
triplet = "x64-windows"
```

## Shell Completions

Generate shell completions for tab-completion support:

```bash
# Bash (add to ~/.bashrc)
eval "$(harbour completions bash)"

# Zsh (add to ~/.zshrc)
eval "$(harbour completions zsh)"

# Fish (add to ~/.config/fish/config.fish)
harbour completions fish | source

# PowerShell (add to $PROFILE)
harbour completions powershell | Out-String | Invoke-Expression
```

## Current Limitations

- **Registry support is experimental** - `harbour search` is a stub and returns no
  results. A git-backed registry works for dependency resolution.
- **Workspace support is partial** - Multi-package workspaces resolve and build, but
  some commands still assume a single root.
- **One archive per dependency** - A package exposing several libraries (openssl ships
  libcrypto *and* libssl) contributes only one to each dependent. Harbour warns and
  names the alternatives; a multi-archive upstream has to expose a single covering
  target.
- **MSVC cannot assemble** - `ml64.exe`/`armasm64.exe` are not driven, so a target with
  assembly sources is rejected on MSVC rather than silently mis-built. Use clang or gcc
  there, or rely on the target's portable C path.
- **Non-native backends are second class** - A target built by CMake, Meson or a custom
  recipe is rebuilt in full every time, is absent from `compile_commands.json`, and must
  copy its artifact to `$HARBOUR_ARTIFACT_DIR` to be linkable by dependents. Prefer a
  native shim listing sources and defines.
- **No configure-style probes** - Harbour does not run `HAVE_*` feature checks, so a
  package whose build is configure-driven needs its generated `config.h` vendored per
  platform (see `include_dirs` in a `when` block, in MANIFEST.md).

## Troubleshooting

### "could not find Harbour.toml"

You're not in a Harbour project directory. Run `harbour init` to create one, or `cd` to your project root.

### "target not found"

Run `harbour tree` to see available targets, or check your `Harbour.toml` for typos.

### "package not found in dependency graph"

The package isn't a dependency. Run `harbour tree` to see all dependencies, or `harbour add` to add it.

## License

MIT

<!-- CI docs-only skip proof; this branch is thrown away. -->
