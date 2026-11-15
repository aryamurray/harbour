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
- **Generated sources** - `prebuild` steps run generators, skipped when their declared `inputs` have not changed; a target can depend on a sibling target of the same package; a `host = true` program is built for the build machine and run by a generator (see `MANIFEST.md`)
- **Configure-style probes** - Ask the real compiler whether a header, function, type or size exists and generate a config header, instead of vendoring one per platform
- **Built against real packages** - zlib, libpng, curl, openssl, LuaJIT, FreeType with HarfBuzz, CPython and the C++ library {fmt} all build through Harbour; the canaries in `ci/canary/` build and run most of them in CI
- **Surface contracts** - Fine-grained control over what headers and flags propagate to dependents

## Installation

Harbour needs [`git`](https://git-scm.com/downloads) on your `PATH`: git dependencies, registries and registry patches all go through it (so private repositories use your own credentials and ssh configuration). `harbour doctor` checks for it. macOS gets it with `xcode-select --install`; on Windows, `winget install Git.Git`.

### macOS and Linux

```bash
curl -fsSL https://raw.githubusercontent.com/aryamurray/harbour/main/install.sh | sh
```

This downloads the latest release for your machine, checks it against the release's
`SHA256SUMS`, and installs `harbour` into `~/.harbour/bin`. It edits no shell startup
file; if that directory is not on your `PATH` it prints the line to add. Set
`HARBOUR_VERSION` to pin a version or `HARBOUR_INSTALL_DIR` to install elsewhere.

With Homebrew:

```bash
brew install aryamurray/harbour/harbour
```

The Linux binaries need glibc 2.35 or newer and OpenSSL 3 (`libssl.so.3`).

### Windows

Download `harbour-<version>-x86_64-pc-windows-msvc.zip` from the
[releases page](https://github.com/aryamurray/harbour/releases), unpack it, and put
`harbour.exe` on your `PATH`.

### From source

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
warnings = "all"          # -Wall -Wextra, or /W4 under MSVC

[dependencies]
# Registry dependency
zlib = "1"

# Git dependency (placeholder URL: use your own)
yourlib = { git = "https://github.com/you/yourlib", tag = "v1.0.0" }

# Local path dependency
myutil = { path = "../myutil" }
```

### Build your project

```bash
# Debug build
harbour build

# Release build
harbour build --release

# Dependencies fetched from a registry or git are compiled with the compiler's
# default warnings, whatever their own manifests ask for. This applies their
# own `warnings` settings instead (one dependency: `warnings = true` on its
# `[dependencies]` entry)
harbour build --dep-warnings

# Build and run the package's executable; arguments after `--` go to the program
harbour run -- --my-flag
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
| `harbour run [TARGET] [-- ARGS]` | Build and run an executable target |
| `harbour test` | Build and run test targets |
| `harbour search <query>` | Search the configured registries (or `--registry-path <dir>`) for package names containing the query; shows each package's latest available version |
| `harbour add <pkg> --git <url>` | Add a git dependency |
| `harbour add <pkg> --path <path>` | Add a local dependency |
| `harbour remove <pkg>` | Remove a dependency |
| `harbour update` | Update dependencies and lockfile |
| `harbour tree` | Show dependency tree |
| `harbour flags <target>` | Show compile/link flags with provenance (`-p <pkg>` for a workspace member's or a dependency's target) |
| `harbour linkplan <target>` | Show link order and sources |
| `harbour explain <pkg>` | Explain why a package is in the graph |
| `harbour clean` | Remove build artifacts |
| `harbour toolchain show` | Show compiler configuration |
| `harbour ffi bundle` | Create portable FFI bundle |
| `harbour doctor` | Check environment and toolchain health |
| `harbour verify <pkg>` | Verify a registry package the way a consumer uses it: fetch, patch, build, link and run a harness (`--format json` for CI) |
| `harbour registry index` | Regenerate a registry's package index |
| `harbour registry list` | List configured registries |
| `harbour cache list` | List cached indices, sources and artifacts |
| `harbour cache clean` | Clear the cache (`size`, `path` also available) |
| `harbour alias` | Create or remove the `harbor` spelling |
| `harbour completions <shell>` | Generate shell completions |

## Dependency Management

### Adding dependencies

```bash
# From the registry
harbour add zlib

# From a git repository (placeholder URL: use your own)
harbour add yourlib --git https://github.com/you/yourlib

# With a specific tag
harbour add yourlib --git https://github.com/you/yourlib --tag v1.3.1

# With a specific branch
harbour add yourlib --git https://github.com/you/yourlib --branch main

# With a specific commit
harbour add yourlib --git https://github.com/you/yourlib --rev abc123

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

[targets.mylib.public]
include_dirs = ["include"]        # Propagates to dependents
defines = ["MYLIB_API"]
system_libs = ["pthread"]

[targets.mylib.private]
include_dirs = ["src"]            # Internal only
defines = ["MYLIB_INTERNAL"]
```

The longer `[targets.mylib.surface.compile.public]` spelling of the same
tables is still accepted; see [MANIFEST.md](MANIFEST.md).

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

MSVC needs no Developer Command Prompt: if `cl.exe` is not already on `PATH` with its
environment set up, Harbour finds Visual Studio with `vswhere.exe`, reads its
default toolset, finds the newest Windows SDK, and computes `INCLUDE`, `LIB` and `PATH`
for the host/target architecture pair itself (no batch file is run) for every compile
and link. `harbour toolchain show` and `harbour doctor` say which way MSVC was found.
The architecture mapping, version selection and environment layout are unit-tested on
every platform; running it against a real Visual Studio is verified only by the Windows
CI job (native x64 builds). Cross forms such as x64 to arm64 are implemented but have
not been run in CI.

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
opt_level = "0"
debug = "2"

[profile.release]
opt_level = "3"
debug = "0"
lto = true
```

Every value is a string (`opt_level = 0` and `debug = true` are rejected at
parse time), and the accepted values are fixed: see
[`[profile.NAME]`](MANIFEST.md#profilename) for the full set and for how each
one is spelled on MSVC, which does not use GCC's flag syntax.

## How Targets Are Built

Harbour has one builder: its own compiler driver. It compiles each target's
sources, archives or links them, tracks what changed, and writes
`compile_commands.json`. There is nothing to select and no flag or config key
that selects it.

A package that cannot be described that way has one escape hatch, declared on
the target in its manifest: a **custom recipe**, a list of commands Harbour
runs in place of compiling the target itself. Those commands may run `cmake`,
`meson`, `make`, `./configure` or anything else.

```toml
[targets.mylib]
kind = "staticlib"

[targets.mylib.recipe]
type = "custom"

[[targets.mylib.recipe.steps]]
program = "make"
args = ["-j4"]
```

A target built by a custom recipe is opaque to Harbour: it is rebuilt in full
on every build, receives no surface flags, and is absent from
`compile_commands.json`. See [Build Recipe](MANIFEST.md#build-recipe) for what
the steps are given and a worked CMake example.

There is no CMake or Meson recipe type (`type = "cmake"` and `type = "meson"`
are rejected, naming the custom recipe to write instead), and there are no
build backends to choose between: `harbour backend`, `harbour build --backend`
and `[build] backend` in config have been removed. The last two are still
recognised only so that they can be refused with an explanation.

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
harbour build --linkage=auto     # No preference (default)

# Cross-compile for another target; output goes to .harbour/target/<triple>/
harbour build --target-triple x86_64-unknown-linux-gnu
```

## Configuration Files

Harbour supports configuration files for persistent settings:

- **Global config**: `~/.harbour/config.toml` - User-wide defaults. Older versions read it from the platform cache directory (`~/Library/Caches/com.harbour.harbour/` on macOS); a config found only there is still used, with a warning saying how to move it. If both exist, `~/.harbour/config.toml` wins. Harbour never moves or deletes the old file.
- **Project config**: `.harbour/config.toml` - Project-specific overrides

Project config takes precedence over global config. CLI flags override both.

Vcpkg integration is optional. Set `VCPKG_ROOT` (and optionally `VCPKG_DEFAULT_TRIPLET`) or configure the `[vcpkg]` section to inject vcpkg include/lib paths into native builds.

### Example Configuration

```toml
# ~/.harbour/config.toml or .harbour/config.toml

[build]
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

- **Registry support is experimental** - A git-backed registry works for dependency
  resolution, but only the git transport exists (the sparse HTTP transport is a design,
  not code). `harbour search` reads the configured registries' `index/<letter>/<name>.idx`
  files (`--offline` searches only registries already cloned in the cache) and matches
  package names only; the index carries no descriptions.
- **Workspace support has gaps** - Every command walks up to the workspace root, and the
  ones about a single package (`run`, `test`, `tree`, `flags`, `linkplan`, `explain`,
  `ffi bundle`, `add`, `remove`) use the member you are in or `-p`. What is not done:
  at a *virtual* root with no `-p` those commands silently use the first member (by
  path), and `add`/`remove` there write a `[dependencies]` table nothing reads;
  `harbour build` from inside a member builds every default member, not just that
  one; `--message-format json` labels every artifact with the first member's package;
  `new`/`init` never edit `[workspace] members`; and `clean` has no `-p`.
- **A dependent picks its libraries by name** - A package exposing several libraries
  (openssl ships libcrypto *and* libssl) contributes its default target to a dependent
  that names none, and Harbour warns. Name them with `targets = ["ssl", "crypto"]` under
  `[targets.X.deps]`; archives are ordered by the package's `local = true` edges between
  its own targets, so a library that needs another's symbols must say so there. Nothing
  infers a dependency between two libraries from their symbols.
- **Source patterns are plain globs** - `*`, `**`, `?` and `[...]` only; `{a,b}` brace patterns are rejected with
  an error rather than expanded, so list each alternative as its own entry.
- **MSVC cannot assemble** - `ml64.exe`/`armasm64.exe` are not driven, so a target with
  assembly sources is rejected on MSVC rather than silently mis-built. Use clang or gcc
  there, or rely on the target's portable C path.
- **The custom recipe is second class** - A target built by a custom recipe (the only
  non-native build path; CMake and Meson are driven through it) is rebuilt in full every time, is absent from `compile_commands.json`, and must
  copy its artifact to `$HARBOUR_ARTIFACT_DIR` to be linkable by dependents. Prefer a
  native shim listing sources and defines.
- **Probes measure, they do not run** - `[targets.NAME.probes]` answers `HAVE_*`-style
  questions by compiling for the real target (headers, functions, types, constants,
  sizes), so curl's `config.h` is generated rather than vendored. Checks that need to
  *execute* a program (configure's `AC_RUN_IFELSE`) cannot be probes and become
  literal assertions in the manifest; see `MANIFEST.md`.

## Troubleshooting

### "could not find Harbour.toml"

You're not in a Harbour project directory. Run `harbour init` to create one, or `cd` to your project root.

### "target not found"

Run `harbour tree` to see available targets, or check your `Harbour.toml` for typos.

### "package not found in dependency graph"

The package isn't a dependency. Run `harbour tree` to see all dependencies, or `harbour add` to add it.

## License

MIT — see [LICENSE](LICENSE).
