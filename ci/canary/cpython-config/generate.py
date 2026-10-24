#!/usr/bin/env python3
"""Generate Harbour.toml and expected.json for the cpython-config canary.

CPython's `./configure` asks about 650 questions and writes the answers to
`pyconfig.h`. This script reads two things from a CPython source tree that has
been configured on each platform of interest:

  * `configure` itself -- *what is asked*: each check's subject (a header
    path, a function, a type and member, a size) and the headers it includes.
    Taking these from configure rather than from the answers matters: the
    answer `HAVE_SYS_SOCKET_H` does not say whether the header was
    `sys/socket.h` or `sys_socket.h`, and a probe must ask the real question.
  * each platform's `pyconfig.h` -- *the answers*, the oracle.

and writes a manifest in which every question that one of Harbour's five probe
kinds can ask is a probe, and every other macro is a literal that says so. See
`regenerate.md` for how to run it and what each category means.

Standard library only.
"""
import argparse
import json
import re
import sys
from collections import OrderedDict
from pathlib import Path

# ---------------------------------------------------------------------------
# Reading configure.
# ---------------------------------------------------------------------------

# `$ac_includes_default`, as autoconf expands it, minus the `#ifdef HAVE_*`
# guards -- every header here exists on every platform this canary supports.
DEFAULT_INCLUDES = [
    "stdio.h", "sys/types.h", "sys/stat.h", "stdlib.h", "stddef.h",
    "string.h", "strings.h", "inttypes.h", "stdint.h", "unistd.h",
]

STR = r'"((?:[^"\\]|\\.)*)"'


def call_args(text, start):
    """The quoted arguments of one `ac_fn_...` call that begins at `start`.

    Arguments are separated by spaces and may span lines (the `includes`
    argument usually does), so this reads quoted strings in order until the
    next token is not one.
    """
    pos, args = start, []
    while True:
        m = re.compile(r'[ \t]*' + STR).match(text, pos)
        if not m:
            # a bare "$ac_includes_default" is a quoted string too; anything
            # else (a newline, a `;`) ends the call
            break
        args.append(m.group(1))
        pos = m.end()
    return args


def includes_of(arg):
    """Header names a check's `includes` argument pulls in.

    A header under `#ifdef HAVE_X_H` is returned as `header?HAVE_X_H`: configure
    includes it only where it exists, and a probe prelude cannot be
    conditional, so the manifest for each platform keeps or drops it by that
    platform's own answer.
    """
    out = []
    if "$ac_includes_default" in arg:
        out.extend(DEFAULT_INCLUDES)
    guards = []
    for line in arg.splitlines():
        stripped = line.strip()
        m = re.match(r'#\s*ifdef\s+(\w+)', stripped)
        if m:
            guards.append(m.group(1))
            continue
        if re.match(r'#\s*(?:ifndef|if)\b', stripped):
            guards.append(None)
            continue
        if re.match(r'#\s*endif\b', stripped):
            if guards:
                guards.pop()
            continue
        for h in re.findall(r'#\s*include\s*<([^>]+)>', stripped):
            # `#include <$ac_cv_struct_tm>` is `time.h` or `sys/time.h`,
            # whichever configure found; both platforms here have `time.h`.
            h = "time.h" if h == "$ac_cv_struct_tm" else h
            guard = next((g for g in reversed(guards) if g), None)
            out.append(f"{h}?{guard}" if guard else h)
    seen, uniq = set(), []
    for h in out:
        if h not in seen:
            seen.add(h)
            uniq.append(h)
    return uniq


def read_py_check_funcs(path):
    """`PY_CHECK_FUNC(fn, [includes], [macro])` calls in configure.ac.

    CPython checks about thirty functions its own way: not by linking, but by
    compiling `void *x = fn;` after the *named* headers, so the answer is "is it
    declared where CPython expects". A `symbol` probe with that prelude asks the
    same question; one without it would link a function the header never
    declares.
    """
    t = Path(path).read_text().replace("@%:@", "#")  # m4's quadrigraph for `#`
    out = {}
    for m in re.finditer(r'PY_CHECK_FUNC(?:_PRIVATE)?\(', t):
        depth, i = 1, m.end()
        while depth and i < len(t):
            depth += {"(": 1, ")": -1}.get(t[i], 0)
            i += 1
        body = t[m.end():i - 1]
        head = re.match(r'\s*\[?([A-Za-z_]\w*)\]?\s*[,)]', body)
        if not head or head.group(1) in ("FUNCTION",) or t[max(0, m.start() - 4):m.start()] == "dnl ":
            continue  # the macro's own definition, or a comment about it
        fn = head.group(1)
        priv = "PRIVATE" in t[m.start():m.end()]
        macros = re.findall(r',\s*\[(\w+)\]\s*$', body.strip())
        macro = macros[0] if macros else (("_Py_HAVE_" if priv else "HAVE_") + fn.upper())
        out[macro] = (fn, includes_of(body))
    return out


def read_configure(path):
    t = Path(path).read_text()
    found = {"header": [], "decl": [], "type": [], "member": [], "sizeof": []}

    for m in re.finditer(r'ac_fn_c_check_header_compile "\$LINENO" ', t):
        a = call_args(t, m.end())
        if a:
            found["header"].append(a[0])
    # `for ac_header in a.h b.h \\\n c.h\ndo :` -- configure checks most
    # headers in loops, and the standard autoconf ones (`stdio.h`...) too.
    # autoconf's own list (`stdio.h`, `stdlib.h`, `unistd.h`, ...), as triples:
    # header, mangled name, macro.
    for m in re.finditer(r'as_fn_append ac_header_c_list " (\S+) \S+ HAVE_\S+"', t):
        if m.group(1) not in found["header"]:
            found["header"].append(m.group(1))
    for m in re.finditer(r'for ac_hdr in ([^;\n]+); do', t):
        for h in m.group(1).split():
            if h not in found["header"]:
                found["header"].append(h)
    for m in re.finditer(r'for ac_(?:header|hdr) in ((?:[^\n]*\\\n)*[^\n]*)\n\s*do', t):
        for h in m.group(1).replace("\\\n", " ").split():
            if h not in found["header"]:
                found["header"].append(h)
    for m in re.finditer(r'ac_fn_check_decl "\$LINENO" ', t):
        a = call_args(t, m.end())
        if len(a) >= 3:
            found["decl"].append((a[0], includes_of(a[2])))
    for m in re.finditer(r'ac_fn_c_check_type "\$LINENO" ', t):
        a = call_args(t, m.end())
        if len(a) >= 3:
            found["type"].append((a[0], includes_of(a[2])))
    for m in re.finditer(r'ac_fn_c_check_member "\$LINENO" ', t):
        a = call_args(t, m.end())
        if len(a) >= 4:
            found["member"].append((a[0], a[1], includes_of(a[3])))
    # AC_CHECK_SIZEOF: `ac_fn_c_compute_int LINENO "(long int) (sizeof (T))" VAR
    # INCLUDES` -- the type, and the includes it is asked under.
    sizeofs = {}
    for m in re.finditer(r'ac_fn_c_compute_int "\$LINENO" "\(long int\) \(sizeof \(([^)]*)\)\)"', t):
        a = call_args(t, m.end() - 0)
        # the remaining quoted args after the expression: VAR, INCLUDES
        rest = call_args(t, m.end())
        sizeofs.setdefault(m.group(1), includes_of(rest[1]) if len(rest) > 1 else [])
    found["sizeof"] = list(sizeofs.items())
    return found


# ---------------------------------------------------------------------------
# Reading an oracle.
# ---------------------------------------------------------------------------

def read_pyconfig(path):
    """name -> value string (defined) or None (`/* #undef NAME */`)."""
    out = OrderedDict()
    for line in Path(path).read_text().splitlines():
        # `# define` too: autoconf writes feature-test macros as
        #   #ifndef _GNU_SOURCE
        #  define _GNU_SOURCE 1
        #   #endif
        # and a parser that only saw `#define` at the start of a line missed
        # every one of them -- including the one that makes glibc declare
        # `memfd_create`.
        m = re.match(r'^#\s*define\s+([A-Za-z_]\w*)(?:[ \t]+(.*))?$', line)
        if m:
            out[m.group(1)] = (m.group(2) or "").strip()
            continue
        m = re.match(r'^(?:/\*\s*)?#\s*undef\s+([A-Za-z_]\w*)\s*(?:\*/)?\s*$', line)
        if m:
            out.setdefault(m.group(1), None)
    return out


def read_cache(path):
    """`ac_cv_*` variables from config.log."""
    log = Path(path).read_text(errors="replace")
    return dict(re.findall(r"^(ac_cv_[A-Za-z0-9_]+)='?([^'\n]*)'?$", log, re.M))


def upper(name):
    return re.sub(r'[^A-Za-z0-9]+', '_', name).strip('_').upper()


def mangle(name):
    return re.sub(r'[^A-Za-z0-9]', '_', name)


# ---------------------------------------------------------------------------
# Classifying every name in the oracle.
# ---------------------------------------------------------------------------

INCLUDE_GUARD = {"Py_PYCONFIG_H"}

# Names whose value is not a value: `STRICT_SYSV_CURSES` is defined to a C
# comment, which no `-D` can carry.
UNEXPRESSIBLE = {
    "STRICT_SYSV_CURSES": "defined to a C comment (`/* Don't use ncurses "
                          "extensions */`); a `-D` cannot carry a comment",
}

# Typedef fallbacks. configure writes `#define mode_t int` only when the type
# is *missing*, and on every platform here it is not, so the oracle says
# `#undef`. Harbour has no way to say "define this if absent", and defining
# nothing is correct wherever the type exists, so these are asserted absent.
TYPEDEF_FALLBACKS = {"clock_t", "const", "gid_t", "mode_t", "off_t", "pid_t",
                     "signed", "size_t", "socklen_t", "uid_t", "ssize_t"}

# Checks of exactly the five kinds, under macro names configure chose itself
# (`AC_DEFINE(HAVE_SOCKADDR_STORAGE)`) rather than the default `HAVE_<NAME>`
# pattern. (kind, subject, member, prelude). Each is verified against the
# oracle on every platform; one that disagrees belongs in the literals, with
# the reason, not here.
CUSTOM = {
    "HAVE_ADDRINFO": ("type", "struct addrinfo", None, ["sys/types.h", "sys/socket.h", "netdb.h"]),
    "HAVE_SOCKADDR_STORAGE": ("type", "struct sockaddr_storage", None, ["sys/types.h", "sys/socket.h"]),
    "HAVE_SOCKADDR_ALG": ("type", "struct sockaddr_alg", None, ["sys/types.h", "sys/socket.h", "linux/if_alg.h"]),
    "HAVE_SOCKADDR_SA_LEN": ("type", "struct sockaddr", "sa_len", ["sys/types.h", "sys/socket.h"]),
    "HAVE_DIRENT_D_TYPE": ("type", "struct dirent", "d_type", ["sys/types.h", "dirent.h"]),
    "HAVE_STAT_TV_NSEC": ("type", "struct stat", "st_mtim", ["sys/types.h", "sys/stat.h"]),
    "HAVE_STAT_TV_NSEC2": ("type", "struct stat", "st_mtimespec", ["sys/types.h", "sys/stat.h"]),
    "HAVE_TM_ZONE": ("type", "struct tm", "tm_zone", ["sys/types.h", "time.h"]),
    "HAVE_TZNAME": ("symbol", "tzname", None, ["time.h"]),
    "HAVE_ALTZONE": ("symbol", "altzone", None, ["time.h"]),
    "HAVE_LONG_DOUBLE": ("type", "long double", None, []),
    "HAVE___UINT128_T": ("type", "__uint128_t", None, []),
    "HAVE_DIRFD": ("symbol", "dirfd", None, ["sys/types.h", "dirent.h"]),
    "HAVE_MAXLOGNAME": ("symbol", "MAXLOGNAME", None, ["sys/param.h"]),
    "HAVE_UT_NAMESIZE": ("symbol", "UT_NAMESIZE", None, ["utmp.h"]),
    "HAVE_GETC_UNLOCKED": ("symbol", "getc_unlocked", None, ["stdio.h"]),
    "HAVE_EPOLL": ("symbol", "epoll_create", None, ["sys/epoll.h"]),
    "HAVE_MAKEDEV": ("symbol", "makedev", None, ["sys/types.h", "@sys/sysmacros.h"]),
    "HAVE_CHFLAGS": ("symbol", "chflags", None, ["sys/stat.h", "unistd.h"]),
    "HAVE_LCHFLAGS": ("symbol", "lchflags", None, ["sys/stat.h", "unistd.h"]),
    "HAVE__GETPTY": ("symbol", "_getpty", None, ["sys/types.h", "stdlib.h"]),
}

# Custom-named checks that configure runs only when an earlier one failed,
# and the cache variable that proves it ran: `AC_STRUCT_TIMEZONE` looks for
# `tzname` only if `struct tm` has no `tm_zone`.
CUSTOM_CACHE = {"HAVE_TZNAME": "ac_cv_var_tzname"}

# Functions configure checks with `-lm` added to LIBS (configure.ac, "Python
# requires C99 compatible libm"). A `symbol` probe links only what it is told
# to, and on glibc libm is a separate library.
LIBM_FUNCTIONS = {"acosh", "asinh", "atanh", "erf", "erfc", "expm1", "log1p", "log2"}

# Feature-test macros configure puts at the top of pyconfig.h *and* compiles
# its later checks under. Their values are asserted (a `-D` before the probes
# run), because a probe asked without them is a different question: glibc
# declares `memrchr` only under `_GNU_SOURCE`.
FEATURE_MACROS = [
    "_GNU_SOURCE", "_DARWIN_C_SOURCE", "_NETBSD_SOURCE", "__BSD_VISIBLE",
    "_BSD_SOURCE", "_POSIX_C_SOURCE", "_XOPEN_SOURCE",
    "_XOPEN_SOURCE_EXTENDED", "_REENTRANT", "_POSIX_PTHREAD_SEMANTICS",
    "_ALL_SOURCE", "_LARGEFILE_SOURCE", "_FILE_OFFSET_BITS", "_LARGE_FILES",
    "_TANDEM_SOURCE", "__EXTENSIONS__", "_INCLUDE__STDC_A1_SOURCE",
]


def classify(oracle_names, cache, cfg):
    """Return {name: (category, probe spec or None)}."""
    cat = OrderedDict()

    header_for = {}
    for h in cfg["header"]:
        header_for["HAVE_" + upper(h)] = h

    func_names = {}
    for k in cache:
        if k.startswith("ac_cv_func_"):
            fn = k[len("ac_cv_func_"):]
            func_names["HAVE_" + upper(fn)] = fn

    decl_for = {}
    for name, inc in cfg["decl"]:
        decl_for["HAVE_DECL_" + upper(name)] = (name, inc)

    type_for = {}
    for name, inc in cfg["type"]:
        type_for["HAVE_" + upper(name)] = (name, inc)

    member_for = {}
    for ty, member, inc in cfg["member"]:
        # `struct stat` + `st_rdev` -> HAVE_STRUCT_STAT_ST_RDEV;
        # `siginfo_t` + `si_band` -> HAVE_SIGINFO_T_SI_BAND
        member_for["HAVE_" + upper(ty + "_" + member)] = (ty, member, inc)

    sizeof_for = {}
    for ty, inc in cfg["sizeof"]:
        sizeof_for["SIZEOF_" + upper(ty.replace("*", "P"))] = (ty, inc)

    for name, val in oracle_names.items():
        if name in INCLUDE_GUARD:
            cat[name] = ("guard", None)
        elif name in UNEXPRESSIBLE:
            cat[name] = ("unexpressible", None)
        elif name in CUSTOM:
            cat[name] = ("custom", CUSTOM[name])
        elif name in cfg["pyfunc"]:
            fn, inc = cfg["pyfunc"][name]
            cat[name] = ("custom", ("symbol", fn, None, inc))
        elif name in header_for:
            cat[name] = ("header", header_for[name])
        elif name in decl_for:
            cat[name] = ("decl", decl_for[name])
        elif name in member_for:
            cat[name] = ("member", member_for[name])
        elif name in sizeof_for:
            cat[name] = ("sizeof", sizeof_for[name])
        elif name in type_for and val is not None:
            cat[name] = ("type", type_for[name])
        elif name in type_for:
            cat[name] = ("type", type_for[name])
        elif name in func_names and func_names[name] in LIBM_FUNCTIONS:
            cat[name] = ("custom", ("symbol", func_names[name], None, [], ["m"]))
        elif name in func_names:
            cat[name] = ("function", func_names[name])
        elif name.startswith("ALIGNOF_"):
            cat[name] = ("alignof", None)
        elif name in TYPEDEF_FALLBACKS:
            cat[name] = ("typedef-fallback", None)
        elif name in FEATURE_MACROS:
            cat[name] = ("feature-macro", None)
        elif re.match(r"HAVE_(BROKEN|WORKING)_|.*_BUG$", name):
            cat[name] = ("behaviour", None)
        else:
            cat[name] = ("literal", None)
    return cat


# ---------------------------------------------------------------------------
# Emitting.
# ---------------------------------------------------------------------------

def toml_str(s):
    return '"' + s.replace('\\', '\\\\').replace('"', '\\"') + '"'


def define_item(name, val):
    """A `defines = [...]` entry reproducing `#define NAME VAL`."""
    return toml_str(name if val == "" else f"{name}={val}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--configure", required=True, help="a configure script")
    ap.add_argument("--configure-ac", required=True, help="the matching configure.ac")
    ap.add_argument("--oracle", action="append", required=True,
                    metavar="PLATFORM=DIR", help="dir with pyconfig.h and config.log")
    ap.add_argument("--varies-with-installed-libraries", action="append", default=[],
                    metavar="MINIMAL_DIR:FULL_DIR",
                    help="two configure runs on one platform, without and with "
                         "third-party dev packages; names that differ are "
                         "environment-dependent")
    ap.add_argument("--out", default=str(Path(__file__).parent))
    a = ap.parse_args()

    cfg = read_configure(a.configure)
    cfg["pyfunc"] = read_py_check_funcs(a.configure_ac)
    oracles = OrderedDict()
    caches = {}
    for spec in a.oracle:
        plat, d = spec.split("=", 1)
        oracles[plat] = read_pyconfig(Path(d) / "pyconfig.h")
        caches[plat] = read_cache(Path(d) / "config.log")

    names = OrderedDict()
    for o in oracles.values():
        for n in o:
            names.setdefault(n, None)

    # The union of every platform's cache variables decides which functions
    # are *asked*; a name asked on one platform and not another is still a
    # probe, answered `no` where the oracle says so.
    cache_union = {}
    for c in caches.values():
        cache_union.update(c)
    cat = classify(names, cache_union, cfg)

    env_dependent = OrderedDict()
    for spec in a.varies_with_installed_libraries:
        lo, hi = spec.split(":")
        A, B = read_pyconfig(Path(lo) / "pyconfig.h"), read_pyconfig(Path(hi) / "pyconfig.h")
        for n in sorted(set(A) | set(B)):
            if (A.get(n) is None) != (B.get(n) is None) or A.get(n) != B.get(n):
                env_dependent[n] = (A.get(n), B.get(n))

    # A header whose cache answer is `yes` while its macro is *undefined* is not
    # a question about the header: CPython decides `HAVE_ZLIB_H`, `HAVE_LZMA_H`
    # and `HAVE_BZLIB_H` in its module-support logic, and leaves them undefined
    # even where the header is installed (checked: a container with the dev
    # packages still says `#undef`). Probing "does zlib.h exist" would answer a
    # different question from the macro's, and flip with the machine, so these
    # are asserted instead. Found by CI: the runner has the packages.
    decoupled = set()
    runs = [(caches[p], oracles[p]) for p in oracles]
    for spec in a.varies_with_installed_libraries:
        for d in spec.split(":"):
            runs.append((read_cache(Path(d) / "config.log"), read_pyconfig(Path(d) / "pyconfig.h")))
    for cache_, py_ in runs:
        for n, (k, spec_) in list(cat.items()):
            if k == "header" and cache_.get("ac_cv_header_" + mangle(spec_)) == "yes" \
                    and py_.get(n) is None:
                decoupled.add(n)
    # The same, one level up: a question configure asks in one environment and
    # skips in another. With `zlib1g-dev` installed pkg-config finds zlib, and
    # the `zlib.h` fallback check never runs at all; without it the check runs
    # and answers `no`. The final `pyconfig.h` is identical in both, so
    # comparing answers cannot see it -- only comparing what was *asked* can.
    def asked(cache_, kind, spec_):
        if kind == "header":
            return ("ac_cv_header_" + mangle(spec_)) in cache_
        if kind == "function":
            return ("ac_cv_func_" + spec_) in cache_
        if kind == "decl":
            return ("ac_cv_have_decl_" + spec_[0]) in cache_
        return True

    for spec in a.varies_with_installed_libraries:
        lo, hi = spec.split(":")
        c_lo, c_hi = read_cache(Path(lo) / "config.log"), read_cache(Path(hi) / "config.log")
        for n, (k, spec_) in list(cat.items()):
            if k in ("header", "function", "decl") and asked(c_lo, k, spec_) != asked(c_hi, k, spec_):
                decoupled.add(n)
    for n in decoupled:
        cat[n] = ("decoupled-header", None)

    # Names that are measured questions but were never *asked* on a platform:
    # configure only checks some functions and headers where a feature test
    # lets it, so an `#undef` there means "not asked", not "answered no".
    not_asked = OrderedDict()
    for plat, c in caches.items():
        miss = []
        for n, (k, spec) in cat.items():
            if k == "function" and ("ac_cv_func_" + spec) not in c:
                miss.append(n)
            elif k == "header" and ("ac_cv_header_" + mangle(spec)) not in c:
                miss.append(n)
            elif k == "decl" and ("ac_cv_have_decl_" + spec[0]) not in c:
                miss.append(n)
            elif n in CUSTOM_CACHE and CUSTOM_CACHE[n] not in c:
                miss.append(n)
        not_asked[plat] = sorted(miss)

    # ---- expected.json ----------------------------------------------------
    exp = OrderedDict()
    for n in names:
        exp[n] = OrderedDict(
            (p, o.get(n)) for p, o in oracles.items()
        )
    out = Path(a.out)
    (out / "expected.json").write_text(json.dumps(OrderedDict([
        ("categories", OrderedDict(sorted((n, c[0]) for n, c in cat.items()))),
        ("not_asked", not_asked),
        ("environment_dependent", OrderedDict(
            (n, {"without_dev_packages": v[0], "with_dev_packages": v[1]})
            for n, v in env_dependent.items())),
        ("answers", exp),
    ]), indent=1, sort_keys=False) + "\n")

    # ---- Harbour.<platform>.toml -----------------------------------------
    #
    # One manifest per platform, each with no conditions at all. A `[probes]`
    # table cannot be scoped by `when`, and the literals pyconfig.h carries
    # (`_POSIX_C_SOURCE`, `PLATFORM`, `SOABI`, ...) legitimately differ by
    # platform, so a single manifest could only express them as compile
    # defines that never reach the generated header. That is a Harbour
    # limitation, recorded in the PR; the probe lists are identical across the
    # files and only the asserted literals differ.
    headers = sorted({c[1] for n, c in cat.items() if c[0] == "header"})
    funcs = sorted({c[1] for n, c in cat.items() if c[0] == "function"})

    named = OrderedDict()
    for n, (k, spec) in cat.items():
        if k == "decl":
            nm, inc = spec
            named[n] = ([("symbol", nm)], inc, None)
        elif k == "type":
            nm, inc = spec
            named[n] = ([("type", nm)], inc, None)
        elif k == "member":
            ty, mem, inc = spec
            named[n] = ([("type", ty), ("member", mem)], inc, None)
        elif k == "sizeof":
            ty, inc = spec
            named[n] = ([("sizeof", ty)], inc, None)
        elif k == "custom":
            kind, subject, member, inc = spec[:4]
            kv = [(kind, subject)] + ([("member", member)] if member else [])
            named[n] = (kv, inc, spec[4] if len(spec) > 4 else None)

    lit_names = [n for n, c in cat.items()
                 if c[0] in ("literal", "behaviour", "alignof", "feature-macro",
                             "decoupled-header")]

    for plat in oracles:
        o = oracles[plat]
        os_, arch = plat.split("-")
        L = []
        w = L.append
        w("# GENERATED by generate.py from CPython 3.14.9's own `configure` (what is\n"
          "# asked) and its `pyconfig.h` on " + plat + " (the answers). Edit the\n"
          "# generator, not this file; see regenerate.md.\n")
        w('[package]\nname = "cpython-config"\nversion = "3.14.9"\n'
          'description = "CPython 3.14.9\'s configure questions, answered by Harbour probes"\n'
          'requires = "hosted"\n')
        w('[targets.cpython-config]\nkind = "exe"\nsources = ["src/main.c"]\n')

        fm = [(n, o[n]) for n in FEATURE_MACROS if o.get(n) is not None]
        w("# configure runs every check after pyconfig.h's feature-test macros are in\n"
          "# scope, so the probes must be asked under them: glibc declares `memrchr`\n"
          "# only with `_GNU_SOURCE`. They are also in the header, below.\n"
          f'[[targets.cpython-config.when]]\nos = "{os_}"\narch = "{arch}"\n'
          "defines = [\n    " + ",\n    ".join(define_item(n, v) for n, v in fm) + ",\n]\n")

        lits = [define_item(n, o[n]) for n in lit_names if o.get(n) is not None]
        w("[targets.cpython-config.probes]\n"
          'emit = { header = "pyconfig.h" }\n\n'
          "# Asserted, not measured: " + str(len(lit_names)) + " names -- feature-test\n"
          "# macros, choices, package metadata, run-time behaviour checks and\n"
          "# alignments. Each is a claim about this platform that a reviewer can see.\n"
          "defines = [\n    " + ",\n    ".join(lits) + ",\n]\n\n"
          "# Measured: " + str(len(headers)) + " headers.\n"
          + "check_headers = [\n" + "".join(f"  {toml_str(h)},\n" for h in headers) + "]\n\n"
          "# Measured: " + str(len(funcs)) + " functions; each one links.\n"
          + "check_symbols = [\n" + "".join(f"  {toml_str(f)},\n" for f in funcs) + "]\n")

        w("# Measured: declarations, types, struct members and sizes, which need a\n"
          "# name of their own or a prelude of configure's own includes.\n")
        for n, (kv, inc, libs) in named.items():
            w(f"[targets.cpython-config.probes.named.{n}]")
            for k, v in kv:
                w(f"{k} = {toml_str(v)}")
            inc = [h.lstrip("@") for h in inc
                   if not (h.startswith("@") and os_ != "linux")]
            keep = []
            for h in inc:
                header, _, guard = h.partition("?")
                if guard and o.get(guard) is None:
                    continue  # configure does not include it on this platform
                keep.append(header)
            inc = keep
            if inc:
                w("prelude = [" + ", ".join(toml_str(h) for h in inc) + "]")
            if libs:
                w("libs = [" + ", ".join(toml_str(l) for l in libs) + "]")
            w("")
        (out / f"Harbour.{plat}.toml").write_text("\n".join(L) + "\n")

    # ---- coverage table ---------------------------------------------------
    counts = OrderedDict()
    for n, (k, _) in cat.items():
        counts[k] = counts.get(k, 0) + 1
    print(json.dumps({"total_names": len(names), "by_category": counts,
                      "environment_dependent": len(env_dependent)}, indent=1))


if __name__ == "__main__":
    sys.exit(main())
