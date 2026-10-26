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
#
# Then the host programs themselves. `minilua` and `buildvm` are `host = true`
# targets: built for the machine running the build, with their executables in
# `host-bin/` rather than `bin/`, never part of the library's own artifacts.
canary_extra_assertions() {
  if canary_on_windows; then
    # MSVC has no assembler Harbour drives, so `buildvm -m peobj` writes the VM
    # as a finished COFF object and the library links it as a source. There is no
    # `lj_vm.o` to read symbols from (and GNU `nm` is not a reliable reader of a
    # COFF file written by another tool), so the evidence is that the generator
    # produced a real object and that the consumer's JIT and arch checks pass on
    # top of it.
    [ -s upstream/src/lj_vm.obj ] \
      && echo "ok   buildvm -m peobj wrote src/lj_vm.obj ($(wc -c < upstream/src/lj_vm.obj) bytes)" \
      || { echo "== canary FAILED: no lj_vm.obj written by buildvm" >&2; exit 1; }
    [ "$(wc -c < upstream/src/lj_vm.obj)" -gt 10000 ] \
      || { echo "== canary FAILED: lj_vm.obj is implausibly small" >&2; exit 1; }
  else
    canary_require_object lj_vm.o
    canary_defines_symbol lj_vm.o lj_vm_asm_begin
  fi

  local tool path
  for tool in minilua buildvm; do
    path="$(find upstream/.harbour -path '*/host-bin/*' \( -name "$tool" -o -name "$tool.exe" \) -print -quit)"
    if [ -z "$path" ]; then
      echo "== canary FAILED: host tool \`$tool\` was not built under host-bin/" >&2
      exit 1
    fi
    echo "ok   host tool built: $path"
    # The tool had to RUN here to produce the library's sources, which is the
    # real proof it is a host binary; naming the architecture makes a failure
    # say which one it got.
    file "$path" | sed 's/^/     /'
  done

  canary_cross_build_targets_the_other_arch
}

# macOS only: build for the *other* Apple architecture, which needs no cross
# toolchain, and check each half landed where it belongs. `buildvm` is compiled
# for the host and configured for the target (`target_arch` conditions), so the
# VM it writes -- and therefore `lj_vm.o` -- must be the target's, while the
# tool itself stays a host binary. A `buildvm` configured for the host would
# write host assembly that the target assembler rejects, so the build succeeding
# is already most of the assertion; the object's architecture pins the rest.
canary_cross_build_targets_the_other_arch() {
  [ "$(uname -s)" = Darwin ] || return 0
  local host_arch target_triple target_word
  case "$(uname -m)" in
    arm64) host_arch=arm64; target_triple=x86_64-apple-darwin; target_word=x86_64 ;;
    x86_64) host_arch=x86_64; target_triple=arm64-apple-darwin; target_word=arm64 ;;
    *) return 0 ;;
  esac
  local repo harbour
  repo="$(canary_repo_root "$here")"
  harbour="$(canary_harbour "$repo")"

  echo "== cross-building the library for $target_triple"
  (cd upstream && "$harbour" build --target-triple "$target_triple")

  local obj tool
  obj="$(find upstream/.harbour/target -path '*/host-tools' -prune -o -name lj_vm.o -path "*${target_triple}*" -print -quit)"
  if [ -z "$obj" ] || ! file "$obj" | grep -q "$target_word"; then
    echo "== canary FAILED: the cross-built lj_vm.o is not $target_word (got: $(file "${obj:-/nonexistent}" 2>&1))" >&2
    exit 1
  fi
  echo "ok   cross-built lj_vm.o is $target_word"

  tool="$(find upstream/.harbour -path '*/host-bin/buildvm' -print -quit)"
  if ! file "$tool" | grep -q "$host_arch"; then
    echo "== canary FAILED: buildvm must stay a $host_arch host binary (got: $(file "$tool"))" >&2
    exit 1
  fi
  echo "ok   buildvm is still a $host_arch host binary"
}

# 76 translation units: the library's 70, plus the host tools' 6 (`minilua` 1,
# `buildvm` 5). On a native build the tools' objects sit in the package's own
# object directory, so they are counted; a cross build keeps them elsewhere.
# Windows is 75: the 69 C files and the 6 tool files, and no `lj_vm.S` to compile
# (its object is the generator's output, linked, not compiled).
if canary_on_windows; then OBJECTS=75; else OBJECTS=76; fi
canary_standard_run \
  "$here" "$URL" "" "" "luajit" "lj-canary" "$OBJECTS" \
  "${1:-${TMPDIR:-/tmp}/harbour-canary-luajit.$$}"
