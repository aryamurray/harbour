#!/usr/bin/env bash
# Windows feasibility probe. Every experiment is isolated: a failure prints its
# first errors and the tail of its log, then the run carries on, and a table is
# printed at the end. This script never exits non-zero on an experiment failure.
set -u
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
HARBOUR="$REPO/target/debug/harbour.exe"
WORK="${RUNNER_TEMP:-/tmp}/wprobe"
mkdir -p "$WORK"
RESULTS="$WORK/results.txt"; : > "$RESULTS"

exp() {
  local name=$1; shift
  local log="$WORK/$name.log" start=$SECONDS st
  echo "::group::$name"
  if "$@" >"$log" 2>&1; then st=PASS; else st=FAIL; fi
  echo "[$st] $name ($((SECONDS - start))s)"
  if [ "$st" = FAIL ]; then
    echo "--- first errors:"; grep -nE "error|Error|FAILED|fatal|LNK[0-9]+|C[0-9]{4}|D[0-9]{4}|cannot|not found|No such" "$log" | head -14
    echo "--- tail:"; tail -n 14 "$log"
  else
    tail -n 4 "$log"
  fi
  echo "::endgroup::"
  echo "$st|$name|$((SECONDS - start))s" >> "$RESULTS"
}

echo "== environment"
echo "harbour: $("$HARBOUR" --version 2>&1)"
for t in cl link lib ml64 nasm perl python3 bash git curl tar sha256sum; do
  printf '%-8s %s\n' "$t" "$(command -v $t 2>/dev/null || echo '(not on PATH)')"
done
vsw="/c/Program Files (x86)/Microsoft Visual Studio/Installer/vswhere.exe"
[ -x "$vsw" ] && "$vsw" -latest -products '*' -property installationPath -property installationVersion 2>&1 | head -4
echo "uname: $(uname -s) $(uname -m)"

# ---- 1. baseline: does Harbour find MSVC by itself (no vcvars, no msvc-dev-cmd)?
baseline() {
  cd "$WORK" && rm -rf base-exe base-lib
  "$HARBOUR" new base-exe && (cd base-exe && "$HARBOUR" build && ./.harbour/target/debug/bin/base-exe.exe) || return 1
  "$HARBOUR" new base-lib --lib && (cd base-lib && "$HARBOUR" build && ls -la .harbour/target/debug/lib/) || return 1
}
exp baseline-exe-and-lib baseline

# ---- 2. canaries driven exactly as CI drives them on Linux. tar cannot create
# symlinks under MSYS without help (zstd's tarball has some): `lnk` makes it
# write .lnk stand-ins, which nothing here reads.
export MSYS=winsymlinks:lnk
for p in zlib cjson; do
  exp "canary-$p" bash "$REPO/ci/canary/$p/run.sh" "$WORK/work-$p"
done

# ---- 3. Windows variants written for this probe (probe branch only)
# libuv's consumer names a macro MSG, which collides with the MSG struct that
# <windows.h> (pulled in by uv.h) defines. A consumer bug, not a Harbour one.
sed -i 's/\bMSG\b/CANARY_MSG/g' "$REPO/ci/canary/libuv-win/consumer/src/main.c"
for p in zstd-win libuv-win luajit-win; do
  exp "canary-$p" bash "$REPO/ci/canary/$p/run.sh" "$WORK/work-$p"
done

echo; echo "================ RESULTS ================"
column -s'|' -t "$RESULTS" 2>/dev/null || cat "$RESULTS"
{ echo '### Windows probe'; echo; echo '| result | experiment | time |'; echo '|---|---|---|'; sed 's/^/| /; s/|/ | /g; s/$/ |/' "$RESULTS"; } >> "${GITHUB_STEP_SUMMARY:-/dev/null}" 2>/dev/null
exit 0
