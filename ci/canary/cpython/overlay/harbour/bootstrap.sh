#!/bin/sh
# Generates Python/frozen_modules/*.h, which Python/frozen.c includes.
#
# CPython freezes its own startup modules by running CPython. That is a
# bootstrap: a host program (_freeze_module, linked with a no-op getpath)
# freezes four modules, a second host program (_bootstrap_python) built from
# the same objects plus those four then freezes the rest. Both must run on the
# machine doing the build, so they are compiled with HARBOUR_HOST_CC.
#
# The 187 library objects below are also compiled by Harbour for the target;
# nothing can share them, so a native build compiles everything twice.
set -e
CC="${HARBOUR_HOST_CC:?HARBOUR_HOST_CC is not set}"
B="${HARBOUR_ARTIFACT_DIR:?}/../harbour-host"
mkdir -p "$B/host-objects" Python/frozen_modules
# Which platform's pyconfig.h. Native builds only: the host programs below are
# compiled for the machine running the build, with the *target's* pyconfig.h,
# so this is wrong in a cross build -- they would need the host's. That is the
# gap docs/superpowers/specs/2026-10-24-host-tools-design.md describes.
PLATFORM_DIR="harbour/pyconfig/${HARBOUR_TARGET_OS:?}-${HARBOUR_TARGET_ARCH:?}"
[ -f "$PLATFORM_DIR/pyconfig.h" ] || {
  echo "no vendored pyconfig.h for $HARBOUR_TARGET_OS-$HARBOUR_TARGET_ARCH ($PLATFORM_DIR)" >&2
  exit 1
}
case "$HARBOUR_TARGET_OS" in
  macos) SYSLIBS="-ldl -framework CoreFoundation -lm"; EXTRA_SRC="" ;;
  *)     SYSLIBS="-ldl -lm"; EXTRA_SRC="Python/asm_trampoline.S" ;;
esac
CFLAGS="-O0 -DNDEBUG -DPy_BUILD_CORE -I$PLATFORM_DIR -I./Include/internal -I./Include/internal/mimalloc -I. -I./Include"

# The host programs use the base module table; see harbour/config.sh.
BASE_MODS="atexit faulthandler posix _signal _tracemalloc _suggestions _datetime _codecs _collections errno _io itertools _sre _sysconfig _thread time _types _typing _weakref _abc _functools _locale _opcode _operator _stat _symtable pwd"
CONFIG_BASE=1 CONFIG_OUT="$B/config.c" sh harbour/config.sh $BASE_MODS
grep -v '^Modules/config.c$' harbour/lib_sources.txt > "$B/lib_sources.txt"

obj() { echo "$B/host-objects/$(echo "$1" | tr '/' '_' | sed 's/\.[cS]$/.o/')"; }
compile() { "$CC" -c $CFLAGS -o "$(obj "$1")" "$1"; }
export CC B CFLAGS

# Stage 1: _freeze_module.
for f in $(cat "$B/lib_sources.txt") Programs/_freeze_module.c Modules/getpath_noop.c "$B/config.c" $EXTRA_SRC; do echo "$f"; done \
  | xargs -n 1 -P "$(sysctl -n hw.ncpu 2>/dev/null || nproc)" sh -c '"$CC" -c $CFLAGS -o "$B/host-objects/$(echo "$1" | tr / _ | sed "s/\.[cS]\$/.o/")" "$1"' _
LIBOBJS="$(for f in $(cat "$B/lib_sources.txt") "$B/config.c" $EXTRA_SRC; do obj "$f"; done)"
"$CC" -o "$B/_freeze_module" "$(obj Programs/_freeze_module.c)" "$(obj Modules/getpath_noop.c)" $LIBOBJS $SYSLIBS
"$B/_freeze_module" getpath ./Modules/getpath.py Python/frozen_modules/getpath.h
"$B/_freeze_module" importlib._bootstrap ./Lib/importlib/_bootstrap.py Python/frozen_modules/importlib._bootstrap.h
"$B/_freeze_module" importlib._bootstrap_external ./Lib/importlib/_bootstrap_external.py Python/frozen_modules/importlib._bootstrap_external.h
"$B/_freeze_module" zipimport ./Lib/zipimport.py Python/frozen_modules/zipimport.h

# Stage 2: _bootstrap_python, using the four headers just written.
for f in Programs/_bootstrap_python.c Modules/getpath.c; do compile "$f"; done
"$CC" -o ./_bootstrap_python "$(obj Programs/_bootstrap_python.c)" "$(obj Modules/getpath.c)" $LIBOBJS $SYSLIBS
# CPython only treats an executable as being in a build tree -- and so finds
# `Lib/` -- when it sits at the source root beside these two landmarks.
touch Modules/Setup.local
echo none > pybuilddir.txt
./_bootstrap_python ./Programs/_freeze_module.py abc ./Lib/abc.py Python/frozen_modules/abc.h
./_bootstrap_python ./Programs/_freeze_module.py codecs ./Lib/codecs.py Python/frozen_modules/codecs.h
./_bootstrap_python ./Programs/_freeze_module.py io ./Lib/io.py Python/frozen_modules/io.h
./_bootstrap_python ./Programs/_freeze_module.py _collections_abc ./Lib/_collections_abc.py Python/frozen_modules/_collections_abc.h
./_bootstrap_python ./Programs/_freeze_module.py _sitebuiltins ./Lib/_sitebuiltins.py Python/frozen_modules/_sitebuiltins.h
./_bootstrap_python ./Programs/_freeze_module.py genericpath ./Lib/genericpath.py Python/frozen_modules/genericpath.h
./_bootstrap_python ./Programs/_freeze_module.py ntpath ./Lib/ntpath.py Python/frozen_modules/ntpath.h
./_bootstrap_python ./Programs/_freeze_module.py posixpath ./Lib/posixpath.py Python/frozen_modules/posixpath.h
./_bootstrap_python ./Programs/_freeze_module.py os ./Lib/os.py Python/frozen_modules/os.h
./_bootstrap_python ./Programs/_freeze_module.py site ./Lib/site.py Python/frozen_modules/site.h
./_bootstrap_python ./Programs/_freeze_module.py stat ./Lib/stat.py Python/frozen_modules/stat.h
./_bootstrap_python ./Programs/_freeze_module.py importlib.util ./Lib/importlib/util.py Python/frozen_modules/importlib.util.h
./_bootstrap_python ./Programs/_freeze_module.py importlib.machinery ./Lib/importlib/machinery.py Python/frozen_modules/importlib.machinery.h
./_bootstrap_python ./Programs/_freeze_module.py runpy ./Lib/runpy.py Python/frozen_modules/runpy.h
./_bootstrap_python ./Programs/_freeze_module.py __hello__ ./Lib/__hello__.py Python/frozen_modules/__hello__.h
./_bootstrap_python ./Programs/_freeze_module.py __phello__ ./Lib/__phello__/__init__.py Python/frozen_modules/__phello__.h
./_bootstrap_python ./Programs/_freeze_module.py __phello__.ham ./Lib/__phello__/ham/__init__.py Python/frozen_modules/__phello__.ham.h
./_bootstrap_python ./Programs/_freeze_module.py __phello__.ham.eggs ./Lib/__phello__/ham/eggs.py Python/frozen_modules/__phello__.ham.eggs.h
./_bootstrap_python ./Programs/_freeze_module.py __phello__.spam ./Lib/__phello__/spam.py Python/frozen_modules/__phello__.spam.h
./_bootstrap_python ./Programs/_freeze_module.py frozen_only ./Tools/freeze/flag.py Python/frozen_modules/frozen_only.h
