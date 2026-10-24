#!/bin/sh
# Generates LuaJIT's VM and library tables. Called from Harbour.toml's
# `prebuild` blocks, once per platform:
#
#   sh harbour/gen.sh <LUAJIT_ARCH_*> <vm_*.dasc> <buildvm -m mode> "<DynASM -D flags>" ["<extra host defines>"]
#
# LuaJIT builds its own code generators. `minilua` (a Lua interpreter) runs
# DynASM over the architecture's `.dasc` to make `buildvm_arch.h`; `buildvm`
# is then compiled with that header and *run* to emit the VM assembly and the
# tables the core sources `#include`. Both programs run on the build machine,
# so they are compiled with HARBOUR_HOST_CC -- but their flags name the
# **target's** architecture, not the host's.
set -e
ARCH=$1 DASC=$2 MODE=$3 DASM=$4 EXTRA=${5:-}
HOSTCC="${HARBOUR_HOST_CC:?HARBOUR_HOST_CC is not set}"

cd src
T="-DLUAJIT_TARGET=LUAJIT_ARCH_$ARCH -DLJ_ARCH_HASFPU=1 -DLJ_ABI_SOFTFP=0 $EXTRA"

# `genversion.lua` reads the release number from a file the Makefile fills in
# from `git log`. Pinned instead, so the build neither needs git nor changes
# with the checkout: this is the committer timestamp of the pinned commit.
echo "$LUAJIT_RELVER" > luajit_relver.txt

# shellcheck disable=SC2086  # $T and $DASM are flag lists
$HOSTCC -O2 -I. $T -o host/minilua host/minilua.c -lm
host/minilua host/genversion.lua
# shellcheck disable=SC2086
host/minilua ../dynasm/dynasm.lua $DASM -o host/buildvm_arch.h "$DASC"
# shellcheck disable=SC2086
$HOSTCC -O2 -I. $T -o host/buildvm host/buildvm.c host/buildvm_asm.c \
  host/buildvm_peobj.c host/buildvm_lib.c host/buildvm_fold.c

LIBS="lib_base.c lib_math.c lib_bit.c lib_string.c lib_table.c lib_io.c lib_os.c lib_package.c lib_debug.c lib_jit.c lib_ffi.c lib_buffer.c"
host/buildvm -m "$MODE" -o lj_vm.S
for m in bcdef ffdef libdef recdef; do
  # shellcheck disable=SC2086
  host/buildvm -m $m -o lj_$m.h $LIBS
done
# shellcheck disable=SC2086
host/buildvm -m vmdef -o jit/vmdef.lua $LIBS
host/buildvm -m folddef -o lj_folddef.h lj_opt_fold.c
