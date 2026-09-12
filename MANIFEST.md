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
default_target = "mylib"      # Optional: see "Default Target" below
```

#### Default Target

When a dependent writes `mylib = "mylib"` under `[targets.X.deps]` without a
`target = "..."`, it gets this package's *default target*. The rule is:

1. `[package] default_target`, if set.
2. Otherwise the first **library** target declared in the manifest.
3. Otherwise the first target declared in the manifest.

"First" means first in the file. Target declaration order is preserved
exactly as written.

`default_target` is a package-level key naming a target rather than a
`default = true` flag on a target, so that only one target can ever claim
it and the answer is readable from `[package]` alone.

It must name a target this package declares. A `default_target` that names
nothing is a parse error listing the targets that do exist -- it does not
quietly fall back to the positional rule.

A package with several library targets is the case worth setting it for;
without it, dependents get the first one declared and Harbour warns that
the others are not linked. Setting `default_target` silences that warning,
because the ambiguity has been resolved.

Workspaces: `default_target` belongs to one package. A member sets its own
and the root's choice does not apply to it. A virtual workspace has no
`[package]` and therefore no default target; `[workspace] default_target`
is rejected.

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
c_std = "11"             # C standard: 89, 99, 11, 17, 23, or the GNU
                         # dialect forms gnu89 ... gnu23 (see below)
cpp_std = "17"           # C++ standard: 11, 14, 17, 20, 23
freestanding = false     # Build without a hosted libc (see below)
linker_script = "..."    # Linker script, relative to the package root
entry = "_start"         # Entry symbol
```

#### `c_std`

Applies to the C sources of this target, and only to them: an assembly
source in the same target is compiled without it (`-std=` describes a C
dialect), and a C++ source takes the graph-wide C++ standard from
`[build] cpp_std` instead.

Unlike `cpp_std`, `c_std` is **per target** and is not folded across the
dependency graph. The C standard does not change the C ABI, so two packages
compiled at different C standards still link; `exceptions`, `rtti` and the
C++ standard do change the C++ ABI, which is why those are graph-wide and
this is not.

Both dialects are spellable, because the difference is load-bearing in real
C code:

| Value | Flag | Meaning |
|-------|------|---------|
| `"99"`, `"c99"` | `-std=c99` | Strict ISO C99. Defines `__STRICT_ANSI__`. |
| `"gnu99"` | `-std=gnu99` | C99 plus the GNU dialect: `typeof`, statement expressions, `asm`. |

`89`/`90`, `99`, `11`, `17`/`18` and `23` are accepted, each with a `gnu`
prefixed form.

**On MSVC**, `cl` has only `/std:c11` and `/std:c17`, and no GNU dialect at
all. A `c_std` it cannot express (`89`, `99`, `23`) is reported as a warning
naming the standard, and those sources compile in `cl`'s default C mode;
a `gnu` form compiles as the corresponding ISO standard, also with a
warning. Guard the setting with a `[[targets.NAME.when]] compiler = "msvc"`
block if the difference matters to your code.

It is inspectable without building: `harbour flags NAME --compile` prints
the `-std=` it will use, attributed to the target.

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

[targets.mylib.surface.abi]
toggles = ["pic", "visibility", "crt", "stdlib"]
```

#### `surface.abi`

`toggles` names the axes this target's ABI depends on. It deliberately emits
no compiler or linker flag — it is a declaration, not a setting. What it does
is enter the target's ABI fingerprint alongside its public defines, so
changing the declaration re-produces the library (and therefore anything
linking it) instead of serving a stale artifact from the cache. Changing it
recompiles nothing, because no compile flag depends on it.

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
]
```

A relative `kind = "path"` resolves against **the root of the package whose
manifest declares it**, exactly like `include_dirs`. So a dependency can
vendor an archive at `vendor/libfoo.a` and name it that way, and it still
resolves correctly when the package is built as a dependency from somewhere
else. Absolute paths are passed through unchanged.

`{ kind = "package", name = "...", target = "..." }` is **rejected with an
error**. It parsed and emitted nothing at all — not even an error for a
package that does not exist — so it is refused rather than accepted in
silence. Depend on the package through `[dependencies]` and
`[targets.NAME.deps]` instead; that is what actually puts a sibling
package's archive on the link line. Tracked in
[#96](https://github.com/aryamurray/harbour/issues/96).

`groups` on a link table is **rejected with an error** for the same reason:
it parsed, propagated as far as the effective link surface, and never became
a `--start-group`, `--end-group` or `--whole-archive`. Tracked in
[#95](https://github.com/aryamurray/harbour/issues/95).

Both checks apply to `surface.link.public`, `surface.link.private`, the
`[targets.X.public]`/`[targets.X.private]` shorthand, and the `link.*`
tables inside `surface.when` — every table that can carry them, including in
a dependency's manifest.

### Target Dependencies

Fine-grained control over which surfaces propagate from dependencies:

```toml
[targets.myapp.deps]
# Simple: use the dependency's default target (see "Default Target"),
# public visibility
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

Conditions support: `os`, `arch`, `env`, `compiler`, `feature`.

`compiler` is matched against the *detected toolchain family*, and there are
four of those, not three: `gcc`, `clang`, `apple-clang`, `msvc`.

Matching is by **family, not string equality**: `compiler = "clang"` matches
both `clang` and `apple-clang`, so a block guarding clang-only flags fires on
macOS. `compiler = "apple-clang"` remains the narrower match for the rare flag
that is Apple's alone.

Nothing else widens, and the omissions are deliberate:

- `compiler = "gcc"` matches **only** real GCC. On macOS `/usr/bin/gcc` is
  clang, but the family is detected by probing the toolchain rather than by
  the name it was invoked under, so a Mac reports `apple-clang` either way. A
  `gcc` block is where GCC-only flags live (`--param=`, `-fno-tree-*`), and
  clang rejects those outright — widening `gcc` would turn a silent no-op into
  a failed build.
- `compiler = "clang"` does not match `msvc`, and would not match `clang-cl`
  if that family were added: `clang-cl` takes MSVC flag *syntax* (`/W4`), so
  a `clang` block full of `-W...` flags would be wrong for it.

The rule, if a fifth family is ever added: two families group together when a
flag written for one is accepted by the other.

A `compiler` value that is not one of the four matches nothing (it is not an
error, and not a wildcard).

### [profile.NAME]

Build profiles for optimization settings.

```toml
[profile.debug]
opt_level = "0"      # 0, 1, 2, 3, s, z, g, fast
debug = "2"          # 0, 1, 2, full
lto = false          # Link-time optimization
sanitizers = []      # address, thread, memory, undefined, leak
cflags = []          # Additional compiler flags
ldflags = []         # Additional linker flags

[profile.release]
opt_level = "3"
debug = "0"
lto = true
```

Every value is a **string**, and every value is **checked**: an `opt_level`,
`debug` level or sanitizer name outside the lists above is an error naming
the valid values, not a flag pasted through to the compiler. `opt_level =
"fastest"` used to reach the compiler as `-Ofastest`.

#### What the compiler is actually told

The keys above describe *intent*. Each toolchain spells it, because GCC and
MSVC do not share a flag syntax and Harbour used to emit GCC's to both --
`cl.exe` answered `-O3`, `-g` and `-fsanitize=address` with `D9002: ignoring
unknown option` and built anyway, so **every Windows release build was
unoptimised and no Windows build had ever carried debug information**
([#100](https://github.com/aryamurray/harbour/issues/100)).

| setting | GCC / clang | MSVC |
|---|---|---|
| `opt_level = "0"` | `-O0` | `/Od` |
| `opt_level = "1"` | `-O1` | `/O1` |
| `opt_level = "2"` | `-O2` | `/O2` |
| `opt_level = "3"` | `-O3` | `/O2` |
| `opt_level = "s"` | `-Os` | `/O1` |
| `opt_level = "z"` | `-Oz` | `/O1` |
| `opt_level = "g"` | `-Og` | *error* |
| `opt_level = "fast"` | `-Ofast` | *error* |
| `debug = "0"` | *(nothing)* | *(nothing)* |
| `debug = "1"` | `-g` | `/Z7` |
| `debug = "2"` / `"full"` | `-g3` | `/Z7`, and `/DEBUG` when linking |
| `sanitizers = ["address"]` | `-fsanitize=address` when compiling and linking | `/fsanitize=address` when compiling, `/INCREMENTAL:NO` when linking |
| `sanitizers = ["thread" / "memory" / "undefined" / "leak"]` | `-fsanitize=<name>` when compiling and linking | *error* |
| `lto = true` | `-flto` when compiling and linking | `/GL` when compiling, `/LTCG` when linking |

The non-obvious entries:

- **`"3"` maps to `/O2`.** MSVC has nothing above `/O2`; `/Ox` is documented
  as a *strict subset* of it and `/Og` is deprecated. `"s"` and `"z"` both
  map to `/O1`, which *is* MSVC's size preset -- bare `/Os` only states a
  size-versus-speed preference within an already-enabled level.
- **`"g"` and `"fast"` are an error on MSVC**, not an approximation. `-Og`
  optimises while staying debuggable and `-Ofast` permits standards-violating
  maths; MSVC has neither mode, and mapping them to `/Od` or `/O2` would
  deliver the opposite of half of what was asked for. Put the exact flag you
  want in `cflags` if you need one.
- **Debug information on MSVC is `/Z7`, not `/Zi`.** `/Zi` writes a separate
  PDB which, for a file compiled outside a Visual Studio project, is named
  `VC<x>.pdb` -- one shared file that every parallel `cl` would be writing at
  once. `/Z7` embeds the CodeView records in each `.obj`, and `/DEBUG` at
  link time turns them into a single PDB beside the image. MSVC has no level
  gradation, so `debug = "1"` and `"2"` are the same command line there.
- **`lto` puts a flag on both command lines.** That is what LTO requires: the
  compiler only emits IR instead of machine code when told at compile time,
  so a link-time-only `-flto` has nothing to optimise. `lto` is a bool, so it
  selects full (monolithic) LTO; clang's cheaper `-flto=thin` has no spelling
  in the schema today
  ([#103](https://github.com/aryamurray/harbour/issues/103)).
- **A sanitizer the toolchain does not implement fails the build.** MSVC
  implements AddressSanitizer and nothing else of the set. Accepting the
  others and emitting nothing would leave you with a build that reports
  success and is not sanitized.

`cflags` and `ldflags` are passed through verbatim -- they are one
compiler's syntax by definition -- so a profile carrying them is a profile
that only works on one toolchain. A flag that needs to differ per compiler
belongs in a target's `surface.when` block guarded by `compiler = "..."`,
which is the only place conditions are evaluated; `[profile]` has no `when`.

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

A `prebuild` generator may produce the linker script: generators run during
planning, before the script is looked for, so templating one with memory sizes
works.

Both `harbour flags` and `harbour linkplan` report these with a provenance of
`target config`, so what the linker receives is inspectable without building.
Flags from a `[[targets.NAME.surface.when]]` block's `link.private` compose
with them and keep their own attribution — which is how `-fuse-ld=lld` is
declared for hosts whose default linker cannot do a freestanding link.

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

Both `when` blocks reject a key that is neither a condition (`os`, `arch`,
`env`, `compiler`, `feature`) nor one of the keys that block accepts. The
condition fields are flattened into the block, so serde cannot tell a typo
from a condition it has not been taught about — the check is explicit in both
places for that reason.

The two accept **different** keys, which is the mistake worth naming:
`ldflags`, `libs` and `frameworks` read naturally next to `cflags` but exist
only on `surface.when`, under `link.public`/`link.private`. Writing one in a
target-level `[[targets.NAME.when]]` block is an error that names where the
key belongs.

Everything both blocks contribute is **additive**: matching blocks are
appended to the base surface. Order is preserved end to end — flags reach the
compiler and linker in the order they were declared — and duplicates are
removed without reordering anything.

- **Order is meaningful, and last wins for `cflags`.**
  `cflags = ["-Wall", "-Wno-error", "-Werror"]` reaches the compiler exactly
  as written, so `-Werror` wins and a warning fails the build. This is the
  only override mechanism the schema has, so it is worth knowing precisely:
  within one table, declaration order; across tables, the order below.
- **The order flags are folded in** is: this target's
  `surface.compile.private`, then its matching `[[targets.NAME.when]]`
  blocks, then `surface.compile.public`, then each dependency's
  `surface.compile.public` with dependents before dependencies. So a
  dependency cannot override a flag the depending target set, and a target's
  own private flags come first — which for `include_dirs` is what you want,
  since `-I` is first-match-wins.
- **Duplicates are removed keeping the occurrence that preserves meaning.**
  A repeated `cflag` keeps its *last* position (last-wins); a repeated
  `include_dir`, `-L`, `-framework` or `ldflag` keeps its *first*
  (first-match-wins for search paths, and a positionally scoped linker flag
  such as `-Wl,--whole-archive` must not move later than the archives it
  wraps). `defines` are neither reordered nor deduplicated.
- **There is no way to remove a flag or define for one platform.** `exclude`
  removes *sources*; nothing removes a `-D` or a `-f`. Express the difference
  by only adding it under the condition where it applies — or, for a flag
  that has a negating form, by relying on last-wins.

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

### Configure-Style Probes

`[targets.NAME.probes]` asks the *actual* target toolchain questions and turns
the answers into defines on the target's compile surface. It is Harbour's
replacement for a vendored, configure-generated `config.h`.

```toml
[targets.mylib.probes]
check_headers = ["sys/socket.h", "sys/ioctl.h", "poll.h", "windows.h"]
check_sizeof = ["long", "size_t", "void *"]
```

That build compiles with, on a Mac:

```
-DHAVE_SYS_SOCKET_H=1 -DHAVE_SYS_IOCTL_H=1 -DHAVE_POLL_H=1
-DSIZEOF_LONG=8 -DSIZEOF_SIZE_T=8 -DSIZEOF_VOID_P=8
```

`windows.h` is absent, so it contributes **nothing** — not `=0`. C code tests
`#ifdef HAVE_WINDOWS_H`, which `#define HAVE_WINDOWS_H 0` would satisfy.

#### Probe kinds

Two are implemented; three more are designed. See
`docs/superpowers/specs/2026-09-11-native-probes-design.md`.

| kind | question | how |
|------|----------|-----|
| `header` | does `#include <X>` compile? | one compile |
| `sizeof` | what is `sizeof(T)`? | 7 compiles, binary search on a compile-time predicate |

**Every probe kind is answerable when cross-compiling**, and that is the rule
deciding which kinds exist rather than a happy accident. Nothing is ever
executed: a `sizeof` answer comes from bisecting `char probe[(sizeof(T) <= N)
? 1 : -1]`, which fails to compile iff the size exceeds `N`. There is no
"run the program and read its output" kind, and no cross-compilation fallback
value — a fallback value is a guess, which is the vendored `config.h` with
extra steps. A question that genuinely needs the target to *run* (does
`malloc(0)` return non-NULL) is not expressible; assert it as a literal define
under a `[[targets.NAME.when]]` block, where a reviewer can see it is an
assertion.

#### Naming

Bulk lists are auto-named: uppercase, non-alphanumeric characters become `_`,
runs collapse, `*` becomes `P`, and the prefix is `HAVE_` or `SIZEOF_`.

| written | define |
|---|---|
| `check_headers = ["sys/socket.h"]` | `HAVE_SYS_SOCKET_H` |
| `check_sizeof = ["long long"]` | `SIZEOF_LONG_LONG` |
| `check_sizeof = ["void *"]` | `SIZEOF_VOID_P` |

`void*` and `void *` both give `SIZEOF_VOID_P`, so a define name cannot depend
on whitespace. (autoconf gives `SIZEOF_VOIDP` for `void*`; this deliberately
differs.)

#### Named probes

For a custom name, or for options the bulk lists cannot express, use the
`named` sub-table:

```toml
[targets.mylib.probes.named.HAVE_NETINET_IN_H]
header = "netinet/in.h"
prelude = ["sys/types.h", "sys/socket.h"]

[targets.mylib.probes.named.SIZEOF_CURL_OFF_T]
sizeof = "long long"

[targets.mylib.probes.named.SIZEOF_OFF_T]
sizeof = "off_t"
prelude = ["sys/types.h"]
```

- Exactly one of `header` or `sizeof` per probe. Two is an error; none is an
  error.
- `prelude` is a list of **header names**, never a code fragment. BSD-derived
  headers need prerequisites (`sys/socket.h` before `netinet/in.h`), and a
  type's size is only askable where the type is visible — `sizeof(off_t)` has
  no answer without `<sys/types.h>`.
- A `sizeof` probe automatically gets `<stddef.h>`, plus `<stdint.h>`,
  `<time.h>` and `<sys/types.h>` when `__has_include` says they exist. So
  `SIZEOF_TIME_T` and `SIZEOF_SIZE_T` need no `prelude`.
- The name must be a valid C identifier — it becomes a `-D`.
- Bulk and named entries that produce the same name are an error, not a
  silent override.

The `named` sub-table exists rather than letting probes sit directly next to
`check_headers` because `#[serde(deny_unknown_fields)]` does not survive a
`flatten`: three of the ten defects in the 2026-09-07 schema audit were keys
silently routed into a flattened struct and dropped. `check_headerz = [...]`
is a hard error here, not a probe that never runs.

#### Visibility

Probe answers are **private to the target that declares them**. They reach that
target's own translation units and nothing else; a dependent does not see them.

There is no `visibility` key, and the omission is worth explaining because the
opposite was built first. `visibility = "public"` parsed, was branched on, and
folded its defines into the target's ABI cache key so a consumer would
relink — and it did not work. A dependent's compile surface is folded from each
dependency's *declared* `surface.compile.public`, and a measured answer exists
in no manifest, so it never propagated. The consumer failed to compile on an
undefined `SIZEOF_LONG` while the field looked, from the library's side, like
it worked. Rather than ship a key that asks for something that does not happen,
the key is gone; `visibility = "public"` is a hard error.

The consequence for package authors: a library whose *public header* is
`#ifdef`'d on a probe result cannot express that yet. Keep probe-dependent code
in private headers and `.c` files, and declare anything a consumer must see as
a literal define on the public surface.

#### What probes see

A probe is compiled with the target's resolved `include_dirs` and `defines`,
plus the flags the target triple requires (`-target`, `-mcpu`, `--sysroot`).
That matters: "does `zlib.h` exist" has no answer without the `-I` a
dependency contributes.

A probe is **not** given the target's own `cflags`, nor the profile's. A
package with `-Werror` in `cflags` would otherwise fail every probe on an
incidental warning and report every `HAVE_*` as `no`, and `-O2`/`-g` cannot
change whether a header exists. The consequence worth knowing: a manifest that
puts `-I` or `--sysroot` in `cflags` rather than in `include_dirs` is invisible
to probes. Use `include_dirs`.

#### Ordering

Probes run during planning, after the compile surface is resolved and before
the pre-build generators, source globbing and compile-command construction.
`harbour build --plan`, `harbour flags` and `harbour linkplan` therefore run
them too, on the same terms as generators — none of them can report the right
answer otherwise.

`harbour flags` lists probe defines with a provenance of `probe`, so what the
compiler receives is inspectable without building:

```
# Compile flags for `mylib`:
  -DHAVE_SYS_SOCKET_H=1    # from: mylib 0.1.0 (probe)
  -DSIZEOF_LONG=8          # from: mylib 0.1.0 (probe)
```

The attribution is its own kind rather than a `surface` table because there is
no manifest line to point at: the value was measured, and knowing that is what
tells you a toolchain change can change it.

Two consequences:

- **Probes cannot read each other's answers.** They are all evaluated against
  one fixed pre-probe surface. "Check for `X` only if header `Y` exists" is
  not expressible; in practice it degrades correctly, because a probe naming a
  header that does not exist fails to compile and answers `no`.
- **Probe defines are appended after the declared surface**, in declaration
  order — bulk `check_headers`, then `check_sizeof`, then `named`. Since
  defines and cflags are last-wins at the compiler, a literal define in the
  manifest can still override a probed one.

#### Caching

Answers are cached in
`.harbour/<...>/probe/<package>/<target>/probes.json`, keyed on the toolchain
fingerprint (the same one that keys compile fingerprints), a hash of the
pre-probe compile surface, and a per-probe hash of the probe's own spec. A
warm rebuild spawns no compiler for probes at all; changing the compiler, the
target triple, the include path or one probe's declaration re-measures what it
must. The cache is discarded wholesale when a key differs rather than merged,
so it can never hold answers from two toolchains at once.

#### Failure

There are three outcomes, and two of them are answers:

- the compiler exits 0 — **yes**
- the compiler exits non-zero — **no**, a real answer
- the compiler could not be run, or died on a signal — **error**, not an
  answer

Before any probe runs, Harbour compiles `int main(void) { return 0; }` with
exactly the flags probes use. If that fails, the build stops and quotes the
compiler. Without this check a broken toolchain or a missing sysroot would
make every probe answer `no`, and the package would configure itself for a
machine that does not exist and then compile — the classic catastrophic
`configure` failure.

Asking for the size of a type that does not exist is an error, not `0`: a
`#define SIZEOF_FOO 0` is indistinguishable from a real answer. `sizeof` is
bounded at 64 bytes.

#### Not yet implemented

- `symbol`, `type` and `flag` probe kinds.
- A generated `config.h` the package `#include`s. Answers only become `-D`
  flags today, so a package needing 250 answers in a header (curl, openssl)
  still vendors one. There is deliberately no `emit` key until there is a
  second thing for it to select — a single-valued knob is a knob that does
  nothing, and `emit = "defines"` is a hard error rather than a no-op.
- Propagating answers to dependents. See "Visibility" above.
- Passing probe answers to a `prebuild` generator.
- **MSVC is unverified.** The probe compile is built by the same
  `Toolchain::compile_command` the real build uses, so `cl /c /Fo` is
  generated rather than guessed, and the negative-array predicate is
  ill-formed under `cl` as it is everywhere. Neither claim has been run on a
  Windows host.

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
- Sources named individually (not matched by a glob) must exist
- `libs` entries must be link *names*, not filenames — `libs = ["libssl.a"]`
  would become `-llibssl.a`, and is refused with the correct spelling in the
  error. This check does not currently reach `libs` inside a
  `[[targets.NAME.surface.when]]` block.

Two places where "unknown fields are rejected" does not hold, both because
serde cannot see unknown keys through a `#[serde(flatten)]`:

- `[targets.NAME.deps]` in its detailed form. `compile` and `link` are
  compared against the literal string `"private"`, so any other value —
  including a typo like `"privte"`, or `"PRIVATE"` — silently means
  `public`, and a misspelled *key* (`compil = "private"`) is dropped.
- The `when` blocks were the same until each grew an explicit check; see
  "Platform-Conditional Sources and Flags".

## Known Gaps

Recorded rather than fixed, so they are not rediscovered by debugging a
build:

- **`harbour flags` omits the C++ language options.** It prints exactly the
  compile and link command lines otherwise — same fold as the build, same
  order, same deduplication, plus the profile's own flags — but `-std=`,
  `-fno-exceptions`, `-fno-rtti` and `-stdlib=` are chosen per source file
  from the graph-wide C++ standard, so they are not a property of the target
  the way everything else it prints is. A C source in a mixed target does
  not receive them at all. A target's own `c_std` *is* printed, since
  it is a property of the target and not of the graph.
  `tests/cli_integration.rs::test_flags_matches_the_real_compile_command`
  captures the real argv the compiler is handed and asserts the rest is
  identical, so this is the only gap.
- **A package with more than one library target resolves by position unless
  it says otherwise.** Consumers that do not pin `target = "..."` get
  `[package] default_target` if it is set, and otherwise the first library
  target in declaration order. That is well-defined but implicit, so pinning
  `target = "..."` or setting `default_target` is still clearer than relying
  on it. Harbour warns when it notices.
- **`link.*.groups` and `libs = [{ kind = "package" }]` are hard errors.**
  They parse but reach no command line, so they are refused rather than
  silently ignored ([#95](https://github.com/aryamurray/harbour/issues/95),
  [#96](https://github.com/aryamurray/harbour/issues/96)).
- **`surface.compile.requires_cpp` and `[features]` are implemented but not
  described here.** `requires_cpp` raises the graph-wide C++ standard;
  `[features]` works as Cargo's does, including `dep/feature`.

## See Also

- [README.md](README.md) - Getting started guide
- [CLI documentation](README.md#usage) - Command reference
