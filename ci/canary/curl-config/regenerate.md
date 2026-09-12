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

# build/lib/curl_config.h is the oracle. 793 lines, 253 answered questions.
```

Run it once per platform (macOS natively, Linux in a container) and merge the
two into `expected.json` as `{"NAME": {"mac": ..., "linux": ...}}`.

**cmake is an oracle here, never a build dependency.** It is used to obtain a
reference answer for comparison, which is the same role
`harvest.yml` already gives it. Harbour does not invoke it.

## How the 253 questions were partitioned

Of curl's 253 answered questions, **89 are asked here**:

| kind | count |
|---|---|
| `header` | 32 |
| `symbol` | 52 |
| `sizeof` | 5 |

The other 164 are excluded, each for a stated reason. The categories:

- **98 project options** — `CURL_DISABLE_*`, `CURL_CA_*` and friends. Never
  measurements at all: they are what the packager chose. A third of the
  vendored file is configuration masquerading as discovered fact, and
  recognising that is part of the result.
- **Optional dependencies, all disabled** in this configuration — `HAVE_LIBZ`,
  `HAVE_BROTLI`, the wolfSSL/mbedTLS/GSSAPI/LDAP families. Their answers are
  a consequence of the feature flags, not of the toolchain.
- **Needs the `type` kind** — `HAVE_BOOL_T`, `HAVE_SA_FAMILY_T`,
  `HAVE_SUSECONDS_T`, `HAVE_STRUCT_TIMEVAL`,
  `HAVE_STRUCT_SOCKADDR_STORAGE`, `HAVE_SOCKADDR_IN6_SIN6_SCOPE_ID`.
- **Needs constant-existence checks** — `HAVE_IOCTL_FIONBIO`,
  `HAVE_FCNTL_O_NONBLOCK`, `HAVE_CLOCK_GETTIME_MONOTONIC`,
  `HAVE_SETSOCKOPT_SO_NONBLOCK`. These test whether a *macro* is defined and
  usable in an expression, which is neither a header nor a symbol.
- **Needs arity or return-type discrimination** — `HAVE_FSETXATTR_5` vs `_6`,
  `HAVE_GETHOSTBYNAME_R_3/5/6`, `HAVE_GLIBC_STRERROR_R` vs
  `HAVE_POSIX_STRERROR_R`. Each asks *which signature* a function has, which
  a symbol lookup cannot answer.
- **One run probe** — `HAVE_WRITABLE_ARGV`. Unanswerable without executing
  target code, and therefore permanently out of scope by the design's
  organising principle. curl's own cmake hardcodes it per platform.
- **Windows and AmigaOS spellings** — `HAVE_IOCTLSOCKET*`, `HAVE_STRICMP`,
  `HAVE_IO_H`, `HAVE_PROTO_BSDSOCKET_H`. Probeable in principle, and they do
  answer `no` correctly on unix, but their *right* answer on Windows is
  untested here, so they are excluded rather than asserted.
- **One that looks exactly like a symbol check and is not** —
  `HAVE_GETADDRINFO_THREADSAFE`. There is no symbol of that name; curl's
  cmake sets it from an OS whitelist (`CMake/OtherTests.cmake:74-89`). This
  was caught by the oracle comparison disagreeing, not by reading the name,
  which is the argument for comparing against an oracle at all.

## What it would take to build curl

Not the probes. The 89 answers here are correct on both platforms. What is
missing is:

1. **The `type` kind**, for the six struct and typedef questions. curl's code
   does not compile without `HAVE_STRUCT_TIMEVAL` and friends.
2. **Constant existence**, for the four `ioctl`/`fcntl`/`setsockopt` flag
   questions.
3. **Arity discrimination**, for `gethostbyname_r` and `fsetxattr`. This one
   may not deserve a probe kind: a package needing it can express the answer
   as a literal define under a `[[targets.X.when]]` condition, which is
   visibly an assertion. Worth deciding rather than building by default.
4. A source list and include dirs for curl's 196 sources, which
   `tools/harvest` produces and which is orthogonal to probing.

Until (1) and (2) exist, curl still needs a small number of vendored answers
— but 89 of the questions no longer have to be vendored, and none of the ones
that remain is a *header*, *function* or *size*.
