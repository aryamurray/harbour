#!/bin/sh
# Generates config.c, the table of built-in extension modules, from
# Modules/config.c.in and the module names given as arguments. This is what
# CPython's `makesetup` does with Modules/Setup.*; only the insertion is
# reproduced here, not makesetup's parsing of the Setup files.
#
#   sh harbour/config.sh atexit faulthandler posix ...
#
# Optional modules come from HARBOUR_FEATURES, which Harbour sets to the
# package's active features, so one step produces the right file for every
# feature combination. CONFIG_BASE=1 ignores them: the host bootstrap programs
# are compiled with the base table, since a built-in zlib would otherwise need
# a zlib built for the host as well.
set -e
OUT="${CONFIG_OUT:-Modules/config.c}"
mkdir -p "$(dirname "$OUT")"
if [ -z "$CONFIG_BASE" ]; then
  case ",$HARBOUR_FEATURES," in *,zlib,*) set -- "$@" zlib ;; esac
fi
awk -v mods="$*" '
BEGIN { n = split(mods, m, " ") }
NR == 1 { print "/* Generated automatically from ./Modules/config.c.in by makesetup. */" }
/-- ADDMODULE MARKER 1 --/ {
    for (i = 1; i <= n; i++) printf "extern PyObject* PyInit_%s(void);\n", m[i]
    print ""
}
/-- ADDMODULE MARKER 2 --/ {
    for (i = 1; i <= n; i++) printf "    {\"%s\", PyInit_%s},\n", m[i], m[i]
    print ""
}
{ print }
' Modules/config.c.in > "$OUT"
