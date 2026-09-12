# Where `expected.json` came from, and what is still missing

## Regenerating the oracle

`expected.json` holds curl 8.22.0's own answers, per platform, produced by
curl's cmake with the options the committed shim was configured with — the
same options `.github/workflows/harvest.yml` uses, so the two cannot drift
apart silently.

```sh
curl -fsSLO https://github.com/curl/curl/releases/download/curl-8_22_0/curl-8.22.0.tar.gz
# sha256: d54dd598bf05927a726deb38df31c6a255ba83ff1de57c5d1464dac3ed8f44a1
tar xzf curl-8.22.0.tar.gz && cd curl-8.22.0

cmake -S . -B build \
  -DBUILD_SHARED_LIBS=OFF -DBUILD_CURL_EXE=OFF \
  -DCURL_ENABLE_SSL=OFF -DCURL_ZLIB=OFF -DCURL_USE_LIBPSL=OFF \
  -DBUILD_TESTING=OFF -DUSE_LIBIDN2=OFF -DCURL_ZSTD=OFF \
  -DCURL_BROTLI=OFF -DUSE_NGHTTP2=OFF -DCURL_USE_LIBSSH2=OFF \
  -DCURL_DISABLE_LDAP=ON -DCURL_DISABLE_LDAPS=ON

# build/lib/curl_config.h is the oracle. 793 lines, 253 answer *lines*,
# 252 distinct questions -- see the correction below.
```

Run it once per platform (macOS natively, Linux in a container) and merge the
two into `expected.json` as `{"NAME": {"mac": ..., "linux": ...}}`.

**cmake is an oracle here, never a build dependency.** It is used to obtain a
reference answer for comparison, which is the same role
`harvest.yml` already gives it. Harbour does not invoke it.

### Correction: 252 questions, not 253

Every previous document in this series — the design, and the first version of
this file — says curl's config header answers **253** questions. It answers
253 *lines*: 110 `#define` and 143 `/* #undef */`. But two of those lines
define the same name:

```c
#define CURL_EXTERN_SYMBOL __attribute__((__visibility__("default")))
/* Ensure using CURL_EXTERN_SYMBOL is possible */
#ifndef CURL_EXTERN_SYMBOL
#define CURL_EXTERN_SYMBOL
```

so there are **252 distinct questions**. This is a one-line correction with
no consequences for anything, and it is recorded because every count
downstream of it was quoted as measured.

It also makes the point the oracle exists to make: the difference was found
by parsing the file into a dictionary and counting keys, not by trusting a
figure that had been copied three times.

## How the 252 questions are partitioned

**106 are asked here**, across every probe kind curl has a use for:

| kind | count |
|---|---|
| `header` | 35 |
| `symbol` | 54 |
| `type` | 6 |
| `constant` | 6 |
| `sizeof` | 5 |
| `flag` | **0** — see below |

All 106 agree with curl on macOS/arm64 and linux/x86_64, and **13 of them
differ between the two platforms**, in both directions (ten Linux-only,
three macOS-only). Those thirteen are printed by `run.sh` on a pass as well
as on a failure, because they are the evidence: an oracle that agreed
everywhere would be satisfied by a subsystem that returned a constant.

### curl asks no `flag` questions, and that is a finding

Nothing in curl's 793-line config header is "does the compiler accept this
flag". curl *does* test compiler flags — `CMake/PickyWarnings.cmake` is
nothing else — but it uses the answers to build its own `CFLAGS`, never to
`#define` anything.

That is the shape of the problem with the `flag` kind as designed: its
answer arrives as a define, and no package wants a define. "Add `-Wno-X` if
the compiler accepts it" needs an emit mode that puts the accepted flag on
the compile line, and there is none. The kind is implemented, correct, and
inspectable through `harbour flags` and the generated header; it is not yet
*useful*, and a canary cannot pretend otherwise by inventing a question curl
does not ask.

### The 146 not asked, each for a stated reason

| count | category |
|---|---|
| 93 | **project options** — `CURL_DISABLE_*`, `CURL_CA_*`, `USE_*`, `CURL_*`. Never measurements: they are what the packager chose. Over a third of the vendored file is configuration masquerading as discovered fact, and recognising that is part of the result. |
| 28 | **optional dependencies, all disabled** in this configuration — `HAVE_LIBZ`, `HAVE_BROTLI`, the wolfSSL/mbedTLS/GSSAPI/LDAP families. Their answers follow from the feature flags, not from the toolchain. |
| 8 | **Windows and AmigaOS spellings** — `HAVE_IOCTLSOCKET*`, `HAVE_STRICMP`, `HAVE_IO_H`, `HAVE_PROTO_BSDSOCKET_H`. Probeable in principle, and they do answer `no` correctly on unix, but their *right* answer on Windows is untested here, so they are excluded rather than asserted. |
| 7 | **arity / return-type discrimination** — see below. |
| 5 | **arbitrary compile-time predicates** — see below. |
| 4 | **derived from curl's own typedefs, or literals** — `SIZEOF_CURL_OFF_T`, `SIZEOF_CURL_SOCKET_T`, `_FILE_OFFSET_BITS`, the `ssize_t` fallback typedef. Answerable, but only against curl's own headers, so they belong to the shim that builds curl rather than to a standalone question set. |
| 1 | **one run probe** — `HAVE_WRITABLE_ARGV`. Unanswerable without executing target code, permanently out of scope by the design's organising principle. curl's own cmake hardcodes it per platform, which is the same admission. |

106 + 93 + 28 + 8 + 7 + 5 + 4 + 1 = 252.

#### Arity discrimination: seven questions, and no new kind

`HAVE_FSETXATTR_5` / `_6`, `HAVE_GETHOSTBYNAME_R_3` / `_5` / `_6`,
`HAVE_GLIBC_STRERROR_R` / `HAVE_POSIX_STRERROR_R`. Seven names — the earlier
count of "five" was of families, not of defines. Each asks *which signature*
a function has, which a symbol lookup cannot answer.

**Decided: no probe kind.** Five of the seven differ between macOS and Linux,
which makes them look like the best possible argument for a kind — and is
actually the argument against one. A literal define under a
`[[targets.X.when]]` condition is *visibly* a human assertion keyed on a
platform, in the manifest, where review can see it is an assertion rather
than a measurement wearing a measurement's clothes. Seven answers do not
justify a kind, and the kind that would answer them — "compile this call with
this many arguments" — is a snippet probe with a fig leaf.

`ci/canary/curl/Harbour.toml` spells them exactly that way, and the `when`
blocks are three lines.

#### Arbitrary compile-time predicates: five, and no kind either

`HAVE_ATOMIC`, `HAVE_BUILTIN_AVAILABLE`, `HAVE_DECL_FSEEKO`,
`HAVE_TIME_T_UNSIGNED`, `HAVE_GETADDRINFO_THREADSAFE`. Each is a
`check_c_source_compiles` over a program with no declarative shape:
`((time_t)-1) > 0`, `__builtin_available(macOS 10.12, *)`, a `_Atomic`
round-trip. This is the category the design document rejects by name, and the
five are its real-world size in the hardest package on the roadmap.

`HAVE_GETADDRINFO_THREADSAFE` is worth keeping in the record separately:
there is no symbol of that name, and curl's cmake sets it from an OS
whitelist (`CMake/OtherTests.cmake:74-89`). It was caught by the oracle
comparison disagreeing, not by reading the name — which is the argument for
comparing against an oracle at all.

### Two caveats about the oracle's own honesty

- **curl hardcodes four answers on Apple.** `CMakeLists.txt:636-641` sets
  `HAVE_EVENTFD`, `HAVE_GETPASS_R`, `HAVE_WRITABLE_ARGV` and
  `HAVE_SENDMMSG` without measuring them there. Three of those are in the
  106; Harbour measures them and agrees, so the agreement is between a
  measurement and an assertion rather than between two measurements. Stated
  because "we agree with curl" means slightly less for those rows.
- **`CMake/unix-cache.cmake` would hardcode 154 answers**, including
  `HAVE_BOOL_T` — but it is only included when `_CURL_PREFILL` is on, and
  that option defaults to `${WIN32}`, i.e. off. In *this* configuration
  nothing in that file applies, so the unix answers are measured. Checked
  rather than assumed, because if it had applied the oracle would have been
  comparing Harbour's measurements against curl's guesses.

### Four questions the `constant` kind answers that were not expected to need it

The earlier partition listed four constant-existence questions. There are
**six**: `HAVE_IOCTL_SIOCGIFADDR` and `HAVE_CLOCK_GETTIME_MONOTONIC_RAW` are
the same shape and had been filed elsewhere.

One of them is why `constant` is a kind of its own rather than a `symbol`
probe: **`CLOCK_MONOTONIC` is a macro on glibc and an enumeration constant on
macOS.** A `symbol` probe's macro branch (`#if defined(...)`) sees the first
and not the second, so it would answer `yes` on Linux and `no` on macOS for
something both platforms have — a disagreement with curl that only an oracle
catches.

And the honest limitation: curl's tests are *conjunctions*. Its
`HAVE_IOCTL_FIONBIO` compiles `ioctl(0, FIONBIO, &flags)`, asking about the
function and the constant together. A `constant` probe answers the constant
half; the function half is a separate `check_symbols` entry. The agreement
above is the evidence that splitting the conjunction gives the same answer on
these two platforms. A platform with `FIONBIO` and no `ioctl` would
disagree, and no such platform is known.

## What it takes to build curl

The 106 answers here are correct on both platforms, and `ci/canary/curl/`
builds curl 8.22.0 from them plus:

1. **11 literal defines under `[[targets.X.when]]` blocks** — the seven
   arity answers, the five predicates minus the one that is also a literal,
   and `HAVE_WRITABLE_ARGV`. Every one of them is visibly an assertion.
2. **The project options**, as `probes.defines` — which is what they always
   were.
3. **A source list and include dirs** for curl's 196 sources, which is
   orthogonal to probing.

No vendored `curl_config.h`. That file was the reason
`.github/workflows/harvest.yml:76-86` had to run on each target OS, and it is
gone.
