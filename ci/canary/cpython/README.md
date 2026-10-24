# The CPython canary

Builds CPython 3.14.9's interpreter through Harbour, with a generated module
table, a generated set of frozen modules, and an optional `zlib` extension.
`run.sh` says what it asserts; this file is about the one thing that is
vendored and how to refresh it.

## What is vendored: `pyconfig.h`, per platform

`overlay/harbour/pyconfig/<os>-<arch>/pyconfig.h` is what CPython's own
`./configure` writes for that platform, with a short section of Makefile-derived
values appended (`SOABI`, `PLATFORM`, `MULTIARCH`, `PREFIX` and friends: values
the Makefile passes with `-D` on a few files rather than writing to
`pyconfig.h`). It has 412 answers and it is the stand-in for Harbour's probe
subsystem. Probing it is a separate effort the size of curl's, deliberately not
attempted here: this canary is about generators and features.

A platform with no directory fails at the bootstrap step and names the
directory it wanted. It does not fall back to another platform's answers.

Present: `macos-aarch64`, `linux-aarch64`, `linux-x86_64`.

### Regenerating one

On the platform, in an unmodified 3.14.9 tree:

```sh
./configure --without-ensurepip
make -j python           # also produces the Makefile values below
grep -E '^(SOABI|MULTIARCH|MACHDEP)=' Makefile
```

Copy `pyconfig.h` to `overlay/harbour/pyconfig/<os>-<arch>/`, then append the
section from an existing platform's file with that platform's values:
`PLATFORM` is `MACHDEP`, `SOABI` and `MULTIARCH` are as printed. The rest
(`PREFIX`, `VERSION`, …) are the same everywhere.

The Linux files were produced in `ubuntu:26.04` containers, one per
architecture. Two things came out identical across macOS arm64, Linux aarch64
and Linux x86_64 and are therefore asserted once: the 24 frozen-module headers
(`expected-frozen.sha256`) and the built-in module table.

## What differs per platform

| | macOS | Linux |
|---|---|---|
| sources | 190 | 191: `Python/asm_trampoline.S`, the perf trampoline configure enables only there |
| system libraries | `dl`, framework `CoreFoundation` | `dl`, `m` |
| `PLATFORM` / `SOABI` | `darwin` / `cpython-314-darwin` | `linux` / `cpython-314-<arch>-linux-gnu` |

## Known limits

- **Native builds only.** The bootstrap compiles host programs with the
  *target's* `pyconfig.h`, which is wrong when cross-compiling. That is the gap
  `docs/superpowers/specs/2026-10-24-host-tools-design.md` describes.
- **Changing a feature re-runs both generators.** A step's skip digest covers
  its whole environment and every generator's environment carries
  `HARBOUR_FEATURES`, so the bootstrap re-runs when `zlib` is switched on
  although it does not read it. Correct, and slower than it needs to be.
- **No installed layout.** The interpreter finds its standard library through
  `PYTHONPATH`, pointing at the source tree's `Lib/`.
