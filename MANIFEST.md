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

Workspace-level build configuration. **Only the `[build]` of the package
being built is read.** A dependency's is ignored, and Harbour warns when it
finds one: these are ABI decisions, and a graph with two answers for
`exceptions` or `rtti` links and then misbehaves, so they come from whoever
is building. A dependency that needs a minimum C++ standard should say so
with `[targets.NAME] cpp_std` or
`[targets.NAME.surface.compile] requires_cpp`, both of which *do* raise the
graph-wide standard.

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


`default-features` is spelled with a hyphen, as in Cargo. The underscore
form `default_features` is accepted as an alias. Until recently only the
underscore form worked: the hyphenated key was absorbed and discarded, and
the dependency was built with its default features on regardless.

A key that is neither is now an error naming the key and the dependency. It
used to parse and vanish, which mattered here more than anywhere else in the
schema: `brnach = "main"` meant the default branch and `verison = "1.2"`
meant any version, so the value being dropped decided *which source was
fetched*.

#### Optional dependencies

`optional = true` means the dependency is not resolved, **not fetched**, not
built and not linked unless some enabled feature activates it. Harbour uses
Cargo's spellings:

```toml
[dependencies]
ssl = { path = "../ssl", optional = true }
zstd = { path = "../zstd", optional = true }

[features]
# `ssl` is an implicit feature: an optional dependency defines a feature of
# its own name, so a dependent writing `features = ["ssl"]` activates it.
default = []

# `dep:zstd` activates the dependency without defining a feature called
# `zstd` -- and *suppresses* the implicit one, so `zstd` stops being a
# feature name a dependent can ask for.
compress = ["dep:zstd"]

# `optdep/feature` activates the dependency and enables one of its features.
fast-ssl = ["ssl/asm"]
```

Three rules worth stating explicitly:

- **Activation is unified across the whole graph**, the same way feature
  sets are, and for the same reason: a C build links one copy of each
  library. An optional dependency that *any* package in the build activates
  is in the build for everyone, including dependents that never asked for
  it.
- **`dep:NAME` may only name a dependency declared `optional = true`.**
  Naming a required dependency is an error, not a no-op.
- **Cargo's weak form `dep?/feature` is a hard error.** Reading it as the
  strong form would activate a dependency the author explicitly asked not to
  activate.

"Not fetched" is the load-bearing part: an optional `git` dependency that no
feature activates is never cloned, because nothing ever queries its source.
Resolution starts from "nothing is active" and grows, rather than resolving
everything and pruning.

Because `[features]` now decides graph *membership*, a manifest's
`[features]` table is part of the lockfile's freshness hash. Editing
`default = []` to `default = ["ssl"]` re-resolves; it used to leave the
lockfile looking fresh, which meant the newly-activated dependency silently
stayed out of the build.

`harbour add --optional` writes the key.

**`optional` belongs on the member's own entry, and is refused in
`[workspace.dependencies]`** — as in Cargo. It is not a property of the
dependency but of the relationship between one package and it, and it only
means anything alongside that package's `[features]` table, which is
per-member. Everything else still inherits:

```toml
# workspace root -- no `optional` here
[workspace.dependencies]
ssl = { version = "3.0", features = ["base"] }

# member
[dependencies]
ssl = { workspace = true, optional = true }
```

One gap: a workspace member that the resolver never reaches from the root
member does not get its *own* `[features]` consulted for activation (see
`ops::resolve::activated_optional_dependencies`). The failure mode is a loud
"not found in dependency graph", not a silently wrong link.

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

#### `public_headers`

Records *which* headers are public. It does **not** add an include directory
and does not install anything: two things read it, the private-define ABI
lint and `harbour ffi generate`'s header discovery, and neither is the
compile line. A library that declares `public_headers` and no public
`include_dirs` exports nothing a consumer can include, so Harbour warns when
it sees that combination. The include directory is the thing consumers get:

```toml
[targets.mylib]
public_headers = ["include/**/*.h"]   # which headers are public

[targets.mylib.public]
include_dirs = ["include"]            # how consumers find them
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

# Any other name is a *named* profile, selected with `--profile asan`.
[profile.asan]
inherits = "release"
opt_level = "1"
sanitizers = ["address"]
```

Select a profile with `harbour build --profile NAME` or
`harbour test --profile NAME`. `--release` is exactly `--profile release`;
the two conflict rather than one silently winning. A name no profile
declares is an error listing the ones that exist, not a build under a
directory named after the typo.

#### Inheritance

`debug` and `release` are the **roots**: they carry Harbour's built-in
defaults (`opt_level` `0`/`3`, `debug` `2`/`0`) and must not set `inherits`.
Every other profile **must** set `inherits`, naming a root or another
declared profile. Chains may be any depth; a cycle is an error.

Harbour does not guess a base, for the same reason Cargo does not: the guess
decides `opt_level`, the MSVC debug runtime and the CMake build type, and
nothing in the build output would say which base you got. `[profile.asan]
sanitizers = ["address"]` with an implicit `debug` base would be a
sanitizer build at `-O0`; with an implicit `release` base it would have no
debug information. Both are plausible; neither is inferable.

Merging along the chain:

- **Scalars replace.** `opt_level`, `debug` and `lto` from the more derived
  profile win.
- **Lists append.** `cflags`, `ldflags` and `sanitizers` accumulate
  root-first. `[profile.asan] inherits = "release"` with `cflags = ["-DX"]`
  gets `[profile.release]`'s `cflags` *and* `-DX`. Replacing would mean an
  inheriting profile could not add one flag without restating every flag its
  ancestor set, which is how an ancestor's flag silently disappears.

A profile is **release-like** if its `inherits` chain ends at `release`.
That, not `name == "release"`, is what decides the MSVC debug runtime, the
CMake build type and the vcpkg triplet — so `[profile.asan] inherits =
"release"` gets release's runtime, as its author intended.

Each profile gets its own output directory (`.harbour/target/asan/...`), so
two profiles never share a fingerprint cache.

#### Whose profile wins

**The package being built.** Profiles are read from the workspace root only
and a dependency's `[profile.*]` is ignored, exactly as in Cargo. Harbour
links one copy of each library, so there is no per-package profile to have:
a dependency setting `opt_level = "0"` would be setting it for the whole
graph, which is not something a consumer running `--release` would expect or
be told about.

Unlike before, the dependency author *is* told: building a package whose
dependency declares a profile warns and names the tables it ignored.
Rejecting would make a package unusable as a dependency for describing its
own standalone build; merging would hand a dependency control over the
consumer's codegen.

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
include_dirs = ["arch/linux-x86_64"]   # hand-written, per-platform headers
```

`defines` here is the *only* place to express a fact no probe kind can
measure — function arity, a compile-time predicate, anything needing the
target to run. Keyed on a platform, in the manifest, where a reviewer can
see it is a human assertion rather than a measurement. `ci/canary/curl/`
uses it for exactly five such answers, next to 108 measured ones.

`include_dirs` here is for hand-maintained headers that differ per platform.
It used to be the way to point at a *vendored, configure-generated*
`config.h` per (os, arch); do not do that any more — declare
`[targets.NAME.probes]` and let Harbour measure and generate the header, so
there is no per-platform file to harvest, review or keep in step. Use
`include_dirs` rather than putting `-I` in `cflags`: a bare relative `-I`
resolves against the process working directory, which is the *root*
package's directory when this package is a dependency, so it silently finds
nothing. Paths in `include_dirs` resolve against the package's own root.

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

Five, all implemented. See
`docs/superpowers/specs/2026-09-11-native-probes-design.md`.

| kind | question | how | links? |
|------|----------|-----|--------|
| `header` | does `#include <X>` compile? | one compile | no |
| `symbol` | does `X` exist and resolve? | one compile **and link** | yes |
| `type` | does type `T` (optionally, member `T.m`) exist? | one compile | no |
| `constant` | does `X` exist as a compile-time integer constant? | one compile | no |
| `sizeof` | what is `sizeof(T)`? | 7 compiles, binary search on a compile-time predicate | no |

**Every probe kind is answerable when cross-compiling, and every answer is a
fact about the *target*.** Both halves decide which kinds exist. The second
half is why there is no `flag` kind: "does this compiler accept `-Wno-X`" is
answerable while cross-compiling and is *not* a fact about the target, so its
answer has no business in a config header. See below. Nothing is ever
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
| `check_symbols = ["strerror_r"]` | `HAVE_STRERROR_R` |
| `check_sizeof = ["long long"]` | `SIZEOF_LONG_LONG` |
| `check_sizeof = ["void *"]` | `SIZEOF_VOID_P` |
| `check_types = ["struct timeval"]` | `HAVE_STRUCT_TIMEVAL` |
| `check_constants = ["O_NONBLOCK"]` | `HAVE_O_NONBLOCK` |

`void*` and `void *` both give `SIZEOF_VOID_P`, so a define name cannot depend
on whitespace. (autoconf gives `SIZEOF_VOIDP` for `void*`; this deliberately
differs.)

#### `symbol`, and why it links

```toml
[targets.mylib.probes]
check_symbols = ["strerror_r", "gettimeofday", "poll", "sendmmsg"]
```

`symbol` is the only kind that **links**, and it has to. A header that
*declares* something the libc does not *provide* is the classic `configure`
trap, and a compile-only check answers `yes` to every one of them --
producing a package that configures cleanly and then fails at link time on a
symbol in a file nobody wrote.

Three cases are handled deliberately:

- **The symbol is a macro.** On macOS `htonl` is a macro and
  `<arpa/inet.h>` declares no function of that name, so taking its address
  does not compile. The probe tests `#if defined(name)` first, so a macro
  answers `yes` -- it is callable, and there is nothing to link. Without
  this, `HAVE_HTONL` is `no` on every Mac.
- **The symbol is provided but not declared.** `fdatasync` on macOS links,
  and `<unistd.h>` does not declare it. With no `prelude` the probe's own
  fallback declaration finds it (`yes`); with `prelude = ["unistd.h"]` the
  compile fails (`no`). **Both answers are correct** -- they are different
  questions, "can I call this if I declare it myself" versus "can I call it
  the way the header offers it" -- which is why `prelude` is part of the
  probe's cache key.
- **The symbol lives in another library.** `libs` puts it on the probe's
  link line, which is what subsumes autoconf's `AC_CHECK_LIB`: "is `dlopen`
  available" and "is `dlopen` available with `-ldl`" are one question asked
  twice with different `libs`, not two kinds.

```toml
[targets.mylib.probes.named.HAVE_DLOPEN]
symbol = "dlopen"
prelude = ["dlfcn.h"]
libs = ["dl"]
```

`libs` on any other kind is a **hard error**: the rest are all
answered by compiling, so there is no link line for it to reach, and a key
that parses and reaches nothing is the defect this schema keeps being
audited for.

A `symbol` probe needs a working **linker**, which is a stronger requirement
than a working compiler when cross-compiling -- it needs a sysroot with
libraries in it. See "Failure" below for what happens when that is missing;
the short version is that it is an error, never an answer.

#### `type`, and the `member` field

```toml
[targets.mylib.probes.named.HAVE_STRUCT_TIMEVAL]
type = "struct timeval"
prelude = ["sys/time.h", "time.h"]

[targets.mylib.probes.named.HAVE_SOCKADDR_IN6_SIN6_SCOPE_ID]
type = "struct sockaddr_in6"
member = "sin6_scope_id"
prelude = ["netinet/in.h"]
```

The probe declares a variable of the type, so the answer means "this type is
complete and usable here" rather than "a name like that was mentioned": a
forward-declared `struct foo;` with no definition in scope answers `no`,
which is the useful answer.

With `member` it asks whether a struct or union has a field, and correctly
answers **no** for a type that exists *without* it. `member` is a separate
field rather than `type = "struct sockaddr_in6.sin6_scope_id"`, because
parsing a C type expression out of a TOML string is the beginning of a
language. It is a hard error on any other kind.

One limitation, recorded rather than worked around: `sizeof` does not apply
to a bit-field, so a `member` naming one answers `no` for a field that is
really there.

#### `constant`, and why it is not `symbol`

```toml
[targets.mylib.probes.named.HAVE_FCNTL_O_NONBLOCK]
constant = "O_NONBLOCK"
prelude = ["fcntl.h"]
```

This is the `O_NONBLOCK` / `FIONBIO` / `CLOCK_MONOTONIC` question: does this
*name* exist as a compile-time integer constant. The probe uses it where only
an integer constant expression is legal — an enumerator's initialiser — and
that is the point rather than an implementation detail:

- a **macro** expanding to an integer constant answers `yes`;
- an **enumerator** answers `yes`, even where it is not also a macro, which a
  `symbol` probe's `#if defined(...)` branch cannot see;
- a **function or variable** of that name answers **no**, because neither is
  a constant expression. That is what keeps `constant` from quietly becoming
  a compile-only `symbol` check.

Why it is not just a `symbol` probe, since in practice a `symbol` probe
happens to answer most constant questions correctly through that macro
branch: `symbol` **links**, and requiring a link to answer a compile-only
question is a strictly stronger demand on the toolchain — a cross target
with a compiler and no sysroot can answer `constant` and cannot answer
`symbol`. `symbol` also accepts `libs`, which a macro has no use for.

It asks about *integer* constants. A string macro or a floating-point limit
answers `no`; the kind is named for the question it answers rather than
widened until it answers nothing precisely.

#### There is no `flag` kind

`check_flags = [...]` and `flag = "-Wno-unused"` are **hard errors**. The kind
existed, worked, and was removed; the full argument is §11 of the design
document, and the short version is:

- A `flag` answer arrived as `#define HAVE_FLAG_WNO_UNUSED 1`. Every other
  kind records a fact about the *target*, which is what makes the generated
  header diffable against autoconf's or CMake's output; this one recorded a
  fact about the *compiler*, in the same file. A define named after the
  compiler's flag table invites a package to `#ifdef` on it, which is not
  what anyone writing a flag check means. autoconf checks flags constantly
  and never puts one in `config.h`.
- What a flag check is *for* is putting the flag on the compile line, and
  probe answers cannot do that. An `emit = "cflags"` mode would have been the
  fix, and it was not built because it has no consumer either: there is
  exactly one conditional compile flag in all seven canary packages
  (`-Wa,--noexecstack` for openssl, under `os = "linux"`), and it is a
  property of the ELF object format rather than of flag acceptance — measured,
  apple-clang accepts it silently, so a probe would answer `yes` on macOS and
  the emit mode would put it on the Darwin compile line, which is exactly what
  that `when` block exists to prevent.

**If you want a flag conditionally, name it in the manifest.** `cflags` under
a `[[targets.NAME.when]]` block keyed on `os`/`arch` is visible to a reviewer
as the assertion it is, which is what the escape hatch for un-probeable
questions is everywhere else in this subsystem.

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

- Exactly one of `header`, `symbol`, `type`, `constant` or `sizeof` per
  probe. Two is an error; none is an error.
- `member` is accepted only on a `type` probe and `libs` only on a `symbol`
  probe. Each is a hard error where it does not apply, rather than being
  parsed and ignored. `prelude` applies to every kind.
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

#### Emitting a generated header

A flag list cannot express 253 answers, and curl and openssl both `#include` a
config header **by name** — so no arrangement of `-D` serves them.

```toml
[targets.curl.probes]
emit = { header = "curl_config.h" }

# Build options, not measurements. Of curl's 253 config lines, 98 are this
# kind of thing.
defines = ["CURL_DISABLE_LDAP=1", "CURL_OS=\"harbour\"", "CURL_STATICLIB"]

check_headers = ["sys/socket.h", "poll.h"]
check_symbols = ["strerror_r", "sendmmsg"]
check_sizeof = ["long", "size_t", "void *"]
```

Harbour writes the file into the build tree and puts its directory **first**
on the target's private include path:

```
.harbour/target/<triple>/<profile>/probe/<pkg>-<ver>/<target>/include/curl_config.h
```

```c
/* Generated by Harbour. Do not edit. */
/* target:    curl/curl */
/* triple:    aarch64-apple-darwin */
/* toolchain: apple-clang-21.0 */
#ifndef HARBOUR_PROBE_CURL_CONFIG_H
#define HARBOUR_PROBE_CURL_CONFIG_H

/* Declared in Harbour.toml, not measured. */
#define CURL_DISABLE_LDAP 1
#define CURL_OS "harbour"
#define CURL_STATICLIB 1

/* Measured from the toolchain. */
#define HAVE_SYS_SOCKET_H 1
#define HAVE_POLL_H 1
#define HAVE_STRERROR_R 1
/* #undef HAVE_SENDMMSG */
#define SIZEOF_LONG 8

#endif /* HARBOUR_PROBE_CURL_CONFIG_H */
```

Seven properties of that are deliberate:

- **In the build tree, never the source tree.** Probing must not dirty a
  vendored checkout, and a git-sourced package's tree is shared between
  builds.
- **Under the triple and profile**, so two triples built from one checkout
  cannot stomp each other's config. This is the `SIZEOF_LONG 8` on 32-bit
  Linux bug, prevented structurally rather than remembered.
- **The directory goes first on the include path.** `-I` is
  first-match-wins, so a package that still vendors a `config.h` of the same
  name gets the generated one. That is the migration path off the vendored
  file: add the probes, and the stale copy stops being reachable.
- **A false answer is a commented `/* #undef NAME */`**, matching autoconf
  and CMake. Not decoration — it records that the question was *asked and
  answered no*, which is what distinguishes this file from a header that
  forgot something, and what lets a reader diff it against a vendored copy.
  It is never `#define NAME 0`, which `#ifdef NAME` would accept.
- **Literals come first**, in declaration order, so a probed answer cannot
  be shadowed by a declared one arriving later in the same file. `defines`
  is a separate list from the probes for a reason: keeping what the packager
  *chose* apart from what the toolchain *reported* is what makes the
  `/* #undef */` lines meaningful.
- **The include guard is prefixed** (`HARBOUR_PROBE_`). `CURL_CONFIG_H` is a
  guard the package's own vendored copy may already define, and a header
  whose guard is already defined expands to nothing at all — a build that
  then fails on missing macros without ever mentioning this file.
- **Byte-identical across clean builds**, and only rewritten when the
  content changes. The file's bytes are a compile fingerprint input, so a
  line that moved between runs would recompile every translation unit that
  includes it, on every build, forever — 196 sources for curl.

When a header is emitted the answers do **not** also arrive as `-D` flags.
Two sources for one fact is the defect shape this schema keeps being audited
for; `emit` selects one or the other.

`defines` without `emit = { header = ... }` is a **hard error**: without a
header they are ordinary compile defines, which `[targets.X.private] defines`
already spells, and a second spelling invites the reader to look for a
difference that is not there.

`emit = "defines"` is the explicit form of the default. `emit = "header"` is
an error — a header needs a name — and so is a typo inside the table
(`emit = { headr = "x.h" }`).

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
`.harbour/<...>/probe/<package>-<version>/<target>/probes.json`, keyed on the
toolchain fingerprint (the same one that keys compile fingerprints), a hash of
the pre-probe compile surface, and a per-probe hash of the probe's own spec. A
warm rebuild spawns no compiler for probes at all; changing the compiler, the
target triple, the include path, the C dialect or one probe's declaration
re-measures what it must. The cache is discarded wholesale when a key differs
rather than merged, so it can never hold answers from two toolchains at once.

**The cache is keyed on what Harbour declares, not on the contents of the
filesystem.** A header appearing or disappearing behind an include path that
was *already* on the list does not invalidate anything:

```
build 1, the header is absent       HAVE_X undefined
build 2, you create the header      HAVE_X still undefined   <- stale
build 3, harbour clean --probes     HAVE_X defined
```

This is a limitation rather than a bug, and it is not fixable by caching more.
The input to a *negative* answer is the **absence** of a file, so there is no
finite set of paths to watch — you would have to watch every directory on
every search path for every name no probe found. The only alternative is
re-running every probe on every build (199 compiler spawns for curl, on every
`harbour build`). CMake's `CMakeCache.txt` and autoconf's `config.cache` make
the same trade.

```sh
harbour clean --probes    # re-measure probes, keep compiled objects
```

`--probes` exists so the way out is not `clean --all`: it removes only the
probe caches, so the next build re-measures every answer and reuses every
object. Fixing one wrong `#define` should not cost a full rebuild.

#### Failure

There are three outcomes, and two of them are answers:

- the compiler exits 0 — **yes**
- the compiler exits non-zero — **no**, a real answer
- the compiler could not be run, or died on a signal — **error**, not an
  answer

Before any probe runs, Harbour compiles `int main(void) { return 0; }` with
exactly the flags probes use, and — if the target declares any `symbol`
probes — **links** it too. If either fails, the build stops and quotes the
compiler or linker. Without this check a broken toolchain or a missing
sysroot would make every probe answer `no`, and the package would configure
itself for a machine that does not exist and then compile — the classic
catastrophic `configure` failure.

The link half is conditional on purpose. A cross toolchain that can compile
but not link is common and usable, and a package whose probes are all
`header` and `sizeof` is perfectly answerable on one; refusing it would be
wrong. The same toolchain cannot answer a single `symbol` probe, and letting
it try would report every `HAVE_<function>` as `no` — for curl, 107 of its
157 real questions, yielding a config that claims the platform has no
sockets and no `poll`, and which then compiles.

Asking for the size of a type that does not exist is an error, not `0`: a
`#define SIZEOF_FOO 0` is indistinguishable from a real answer. `sizeof` is
bounded at 64 bytes.

#### Not yet implemented

- Emitting defines **and** a header at once. `emit` selects one. No package
  has needed both, and a list form would be a second spelling with no
  consumer.
- Propagating answers to dependents. See "Visibility" above.
- Passing probe answers to a `prebuild` generator. This is the gap that
  matters most in practice: it is the only thing standing between openssl's
  generated `bn_conf.h` and a measured `sizeof(long)`.
- A `cflags` emit mode, which is what a `flag` probe would need to be worth
  having. See "There is no `flag` kind".

Two entries were **removed from this list as stale** rather than fixed, and
both had been true when written:

- *"`type` and `flag` probe kinds"* — `type` shipped (it is in the table
  above and `the_type_and_constant_kinds_...` builds a consumer that uses
  it), and `flag` shipped and was then removed.
- *"MSVC is unverified"* — `header`, `sizeof` and `symbol` are now measured
  live on `windows-latest` by
  `msvc_answers_every_probe_kind_correctly`, including that `link.exe`
  resolves a CRT symbol from `cl`'s embedded `/DEFAULTLIB` directives with
  no library on the link line. What that test does **not** cover, despite
  its name: `type` and `constant`. Treat those two as unverified on MSVC.
  `libs = ["m"]` is a known defect there — `m.lib` does not exist, so a
  `symbol` probe with `libs` answers `no` for the wrong reason — and it has
  an `#[ignore]`d test that demonstrates it.

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

A generator is given **only** the `env` the block declares. Unlike a
`recipe`'s custom step, it receives no `HARBOUR_ARTIFACT_DIR`, no
`HARBOUR_PACKAGE_ROOT` and no target triple, so the only way it learns
anything about the platform is the `when` block that selected it (below).
That matters for real generators: openssl's x86_64 perlasm scripts run
`$ENV{CC}` to decide which instruction encodings the assembler accepts, and
with `CC` unset they emit half the file — no AVX2, no SHA extensions —
which still assembles, links and computes correct digests. Set `env`
explicitly rather than relying on inheritance.

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

**`[targets.NAME.backend]` is not part of the schema.** It is documented
here so the error is findable rather than surprising.

```toml
# Rejected. Use `[targets.mylib.recipe]` (below) or `--backend`.
[targets.mylib.backend]
backend = "cmake"
```

This table was **removed rather than implemented**, which is the decision
[#107](https://github.com/aryamurray/harbour/issues/107) asked for. It
promised "build *this target* with cmake/meson", which
`[targets.NAME.recipe]` already does — and `recipe` is the better of the
two spellings, not merely the incumbent:

- `recipe` is an internally-tagged enum, so `type = "cmake"` gets cmake's
  own option keys checked. `backend` carried `options: toml::Table`, opaque
  by construction, which could not reject a meson option on a cmake build.
- `--backend` (and `.harbour/config.toml`) already covers "use this backend
  for the whole build".

So the table added no expressiveness and one more place for a field to have
two readers that drift apart. What it actually did was *look* live: it
validated its backend name — `backend = "nonesuch"` was an error listing
the valid backends — while nothing read the result, so
`backend = "cmake"` built natively and reported `Finished debug [native]`.

The rejection is hand-written rather than left to the generic unknown-key
error, because the hint (use `recipe`) is the whole value of the message.

One thing #107 raised that is *not* closed by this: dispatching `meson` and
`custom` shims through `harbour verify`. `build_package` refuses a shim
whose backend is neither `native` nor `cmake`, so the gap is loud, but it
is still a gap — and it is about the **shim** schema in `tools/harvest`,
which is a different table from the manifest one removed here.

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
- **`--profile` reaches `build`, `test` and `flags`.** `harbour linkplan`
  and `harbour verify` still take `--release` or a fixed profile. They read
  the same `resolved_profile`, so a named profile is not *wrong* there — it
  simply cannot be asked for.
- **`[targets.NAME.backend]` has been removed from the schema**, not
  deferred: it was a second and weaker spelling of `[targets.NAME.recipe]`,
  which already dispatches per target *and* checks its per-backend option
  keys. See "Backend Configuration" above for the full argument
  ([#107](https://github.com/aryamurray/harbour/issues/107)).
- **`workspace = true` does not resolve for the member the resolver uses as
  its root.** `Package::summary` builds the root's dependency list with
  `DependencySpec::to_dependency`, which has no workspace context, so a bare
  `{ workspace = true }` entry reaches the "must specify `path`, `git`,
  `registry`, `vcpkg`, or `version`" error rather than inheriting.
  `resolve_dependency` — which *does* inherit — is used for the seeded
  direct dependencies, so a non-root member's entry works. Reproduced by
  running a two-file workspace; predates optional dependencies and is
  unchanged by them, but it is why the `optional`-on-the-member rule above
  cannot be demonstrated end to end in a single-member workspace
  ([#133](https://github.com/aryamurray/harbour/issues/133)).
- **A path dependency's own manifest is not part of the lockfile hash.**
  `compute_workspace_hash` covers the workspace members' manifests and
  `[workspace.dependencies]`; a *path dependency's* `[dependencies]` or
  `[features]` can change without the lockfile looking stale. This predates
  optional dependencies and is unchanged by them, but optional dependencies
  make it reachable in one more way: adding `optional = true` inside a path
  dependency does not re-resolve the consumer until something else does.
- **A registry index record carries no `features`.** `IndexDependency` has
  `optional` and `default_features` but no `features` list, and
  `IndexRecord` has no `[features]` table at all, so a dependency's
  `features = [...]` does not survive index generation
  (`RegistrySource::dependency_from_index` reconstructs everything else).
  Feature unification is unaffected today, because it reads dependents'
  *manifests* rather than their index records — but the two views of the
  same field disagree, which is the shape of defect this schema keeps
  producing.
- **`[targets.NAME.ffi]` accepts only `header_files`, and stays that way
  for now.** The other nine keys (`languages`, `bundler`, `output_dir`,
  `include_functions`, `exclude_functions`, `include_types`,
  `exclude_types`, `strip_prefix`, `async_wrappers`) parsed and reached
  nothing — `harbour ffi generate` takes those from the command line, and
  for the four filtering keys there is no flag either, because binding
  filtering is not implemented in any form. They are hard errors naming the
  flag to pass instead.

  Making the table the source of defaults is deliberately **not** done yet,
  and the ordering is the reason: six of the nine keys are a cheap
  "manifest value is the flag's default", but the four filtering keys need a
  generator that does not exist, so implementing the table first would
  deliver six working keys and four that still do nothing — the same defect,
  smaller. Do the generator work first, then wire the table. Tracked in
  [#109](https://github.com/aryamurray/harbour/issues/109).

  The related defect *has* been fixed: `--lang python`, `--lang csharp` and
  `--lang rust` parsed the headers, printed "not yet implemented", wrote no
  files — not even the output directory — and exited **0**. In CI the exit
  code is the only thing read, so that was a green binding-generation step
  that generated no bindings. They now exit non-zero. That fix is also what
  makes deferring the table the right call: `languages = ["python"]` as a
  default would have turned "I typed `--lang python`" into "my manifest says
  python and the build is green".
- **`[package]` metadata is metadata.** `license`, `authors`,
  `repository`, `homepage`, `documentation`, `keywords` and `categories` are
  parsed and have no readers — not even registry index generation, which
  copies none of them. `description` alone is used, for pkg-config's
  `Description:`. This is deliberate and stays: a field whose whole purpose
  is to describe the package to a human cannot mislead anyone about what the
  build did, which is what separates it from the settings rejected above.
- **`prebuild` steps are not fingerprinted.** They run on every build,
  regardless of `inputs` and `outputs` — `outputs` is checked, `inputs` is
  advisory. Confirmed by running three builds with nothing changed and
  watching the generator run three times.
- **Unknown keys are rejected, but not everywhere by serde.**
  `deny_unknown_fields` is silently ignored on an internally tagged enum,
  which is why `[targets.NAME.recipe]` has a hand-written key check
  (derived from the enum, so it cannot drift), and it produces a useless
  message through an `untagged` enum, which is why `[dependencies]` entries
  and `[targets.NAME.deps]` entries collect unknown keys and reject them by
  hand. If you add a type to the schema, check which of the three cases it
  is rather than assuming the attribute did the job.
- **`surface.compile.requires_cpp` and `[features]` are implemented but not
  described here.** `requires_cpp` raises the graph-wide C++ standard;
  `[features]` works as Cargo's does, including `dep/feature`, `dep:name`
  and the implicit feature an optional dependency defines (described under
  [dependencies] above).

## See Also

- [README.md](README.md) - Getting started guide
- [CLI documentation](README.md#usage) - Command reference
