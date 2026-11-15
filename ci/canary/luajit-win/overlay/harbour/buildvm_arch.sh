#!/bin/sh
# Writes `src/host/buildvm_arch.h`, which `buildvm` is compiled against. Run by
# `buildvm`'s own `prebuild` step, with `minilua` built first:
#
#   sh harbour/buildvm_arch.sh <vm_*.dasc> "<DynASM -D flags>"
#
# The DynASM flags name the TARGET architecture, not the host: the header
# contains the target's assembly encoder tables.
set -e
cd src

# `genversion.lua` reads the release number from a file the Makefile fills in
# from `git log`. Pinned instead, so the build neither needs git nor changes
# with the checkout: this is the committer timestamp of the pinned commit.
echo "$LUAJIT_RELVER" > luajit_relver.txt
"$HARBOUR_TOOL_MINILUA" host/genversion.lua

# shellcheck disable=SC2086  # $2 is a flag list
"$HARBOUR_TOOL_MINILUA" ../dynasm/dynasm.lua $2 -o host/buildvm_arch.h "$1"
