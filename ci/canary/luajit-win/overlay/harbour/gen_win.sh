#!/bin/sh
# Generates LuaJIT's VM and library tables by running `buildvm`, which Harbour
# built for the build machine. Called from the library's `prebuild` step:
#
#   sh harbour/gen.sh <buildvm -m mode: machasm | elfasm>
set -e
MODE=$1
cd src

LIBS="lib_base.c lib_math.c lib_bit.c lib_string.c lib_table.c lib_io.c lib_os.c lib_package.c lib_debug.c lib_jit.c lib_ffi.c lib_buffer.c"
"$HARBOUR_TOOL_BUILDVM" -m peobj -o lj_vm.obj
for m in bcdef ffdef libdef recdef; do
  # shellcheck disable=SC2086
  "$HARBOUR_TOOL_BUILDVM" -m $m -o lj_$m.h $LIBS
done
# shellcheck disable=SC2086
"$HARBOUR_TOOL_BUILDVM" -m vmdef -o jit/vmdef.lua $LIBS
"$HARBOUR_TOOL_BUILDVM" -m folddef -o lj_folddef.h lj_opt_fold.c
