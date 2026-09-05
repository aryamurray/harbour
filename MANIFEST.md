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
freestanding = false     # Build without a hosted libc (see below)
linker_script = "..."    # Linker script, relative to the package root
entry = "_start"         # Entry symbol
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

### Freestanding and Bare-Metal Targets

Three target-level keys build a payload that runs with no operating system
underneath it — a boot image, a hypervisor component, firmware.

```toml
[package]
name = "payload"
version = "0.1.0"
requires = "freestanding"

[targets.payload]
kind = "exe"
sources = ["src/start.S", "src/main.c"]
freestanding = true
linker_script = "boot/layout.ld"
entry = "_start"
```

| Key | Compiler | Linker |
|-----|----------|--------|
| `freestanding = true` | `-ffreestanding` | `-nostdlib` |
| `linker_script = "P"` | — | `-Wl,-T,<package root>/P` |
| `entry = "NAME"` | — | `-Wl,--entry=NAME` |

**These are target keys, not a target kind.** A freestanding image is linked
exactly like an `exe` — objects in, one file out, same driver, same output
naming — so `kind` stays `exe`. What changes is *how* it is built, which is
also why this is separate from `[package] requires`: `requires` is a claim
about what the package's code can run on and is checked across the whole
dependency graph, while these say how this one artifact is produced. Declare
both; they answer different questions.

`linker_script` resolves against **the package's own root**, never the
directory `harbour` was run from. That distinction is invisible while the
package is the root of the build and breaks the moment it is a dependency,
because the process working directory during a build is the *root* package's.
Absolute paths are used verbatim.

Which keys each kind accepts:

| Kind | `freestanding` | `linker_script` / `entry` |
|------|----------------|---------------------------|
| `exe`, `sharedlib` | yes | yes |
| `staticlib` | yes — it changes how this library's own sources compile | **rejected**: `ar` archives, it never links, so nothing would read them |
| `header-only` | **rejected** | **rejected** — never compiled, never linked |

Notes and limits:

- **Per target, not per graph.** `freestanding = true` applies to *this*
  target's translation units. A dependency is compiled from its own manifest,
  so a library meant for bare metal has to say `freestanding = true` itself
  (and `requires = "freestanding"` to have that checked).
- **`-nostdlib` also drops libgcc.** Code needing the compiler's runtime
  helpers (64-bit division, `__aeabi_*`) must ask: `libs = ["gcc"]`.
- **GCC/Clang drivers only.** A target using any of these keys is rejected
  under MSVC, whose equivalents (`/NODEFAULTLIB`, `/ENTRY:`) are not wired and
  which has no linker-script concept at all.
- **Not linkable on Apple targets.** `ld64` has no `-T` and refuses a
  `-nostdlib` link. Building a freestanding target for an Apple triple warns
  and then fails in the linker. Use a bare-metal triple with a cross toolchain
  (`harbour build --target-triple aarch64-unknown-none`), or put
  `-fuse-ld=lld` in the target's `ldflags`.
- **The linker produces an ELF, not a raw image.** There is no `objcopy`
  post-link step yet; converting to a flat binary is still a manual step.
- **A comma in the script path is rejected.** The script is passed as
  `-Wl,-T,<path>`, and `-Wl,` splits its argument on commas, so the path
  would reach the linker in pieces.
- **Untested on Windows with a GCC/Clang driver.** MSVC — the default there —
  refuses these keys, so the only way to reach the flags on Windows is a
  MinGW/clang toolchain selected deliberately. In that configuration the
  emitted path mixes separators (`-Wl,-T,C:\pkg\boot/layout.ld`, because
  `Path::join` appends `\` and leaves the `/` inside the manifest value
  alone). Whether MinGW `ld` accepts that is unverified.

Both `harbour flags` and `harbour linkplan` report these with a provenance of
`target config`, so what the linker receives is inspectable without building.

### Platform-Conditional Sources and Flags

`[[targets.NAME.when]]` patches a target privately when its condition matches.
Conditions are `os`, `arch`, `env`, `compiler`, and `feature`.

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
`[[targets.NAME.surface.when]]` with `compile.public` / `link.public` instead;
the target-level block above is private to this target's own compilation.

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
