#!/usr/bin/env bash
# CPython 3.14.9's configure questions, answered by Harbour probes and compared
# against what CPython's own `./configure` concluded on this platform.
#
# This is curl-config's sibling and the bigger test of the probe subsystem: 654
# names, of which ~540 are measured from the toolchain and the rest asserted.
# The comparison prints the two groups separately -- an oracle satisfied by
# literals would say nothing about the probes -- and lists every disagreement
# by name. See `regenerate.md` for how the manifests and `expected.json` were
# produced and for what is not asked, each with a reason.
#
# Usage: ci/canary/cpython-config/run.sh [work-dir]
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
. "$here/../lib.sh"

repo="$(canary_repo_root "$here")"
harbour="$(canary_harbour "$repo")"
work="${1:-${TMPDIR:-/tmp}/harbour-canary-cpython-config.$$}"

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) platform=macos-aarch64 ;;
  Linux-aarch64 | Linux-arm64) platform=linux-aarch64 ;;
  Linux-x86_64) platform=linux-x86_64 ;;
  *)
    echo "== skipped: no CPython configure oracle for $(uname -s)-$(uname -m)" >&2
    exit 0
    ;;
esac

rm -rf "$work"
mkdir -p "$work/pkg"
cp -r "$here/src" "$work/pkg/src"
cp "$here/Harbour.$platform.toml" "$work/pkg/Harbour.toml"
cd "$work/pkg" || exit 1

echo "== answering CPython's configure questions on $platform"
start=$(date +%s)
"$harbour" build
elapsed=$(( $(date +%s) - start ))

header="$(find .harbour -name pyconfig.h | head -1)"
if [ -z "$header" ]; then
  echo "== canary FAILED: no pyconfig.h was generated" >&2
  exit 1
fi
echo "== generated $header ($(grep -c '' "$header") lines) in ${elapsed}s"

python3 - "$header" "$here/expected.json" "$platform" <<'PY'
import json, re, sys

hdr, expected_path, platform = sys.argv[1], sys.argv[2], sys.argv[3]
spec = json.load(open(expected_path))
answers, cats = spec["answers"], spec["categories"]
env_dependent = spec["environment_dependent"]
not_asked = set(spec["not_asked"].get(platform, []))

got = {}
for line in open(hdr):
    m = re.match(r'^#define\s+([A-Za-z_]\w*)(?:[ \t]+(.*))?$', line)
    if m:
        got[m.group(1)] = (m.group(2) or "").strip()
        continue
    m = re.match(r'^/\*\s*#undef\s+([A-Za-z_]\w*)\s*\*/', line)
    if m:
        got.setdefault(m.group(1), None)

def same(want, have):
    # Defined means defined: `#define HAVE_DECL_X 0` and `#undef HAVE_DECL_X`
    # are the same answer to C's `#if`, and are the same here. Sizes are
    # compared as numbers.
    if want is not None and have is not None and re.fullmatch(r'-?\d+', want) \
            and re.fullmatch(r'-?\d+', have) and int(want) > 1:
        return int(want) == int(have)
    falsy = lambda v: v is None or v == "0"
    if falsy(want) or falsy(have):
        return falsy(want) and falsy(have)
    return want == have

MEASURED = {"header", "function", "decl", "type", "member", "sizeof", "custom"}
groups = {}
disagree, ignored = [], []
for name, cat in cats.items():
    want = answers[name][platform]
    if cat in ("guard", "unexpressible", "typedef-fallback"):
        bucket = "not asked: " + cat
        if cat == "typedef-fallback" and got.get(name) is not None:
            disagree.append((name, cat, want, got.get(name)))
    elif name in not_asked:
        # configure only checks some functions and headers where an earlier
        # feature test allowed it, so its `#undef` here means "never asked".
        bucket = "not asked by configure on this platform (not compared)"
    elif name in env_dependent and cat in MEASURED:
        ignored.append(name)
        bucket = "environment-dependent (not compared)"
    else:
        bucket = "measured" if cat in MEASURED else "asserted"
        have = got.get(name)
        if not same(want, have):
            disagree.append((name, cat, want, have))
    groups[bucket] = groups.get(bucket, 0) + 1

print("== " + ", ".join(f"{k}: {v}" for k, v in sorted(groups.items())))
if ignored:
    print(f"== {len(ignored)} measured names are environment-dependent "
          f"(they change with installed dev packages) and are not compared:")
    print("   " + " ".join(sorted(ignored)))

by_platform = [n for n, a in answers.items()
               if cats[n] in MEASURED and n not in env_dependent
               and len({repr(v is None or v == '0') for v in a.values()}) > 1]
print(f"== {len(by_platform)} measured questions whose answer differs between "
      f"the platforms this canary knows:")
for n in sorted(by_platform):
    want, have = answers[n][platform], got.get(n)
    mark = "  " if same(want, have) else "!!"
    print(f"   {mark} {n:42} cpython={want!s:8} harbour={have}")

for n, cat, want, have in disagree:
    print(f"== DISAGREE {n} ({cat}): configure says {want!r}, harbour says {have!r}",
          file=sys.stderr)
if disagree:
    print(f"== canary FAILED: {len(disagree)} answers do not match CPython's configure",
          file=sys.stderr)
    sys.exit(1)
PY

echo "== canary passed (cpython-config on $platform)"
