# Where the manifests and `expected.json` came from, and what is not asked

`Harbour.<platform>.toml` and `expected.json` are **generated** by
`generate.py` from CPython 3.14.9's own `configure`. Edit the generator, not
the outputs.

## What is generated from what

`generate.py` reads two things:

- **`configure` and `configure.ac`** — *what is asked.* Each check's subject (a
  header path, a function, a type and member, a size) and the headers it
  includes. The answer `HAVE_SYS_SOCKET_H` does not say whether the header was
  `sys/socket.h`, and `PY_CHECK_FUNC` names the header a function must be
  declared in; a probe has to ask the real question.
- **`pyconfig.h` and `config.log` from a configured tree on each platform** —
  *the answers* (the oracle), and which questions configure actually asked
  there.

## Regenerating

```sh
curl -fsSLO https://www.python.org/ftp/python/3.14.9/Python-3.14.9.tgz
tar xzf Python-3.14.9.tgz && cd Python-3.14.9
./configure --without-ensurepip          # on each platform; keep pyconfig.h and config.log

python3 ci/canary/cpython-config/generate.py \
  --configure Python-3.14.9/configure --configure-ac Python-3.14.9/configure.ac \
  --oracle macos-aarch64=DIR_MAC --oracle linux-aarch64=DIR_LINUX_ARM --oracle linux-x86_64=DIR_LINUX_X86 \
  --varies-with-installed-libraries DIR_LINUX_MINIMAL:DIR_LINUX_WITH_DEV_PACKAGES
```

Linux oracles come from `ubuntu:26.04` containers (the distro the canary job is
pinned to — see the comment in `.github/workflows/ci.yml`): `build-essential` and
`pkg-config` only. `--platform linux/amd64` gives the x86_64 one on an arm64
host, under emulation.

`--varies-with-installed-libraries` takes two configure runs on one platform,
without and with third-party dev packages (openssl, readline, ffi, zlib, bzip2,
lzma, sqlite, ncurses, uuid, gdbm, mpdecimal, db, expat, edit, zstd). Every name
that differs between them is **environment-dependent** and is listed, with both
answers, in `expected.json`. CI's runner image has many of those packages and
a bare container has none, so those answers cannot be pinned; the canary prints
them and does not compare them.

## The three groups

`run.sh` prints the measured and asserted groups separately, and that is the
point: an oracle satisfied by literals says nothing about the probes.

| group | what | count |
|---|---|---|
| **measured** | headers (134), functions (258), custom-named checks of the five kinds (42), sizes (17), types (4), declarations (10), struct members (10) | 475 |
| **asserted** | choices, package metadata, feature-test macros (12), run-time behaviour checks (11), alignments (3), library-dependent feature tests, and the rest | 167 |
| **not asked** | the include guard, ten typedef fallbacks, one unrepresentable value | 12 |
| | | **654** |

### Asserted, and why a probe cannot ask them

| why | examples | roughly |
|---|---|---|
| a choice, an option or package metadata, not a measurement | `WITH_PYMALLOC`, `PACKAGE_VERSION`, `Py_HASH_ALGORITHM`, `THREAD_STACK_SIZE` | 54 |
| needs code to run or to be inspected (`AC_RUN_IFELSE`, `AC_COMPILE_IFELSE` over a program body) | `HAVE_BROKEN_POLL`, `HAVE_GETHOSTBYNAME_R_6_ARG`, `X87_DOUBLE_ROUNDING`, `DOUBLE_IS_LITTLE_ENDIAN_IEEE754` | 41 |
| curses features, each a function in a library configure found | `HAVE_CURSES_RESIZETERM`, `HAVE_NCURSESW` | 22 |
| readline / editline | `HAVE_RL_COMPLETION_MATCHES` | 10 |
| third-party library presence | `HAVE_LIBSQLITE3`, `HAVE_UUID_CREATE` | 10 |
| alignments | `ALIGNOF_LONG`, `ALIGNOF_MAX_ALIGN_T` | 3 |

The last four rows are probe-shaped questions that are *also* environment
dependent, which is why they are asserted rather than half-measured.

### Not asked

- **The include guard** (`Py_PYCONFIG_H`).
- **Ten typedef fallbacks** (`mode_t`, `size_t`, `uid_t`, ...). configure writes
  `#define mode_t int` only when the type is *missing*, so the oracle says
  `#undef` everywhere. Harbour cannot say "define this if absent"; defining
  nothing is right wherever the type exists. The canary asserts the generated
  header does not define them.
- **`STRICT_SYSV_CURSES`**, defined to a C comment, which no `-D` can carry.

## Differences between platforms, and between "no" and "not asked"

The canary prints the measured questions whose answers differ across the
platforms it knows, on a pass as well as a failure. Where configure never *asked*
a question on a platform (`dup3`, `pipe2` and several headers on macOS, `tzname`
where `struct tm` has `tm_zone`), an `#undef` in the oracle means "not asked",
not "answered no", and the name is skipped for that platform and counted.

## Per-platform manifests, and why

There is one manifest per platform because a `[targets.X.probes]` table cannot be
scoped by `when`: the literals `pyconfig.h` carries (`_POSIX_C_SOURCE`,
`PLATFORM`, `SOABI`, ...) differ by platform, and a `when` block's `defines` are
compile defines that never reach the generated header. The probe lists are
identical across the files and only the literals differ.

## What Harbour could not express

Each of these is a manifest or probe feature that does not exist, found while
building this canary:

1. **Platform-scoped probe literals.** `probes.defines` has no `when`.
2. **A nested member.** `member` takes one identifier, so
   `struct stat.st_mtim.tv_nsec` is asked as `st_mtim`.
3. **A function that is declared, not just linkable.** `symbol` links; configure's
   `PY_CHECK_FUNC` asks whether the function is declared in a *named* header. A
   `symbol` probe with that prelude asks the same thing, but the bulk
   `check_symbols` list has no prelude, so the ~30 such functions are named
   probes.
4. **Library presence and a function inside a library** (`AC_CHECK_LIB`,
   `HAVE_LIBDL`, the curses and readline families) as one question.
5. **Run-time and program-body checks** (`AC_RUN_IFELSE`).
6. **"Define only if missing"** for typedef fallbacks.
7. **Alignment.** `alignof(T)` needs `sizeof(struct { char c; T x; }) - sizeof(T)`,
   which is two probes and arithmetic.
