# harvest

Derives `Harbour.toml` content for a package that already has a build system,
by reading what that build system actually compiles rather than guessing.

## Why

A native shim for a real C library needs the exact source list and defines,
per platform. openssl is 1115 objects across four separately-flagged product
groups whose `*_ASM` define sets differ; curl is 196 sources plus a 793-line
generated `config.h`. Hand-transcribing either is not realistic, and getting
it subtly wrong produces a library that builds and misbehaves.

## Use

```sh
# openssl: read its generated Makefile, once per target platform
perl ./Configure darwin64-arm64-cc no-shared no-tests no-docs no-apps
harvest.py extract-openssl --source . --os macos --arch aarch64 -o mac-arm.json

# cmake-based packages: read compile_commands.json
cmake -S . -B build -DCMAKE_EXPORT_COMPILE_COMMANDS=ON <options>
harvest.py extract-cc --file build/compile_commands.json --source . \
    --os linux --arch x86_64 --filter /lib/ -o linux-x64.json

# merge every platform into one manifest
harvest.py merge mac-arm.json linux-arm.json linux-x64.json \
    --package openssl --version 3.5.8 \
    --flatten-into openssl \
    --public-include-dir include --public-headers 'include/openssl/**/*.h' \
    -o Harbour.toml
```

## The baseline

Pass `--baseline` once per OS, harvested from a *portable* configure (openssl:
`no-asm`). The unconditional set becomes that baseline rather than the
intersection of the platform harvests, and each architecture layer `exclude`s
the generic C its assembly supersedes.

This is what makes an unharvested platform work. Intersecting was wrong in a
way that only appears there: assembly *replaces* generic C, so openssl's
`aes_core.c`, `bn_asm.c`, `camellia.c`, `chacha_enc.c` and `rc4_enc.c` are in
no asm-enabled platform's source list and an intersection drops them. An
unlisted architecture would compile 1071 files and fail to link. `when` blocks
are additive with no "else", so the fallback cannot be another layer — the
baseline itself has to be the thing that works everywhere.

Two traps, both hit while building this:

* **One baseline is not enough.** A single Configure run is portable in its
  *architecture* only and still carries that OS's sources. A Linux `no-asm`
  baseline drags in `engines/e_afalg.c`, which includes `linux/version.h` and
  breaks every other OS. Baselines are intersected, so pass one per OS.
* **Generate before harvesting the baseline.** A baseline taken from a
  merely-configured tree omitted ten template-generated `.c` files, and every
  platform falling back to it failed to link on `ossl_der_oid_*`. `merge` now
  checks the baseline on the same terms as the harvests: a baseline with holes
  is worse than none, since it is exactly what an unharvested platform relies
  on.

Verified both ways from one manifest: native aarch64 builds with assembly
(`aesv8-armx.o` present) and computes a correct SHA-256, and a cross-build to
`x86_64-apple-darwin` -- which matches no `when` block -- builds the 1082-source
portable baseline with no assembly and computes the same digest under Rosetta.

## How the merge works

Deltas are emitted in layers, general to specific, because `when` blocks are
additive — every matching block contributes. For openssl across three
platforms:

| condition | sources | defines |
|---|---|---|
| *(unconditional)* | 1071 | shared |
| `arch = "aarch64"` | 33 | 6 |
| `os = "linux", arch = "aarch64"` | 2 | 1 |
| `os = "linux", arch = "x86_64"` | 44 | 7 |
| `os = "macos", arch = "aarch64"` | 1 | 2 |

The arch layer matters: those 33 files are the arm assembly, identical on
macOS and Linux. One block per harvested platform would either duplicate them
or gate them on `os = "macos"` and silently drop them on Linux aarch64 — a
manifest that looks granular while covering only what happened to be
harvested. Adding a fourth aarch64 platform now reuses that layer.

**Which axis cuts across the platforms is a property of the package.** openssl's
is the architecture. libuv's is the *OS*: `src/unix/linux.c` and its three
companions are the same four files on every Linux architecture, and nothing
about them is arch-specific. So `os` is a layering axis on the same terms as
`arch`, and each item is attached to the coarsest condition whose harvested
platforms all want it:

| package | condition | what lands there |
|---|---|---|
| openssl | `arch = "aarch64"` | 33 arm assembly sources, 6 defines |
| libuv | `os = "linux"` | `linux.c`, `procfs-exepath.c`, `random-getrandom.c`, `random-sysctl-linux.c` |
| zstd | `arch = "aarch64"` | `ZSTD_DISABLE_ASM` |

Layering on `arch` alone emitted libuv's four Linux files twice, under
`os = "linux", arch = "x86_64"` and again under `os = "linux", arch = "aarch64"` —
which is the failure the paragraph above describes, one axis over: linux/riscv64
then matches no block, compiles the intersection, and fails to link on
`uv__platform_loop_init`, on a package that supports it.

`tools/harvest/test_layering.py` covers both shapes. Run it with
`python3 tools/harvest/test_layering.py`; it needs nothing but the standard
library.

## Rules it enforces

**Unresolved objects are an error.** The first version of this silently
dropped 49 of openssl's 1115 x86_64 objects, whose sources are generated by
perlasm at build time and did not exist yet. The harvest looked complete and
would have produced a library missing every hand-written AES, SHA and bignum
path. `merge` refuses while any harvest has them, and names the generator for
each from the Makefile's own rules.

**Generators are read, not guessed.** openssl's do not follow one layout:
`crypto/sha/sha256-x86_64.s` comes from `asm/sha512-x86_64.pl` (one script
emits both digests), `crypto/x86_64cpuid.s` from a script beside it rather
than under `asm/`, and ten `.c` files from `.c.in` templates. Every guess at
those paths was wrong for something.

**Flags come from a split argument list.** `-D` and `-I` occur inside ordinary
paths — a scratch directory named `.../-Documents-personal-code/...` matches a
naive `-D` regex — so a text scan invents defines that do not exist.

## Known limitations

Harvest from a tree where generation has already run — or pass
`--emit-prebuild` (above) and let the generated sources stay generated.
openssl's assembly is perlasm-generated on *every* platform; an early harvest
resolved 22 `.S` files only because that tree had already been built, which
made the asm look shipped when it is not. A clean tree reports them as
generated, which is correct.

`--emit-prebuild` does not close the loop by itself for openssl: the ten
`.c.in` template sources still have to be produced (with
`util/dofile.pl -i.in`) before `merge` will accept the harvest, because their
recipes need a shell. So the order is: Configure once as an oracle, generate
the template sources, harvest, merge with `--emit-prebuild`, then hand-write
the two template-generating `prebuild` steps at the top of the manifest.

The public compile surface cannot be inferred — a build system records what the
library compiles itself with, not what consumers should see — so
`--public-include-dir` and `--public-headers` are supplied by the author.

## Generated sources: `--emit-prebuild`

openssl has no source list to harvest. Its assembly is emitted by perlasm on
*every* platform, so a clean tree resolves 22 of its aarch64 objects to files
that do not exist, and `merge` refuses. `--emit-prebuild` turns each of those
into a `[[targets.NAME.when.prebuild]]` step, read from the build system's own
recipe:

```toml
[[targets.crypto.when]]
os = "macos"
arch = "aarch64"

[[targets.crypto.when.prebuild]]
program = 'perl'
args = ['crypto/sha/asm/sha512-armv8.pl', 'ios64', '-Icrypto', ..., 'crypto/sha/sha256-armv8.S']
outputs = ['crypto/sha/sha256-armv8.S']
env = { CC = 'cc' }
```

Three things about that are load-bearing:

* **The flavour comes from the recipe, not from a guess.** `ios64` on macOS,
  `linux64` on Linux, `macosx`/`elf` on x86_64 — it decides symbol
  decoration and section directives, and a macOS build handed `elf`
  assembles to objects the Mach-O linker rejects. It appears nowhere but the
  recipe, which is why prerequisites alone were not enough.
* **The generator is pinned to the exact (os, arch); the source it writes is
  layered.** openssl's 33 arm assembly files are one `arch = "aarch64"`
  block and *two* generator blocks, one per OS. Emitting both at (os, arch)
  would duplicate 33 source lines per OS; layering the generator would send
  `ios64` to Linux.
* **`CC` is recorded as plain `cc`, and absolute paths are dropped.** The
  x86_64 perlasm scripts run the assembler to decide which encodings it
  accepts, so `CC` must be set — but the harvesting machine's
  `/Library/Developer/.../clang` in a committed manifest is wrong everywhere
  else. Arguments carrying an absolute path (`-DOPENSSLDIR="/usr/local/ssl"`,
  an SDK path) are dropped with a note on stderr rather than kept.

**What it still refuses.** A recipe it cannot reproduce faithfully. openssl's
`.c.in`/`.h.in` templates end in `> $@`, and a `prebuild` step has no shell
and therefore no redirection, so there is nothing honest to emit — inventing
`sh -c` would make the manifest unportable and the tool's output a guess. The
answer for those is `util/dofile.pl -i.in FILE.in`, which writes the file
itself; the refusal now says so. Emitting *most* of the generators is the
failure this whole tool exists to avoid: the library would link and compute
correct answers, slowly, with no witness.

Verified by running, not by reading the output: all 22 emitted blocks were
executed against a pristine 3.5.4 tarball with the program, args, env and cwd
Harbour's `PrebuildStep` uses, and all 22 produced their declared outputs —
2.3 MB of aarch64 assembly, from a tree where `perl ./Configure` had never
run.

**The harvest reports the build system's answer, including its artefacts.** It
is a faithful reading, not a curated one, so the author still prunes:

* CMake's per-target-type defines come through. cJSON's shared-library build
  contributes `cjson_EXPORTS` and `CJSON_EXPORT_SYMBOLS`, which are wrong for
  the static archive a shim declares.
* CMake adds its own binary directory to the include path. It shows up in
  `external_include_dirs` (an absolute path outside the package), which is the
  right place for it — but it means "is there a generated header here?" is a
  question the author has to answer, not one the harvest answers. For curl
  there was one and it had to be vendored; for zstd, libuv and cJSON there is
  none and the entry is noise.
* A define recording what the harvested build *disabled* is usually the wrong
  thing to copy. cmake sets `ZSTD_DISABLE_ASM` on every non-x86_64 platform,
  and `merge` duly emits it under `arch = "aarch64"`; but zstd's own guard
  already tests `defined(__x86_64__)`, so the correct manifest omits the define
  entirely and adds the assembly under `arch = "x86_64"`. Enumerating the
  architectures that must disable something means the next architecture is
  silently missing from the list — `when` blocks have no "else", so the
  unconditional case has to be the one that is right everywhere.
