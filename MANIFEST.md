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

For non-native build systems:

```toml
[targets.mylib]
kind = "staticlib"

[targets.mylib.recipe]
type = "cmake"
source_dir = "."
args = ["-DBUILD_SHARED=OFF"]
targets = ["mylib"]

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
