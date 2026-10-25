#!/usr/bin/env python3
"""Compare two pyconfig.h files by the macros the C preprocessor ends up with.

A textual comparison (what the cpython-config canary does) counts a `#define` inside
a false `#if` as defined: configure's header has `WORDS_BIGENDIAN` under
`#if defined AC_APPLE_UNIVERSAL_BUILD`, so a textual oracle called it "defined" and a
header built from it claimed a big-endian machine. `cc -dM -E` evaluates the
conditionals, so this compares what compiling CPython would actually see.

usage: verify_pyconfig.py <expected pyconfig.h> <actual pyconfig.h> [--allow NAME,...]
Exit status 1 if the effective macros differ, beyond the allowed names.
"""
import os
import subprocess
import sys
import tempfile


def effective(header, cc):
    """{macro: replacement} the preprocessor defines after including `header`,
    minus what the compiler defines by itself."""
    with tempfile.TemporaryDirectory() as d:
        src = os.path.join(d, "t.c")
        open(src, "w").write(f'#include "{os.path.abspath(header)}"\n')
        empty = os.path.join(d, "e.c")
        open(empty, "w").write("")
        def dump(path):
            out = subprocess.run([cc, "-dM", "-E", "-x", "c", path],
                                 capture_output=True, text=True, check=True).stdout
            result = {}
            for line in out.splitlines():
                parts = line.split(None, 2)
                if len(parts) >= 2 and parts[0] == "#define":
                    result[parts[1]] = parts[2] if len(parts) == 3 else ""
            return result
        base, full = dump(empty), dump(src)
    return {k: v for k, v in full.items() if base.get(k) != v}


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    allow = set()
    if "--allow" in sys.argv:
        allow = set(sys.argv[sys.argv.index("--allow") + 1].split(","))
        args = [a for a in args if a not in sys.argv[sys.argv.index("--allow") + 1:]]
    if len(args) != 2:
        sys.exit(__doc__)
    cc = os.environ.get("CC", "cc")
    want, have = effective(args[0], cc), effective(args[1], cc)
    only_want = sorted(k for k in want if k not in have and k not in allow)
    only_have = sorted(k for k in have if k not in want and k not in allow)
    differ = sorted(k for k in want if k in have and want[k] != have[k] and k not in allow)
    print(f"effective macros: expected {len(want)}, actual {len(have)}")
    print(f"missing from actual ({len(only_want)}): {only_want}")
    print(f"extra in actual ({len(only_have)}): {only_have}")
    print(f"different value ({len(differ)}): {[(k, want[k], have[k]) for k in differ]}")
    sys.exit(1 if (only_want or only_have or differ) else 0)


if __name__ == "__main__":
    main()
