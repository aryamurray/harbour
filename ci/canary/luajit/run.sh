#!/usr/bin/env bash
# LuaJIT 2.1 -- a package that compiles a program and runs it during the build.
#
# `buildvm` writes the VM's assembly and seven tables; the library cannot be
# compiled until it has run, and it has to run on the build machine whatever
# the target is. That is what this canary is for: it exercises `prebuild`
# generators that are themselves compiled, `HARBOUR_HOST_CC`, and a generated
# `.S` that is the interpreter's core.
#
# The consumer runs a hot loop long enough for the tracing JIT to compile it
# and then asks the VM whether it did, so the generated assembly is exercised
# and not just linked. Correct arithmetic alone would pass on the interpreter
# even if the JIT backend were absent or the wrong architecture's.
#
# LuaJIT publishes no releases, so this pins a commit and fetches it with git;
# git checks every object against the hash, which a GitHub-generated tarball's
# bytes cannot promise.
#
# Usage: ci/canary/luajit/run.sh [work-dir]
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
. "$here/../lib.sh"

URL="https://github.com/LuaJIT/LuaJIT.git"
export CANARY_GIT_COMMIT=a4c064539ea931274d407e1249b4bb575424a0f4

# The architecture name LuaJIT's `jit.arch` reports. A build that picked the
# wrong DynASM flags crashes rather than printing the wrong name, but the
# consumer prints it anyway and this asserts it: a manifest block selected for
# the wrong platform should name itself.
case "$(uname -m)" in
  x86_64 | amd64) ARCH=x64 ;;
  arm64 | aarch64) ARCH=arm64 ;;
  *) echo "== canary FAILED: no expected jit.arch for $(uname -m)" >&2; exit 1 ;;
esac
export LJ_EXPECT_ARCH="$ARCH"

# The generated VM assembly. Its object must define the interpreter entry
# point, or the "library" is a pile of C that links and never runs.
canary_extra_assertions() {
  canary_require_object lj_vm.o
  canary_defines_symbol lj_vm.o lj_vm_asm_begin
}


canary_standard_run \
  "$here" "$URL" "" "" "luajit" "lj-canary" "70" \
  "${1:-${TMPDIR:-/tmp}/harbour-canary-luajit.$$}"
