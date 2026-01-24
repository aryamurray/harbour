# Harbour

A Cargo-like package manager and build system for C.

Harbour brings modern dependency management to C projects with a familiar workflow inspired by Rust's Cargo.

## Features

- **Simple manifest format** - `Harbor.toml` defines your project and dependencies
- **Deterministic builds** - Lockfile ensures reproducible builds across machines
- **Git dependencies** - Pull dependencies directly from git repositories
- **Path dependencies** - Use local packages during development
- **Parallel builds** - Compile sources in parallel for faster builds
- **Surface contracts** - Fine-grained control over what headers and flags propagate to dependents

## Installation

```bash
cargo install --path .
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
├── Harbor.toml      # Project manifest
├── src/
│   └── main.c       # Source files
└── include/         # Public headers (for libraries)
```

### Harbor.toml

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

Example test target in Harbor.toml:

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

# Cross-compilation (requires cmake/meson backend)
harbour build --backend=cmake --target-triple=x86_64-unknown-linux-gnu
```

## Configuration Files

Harbour supports configuration files for persistent settings:

- **Global config**: `~/.harbour/config.toml` - User-wide defaults
- **Project config**: `.harbour/config.toml` - Project-specific overrides

Project config takes precedence over global config. CLI flags override both.

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

- **No registry support** - Dependencies must use `--git` or `--path`. Central package registry is planned for a future release.
- **Single package workspaces** - Multi-package workspaces not yet supported.

## Troubleshooting

### "could not find Harbor.toml"

You're not in a Harbour project directory. Run `harbour init` to create one, or `cd` to your project root.

### "target not found"

Run `harbour tree` to see available targets, or check your `Harbor.toml` for typos.

### "package not found in dependency graph"

The package isn't a dependency. Run `harbour tree` to see all dependencies, or `harbour add` to add it.

## License

MIT
