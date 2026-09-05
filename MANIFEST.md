# Harbour.toml Manifest Reference

This document describes the complete schema for `Harbour.toml` manifest files.

## Overview

The manifest file (`Harbour.toml` or `Harbor.toml`) is the central configuration file for a Harbour package. It defines package metadata, dependencies, build targets, and compilation settings.

## Sections

### [package]

Package metadata (required unless this is a virtual workspace).

```toml
[package]
name = "mylib"           # Required: Package name
version = "1.0.0"        # Required: Semver version
description = "..."      # Optional: Short description
license = "MIT"          # Optional: SPDX license identifier
authors = ["Name <email>"]  # Optional: List of authors
repository = "https://..." # Optional: Repository URL
homepage = "https://..."   # Optional: Homepage URL
documentation = "https://..." # Optional: Documentation URL
keywords = ["c", "library"]   # Optional: Discovery keywords
categories = ["development"]  # Optional: Categories
```

### [workspace]

Workspace configuration for multi-package projects.

```toml
[workspace]
members = ["packages/*"]           # Glob patterns for member directories
exclude = ["packages/experimental"] # Directories to exclude
default-members = ["packages/core"] # Default packages to build (optional)

[workspace.dependencies]           # Shared dependencies for inheritance
zlib = { git = "https://github.com/madler/zlib", tag = "v1.3.1" }
```

Members can inherit workspace dependencies with `workspace = true`:

```toml
# In member's Harbour.toml
[dependencies]
zlib = { workspace = true }
```

### [build]

Workspace-level build configuration.

```toml
[build]
cpp_std = "17"           # Default C++ standard (11, 14, 17, 20, 23)
cpp_runtime = "libstdc++" # C++ runtime: libstdc++ or libc++
msvc_runtime = "dynamic"  # MSVC runtime: dynamic or static
exceptions = true         # Enable C++ exceptions (default: true)
rtti = true              # Enable C++ RTTI (default: true)
```

### [dependencies]

Package dependencies.

```toml
[dependencies]
# Path dependency (local)
mylib = { path = "../mylib" }

# Git dependency
zlib = { git = "https://github.com/madler/zlib", tag = "v1.3.1" }
zlib = { git = "...", branch = "main" }
zlib = { git = "...", rev = "abc123" }

# Registry dependency (when registries are configured)
openssl = "1.1.1"
openssl = { version = "1.1.1", registry = "custom" }

# Vcpkg dependency (auto-resolved when not in registry)
glfw3 = { vcpkg = true }
# Optional overrides
glfw3 = { vcpkg = true, triplet = "x64-windows", libs = ["glfw"] }

# Workspace inheritance
shared = { workspace = true }
```

### [targets.NAME]

Build targets. If no targets are defined, a default target is created from the package name.

```toml
[targets.mylib]
kind = "staticlib"        # Required: exe, staticlib, sharedlib, header-only
sources = ["src/**/*.c"]  # Source file patterns (defaults based on lang)
public_headers = ["include/**/*.h"]  # Public header patterns
lang = "c"               # Language: c or c++ (default: c)
c_std = "11"             # C standard: 89, 99, 11, 17, 23
cpp_std = "17"           # C++ standard: 11, 14, 17, 20, 23
```

#### Target Kinds

| Kind | Description | File Output |
|------|-------------|-------------|
| `exe` | Executable binary | `myapp` / `myapp.exe` |
| `staticlib` | Static library | `libmylib.a` / `mylib.lib` |
| `sharedlib` | Shared library | `libmylib.so` / `libmylib.dylib` / `mylib.dll` |
| `header-only` | Header-only library | No artifact (headers only) |

#### Default Source Patterns

When `sources` is not specified:
- For C (`lang = "c"`): `["src/**/*.c"]`
- For C++ (`lang = "c++"`): `["src/**/*.cpp", "src/**/*.cc", "src/**/*.cxx"]`

Header-only targets don't get default sources.

### Surface Configuration

The "surface" defines compile-time and link-time requirements. There are two syntax options:

#### Shorthand Syntax (Recommended)

```toml
[targets.mylib.public]
include_dirs = ["include"]
defines = ["MYLIB_API=1", "DEBUG"]  # String format: "FOO" or "FOO=value"
cflags = ["-Wall"]
system_libs = ["m", "pthread"]      # Shorthand for system libraries
frameworks = ["Security"]            # macOS frameworks

[targets.mylib.private]
include_dirs = ["src"]
defines = ["INTERNAL=1"]
cflags = ["-Wextra"]
```

#### Full Nested Syntax

```toml
[targets.mylib.surface.compile.public]
include_dirs = ["include"]
defines = [
    "MYLIB_API=1",                    # String format
    { name = "DEBUG", value = "1" }   # Object format
]
cflags = ["-Wall"]

[targets.mylib.surface.compile.private]
include_dirs = ["src"]
cflags = ["-Wextra"]

[targets.mylib.surface.link.public]
libs = [
    "m",                              # String shorthand (system lib)
    "-lpthread",                      # -l prefix format
    { kind = "system", name = "dl" }, # Object format
    { kind = "path", path = "vendor/libfoo.a" }
]
ldflags = ["-Wl,-rpath,$ORIGIN"]
frameworks = ["Security", "Foundation"]

[targets.mylib.surface.link.private]
libs = ["internal"]
```

#### Define Formats

```toml
# All equivalent ways to define FOO=1:
defines = [
    "FOO=1",                         # String with =
    { name = "FOO", value = "1" }    # Object format
]

# Flag-only define (no value):
defines = ["DEBUG", "NDEBUG"]
```

#### Library Reference Formats

```toml
libs = [
    # String shorthands
    "pthread",              # System library
    "-lm",                  # -l prefix (same as above)
    "-framework Security", # macOS framework

    # Object formats
    { kind = "system", name = "dl" },
    { kind = "framework", name = "Foundation" },
    { kind = "path", path = "vendor/libfoo.a" },
    { kind = "package", name = "mylib", target = "mylib" }
]
```

### Target Dependencies

Fine-grained control over which surfaces propagate from dependencies:

```toml
[targets.myapp.deps]
# Simple: use default target, public visibility
mylib = "mylib"

# Detailed: specify target and visibility
mylib = { target = "mylib", compile = "public", link = "private" }
```

### Conditional Surfaces

Platform-specific configuration:

```toml
[[targets.mylib.surface.when]]
os = "linux"
[targets.mylib.surface.when."compile.public"]
defines = ["LINUX=1"]

[[targets.mylib.surface.when]]
os = "windows"
[targets.mylib.surface.when."compile.public"]
defines = ["WIN32=1"]
```

Conditions support: `os`, `arch`, `env`, `compiler`.

### [profile.NAME]

Build profiles for optimization settings.

```toml
[profile.debug]
opt_level = "0"      # 0, 1, 2, 3, s, z
debug = "2"          # 0, 1, 2, full
lto = false          # Link-time optimization
sanitizers = []      # address, thread, memory, undefined
cflags = []          # Additional compiler flags
ldflags = []         # Additional linker flags

[profile.release]
opt_level = "3"
debug = "0"
lto = true
```

### Target Support

```toml
[package]
name = "curl"
version = "8.22.0"
requires = "hosted"                       # or "freestanding"
supports = ["*-*-linux-gnu", "*-apple-darwin", "x86_64-pc-windows-msvc"]
```

These two are enforced differently on purpose, because C guarantees something
at only one of these levels.

`requires` is **checked, and fails the build**. Freestanding versus hosted is
the one split the C standard defines (C §4): a freestanding implementation
promises only `<float.h>`, `<limits.h>`, `<stdarg.h>`, `<stddef.h>` and the C11
additions, while a hosted one adds the rest of libc. So a package needing libc
on a bare-metal target is definitely broken, and the error names the package
rather than leaving you to read a cascade of missing-header failures from a
dependency you weren't thinking about. It is checked for every package in the
graph, not just the root. Omitting it means the package makes no claim and
nothing is enforced — defaulting to `hosted` would reject a freestanding build
of a package perfectly capable of one that simply never said so.

`supports` only **warns**. Above that line nothing is guaranteed: glibc, musl,
MSVC and newlib disagree on POSIX coverage, threads and sockets, so the list
records the triples someone has actually built, not the ones that can work.
Patterns are globs over the canonical triple. Building for an unlisted triple
proceeds with a warning, because a hard list would reject working builds as
targets proliferate and C's triple space is effectively unbounded.

### Platform-Conditional Sources and Flags

`[[targets.NAME.when]]` patches a target privately when its condition matches.
Conditions are `os`, `arch`, `env`, `compiler`, and `feature`. A block may
supply `sources`, `exclude`, `defines`, `cflags`, `include_dirs`, and
`prebuild`.

```toml
[[targets.crypto.when]]
arch = "aarch64"
sources = ["crypto/**/*-armv8.S"]
defines = ["VPAES_ASM=1"]

[[targets.crypto.when]]
os = "linux"
include_dirs = ["harbour-config/linux-x86_64"]   # vendored config.h
```

`include_dirs` here is for generated headers that differ per platform — a
configure-derived `config.h` is the usual case. Use it rather than putting
`-I` in `cflags`: a bare relative `-I` resolves against the process working
directory, which is the *root* package's directory when this package is a
dependency, so it silently finds nothing. Paths in `include_dirs` resolve
against the package's own root.

For requirements that must reach *consumers*, use
`[[targets.NAME.surface.when]]`, which carries `compile.public`,
`compile.private`, `link.public` and `link.private`:

```toml
[[targets.mylib.surface.when]]
compiler = "gcc"
[targets.mylib.surface.when."compile.private"]
cflags = ["-Wall", "-Wextra"]
```

A key in a `when` block that is neither a condition (`os`, `arch`, `env`,
`compiler`, `feature`) nor one of those four tables is rejected. The condition
fields are flattened into the block, so serde cannot tell a typo from a
condition it has not been taught about — the check is explicit for that reason.

### Assembly Sources

`.S`, `.s`, and `.asm` sources compile alongside C and C++ in the same target --
most crypto and codec libraries are laid out that way. Language is chosen per
file by extension, so a target's `lang` only decides ambiguous cases (a `.c` in
a `lang = "c++"` target still compiles as C++).

```toml
[targets.crypto]
kind = "staticlib"
sources = ["src/**/*.c", "src/**/*.S"]
```

`.S` (capital) runs through the C preprocessor, so `include_dirs` and `defines`
from the compile surface apply and `#include`d headers participate in
incremental rebuilds. `.s` is passed to the assembler unpreprocessed.

MSVC is not supported for assembly: it assembles with a separate,
architecture-specific assembler (`ml64.exe`, `armasm64.exe`) rather than `cl`,
and a target with assembly sources is rejected with a dedicated error there.

### Pre-Build Code Generation

`[[targets.NAME.prebuild]]` runs a command before the target is built. Its
purpose is code generation: a script that writes a header, or a whole
translation unit, that the target then compiles.

```toml
[targets.decoder]
kind = "staticlib"
sources = ["src/**/*.c", "generated/*.c"]

[targets.decoder.private]
include_dirs = ["generated"]

[[targets.decoder.prebuild]]
program = "python3"
args = ["tools/gen_decoder.py", "--out", "generated"]
outputs = ["generated/decoder_table.c", "generated/decoder_table.h"]
```

- `program`, `args`, `env` describe the command; `cwd` is relative to the
  package root and defaults to it. Several blocks may be given and run in
  order.
- `outputs` lists the files the step must produce, relative to the package
  root. This is enforced: a generator that exits successfully without
  writing every declared output fails the build, naming what is missing.
  Declare generated sources here rather than leaving them implicit.

Generated sources are compiled. `sources` is expanded *after* the
generators for that target have run, so `generated/*.c` above matches the
file the generator just wrote, on a clean checkout as well as a rebuild.
Generated sources may also be named individually rather than globbed.

Two consequences follow from that ordering:

- Generators run while the build plan is being computed, so
  `harbour build --plan` runs them too. The set of compile steps cannot be
  known without them.
- Generators are re-run on every build; their inputs are not tracked. This
  does not by itself cause recompilation: fingerprints are taken after
  regeneration, so a generator that rewrites byte-identical output leaves
  everything downstream up to date. Keep generators deterministic and
  reasonably cheap.

Packages are processed in dependency order, so a dependency's generated
headers exist before any dependent is planned.

#### Per-Platform Generators

A generator is often the most platform-specific step a package has, so
`prebuild` may also appear inside a `[[targets.NAME.when]]` block. Matching
blocks contribute their generators in addition to the unconditional ones,
which run first.

```toml
[[targets.crypto.when]]
os = "linux"
arch = "x86_64"
sources = ["generated/*.S"]

[[targets.crypto.when.prebuild]]
program = "perl"
args = ["crypto/aes/asm/aesni-x86_64.pl", "elf", "generated/aesni-x86_64.S"]
outputs = ["generated/aesni-x86_64.S"]

[[targets.crypto.when]]
os = "macos"
arch = "x86_64"
sources = ["generated/*.S"]

[[targets.crypto.when.prebuild]]
program = "perl"
args = ["crypto/aes/asm/aesni-x86_64.pl", "macosx", "generated/aesni-x86_64.S"]
outputs = ["generated/aesni-x86_64.S"]
```

Conditions are the same `os`/`arch`/`env`/`compiler`/`feature` set as every
other `when` block, and are evaluated against the platform being built
*for*, so cross-compiling selects the right generator. A generator behind a
condition that does not match is not run at all.

### Backend Configuration

Target-specific backend configuration:

```toml
[targets.mylib.backend]
backend = "cmake"     # native, cmake, meson, custom

[targets.mylib.backend.options]
CMAKE_POSITION_INDEPENDENT_CODE = "ON"
CMAKE_CXX_STANDARD = 17
```

### Build Recipe

For non-native build systems.

**Recipes are a second-class escape hatch.** A target built by CMake or Meson is
opaque to Harbour, which means it is rebuilt in full on every build (recipe steps
are not fingerprinted), receives no surface flags, and contributes nothing to
`compile_commands.json`. Prefer a native shim listing sources and defines; prefer
vcpkg for packages that genuinely resist shimming. See "Package Build Strategy"
in ARCHITECTURE.md for the reasoning.


```toml
[targets.mylib]
kind = "staticlib"

[targets.mylib.recipe]
type = "cmake"
source_dir = "."
args = ["-DBUILD_SHARED=OFF"]
targets = ["mylib"]

Recipe steps receive `HARBOUR_ARTIFACT_DIR` (where Harbour expects this
target's artifacts, so dependents can find them) and `HARBOUR_PACKAGE_ROOT`.
A recipe building a library that others depend on must copy its output to
`$HARBOUR_ARTIFACT_DIR/lib<target>.a` — nothing else puts it there. Step
output is captured and shown with `-v`.

# Or custom commands:
[targets.mylib.recipe]
type = "custom"
[[targets.mylib.recipe.steps]]
program = "make"
args = ["-j4"]
cwd = "."
outputs = ["build/libmylib.a"]
```

## Complete Example

```toml
[package]
name = "myapp"
version = "1.0.0"
license = "MIT"

[build]
cpp_std = "17"

[dependencies]
zlib = { git = "https://github.com/madler/zlib", tag = "v1.3.1" }
mylib = { path = "../mylib" }

[targets.myapp]
kind = "exe"
lang = "c++"
sources = ["src/**/*.cpp"]

[targets.myapp.private]
include_dirs = ["src"]
cflags = ["-Wall", "-Wextra"]
system_libs = ["pthread"]

[targets.myapp.deps]
zlib = "zlib"
mylib = "mylib"

[profile.release]
opt_level = "3"
lto = true
```

## Validation

Harbour validates manifests strictly:
- Unknown fields are rejected (typo detection)
- Invalid values produce errors with line numbers and context
- Source patterns in C++ targets require `lang = "c++"`
- Header-only targets must not have sources or recipes

## See Also

- [README.md](README.md) - Getting started guide
- [CLI documentation](README.md#usage) - Command reference
