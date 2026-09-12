#!/usr/bin/env bash
# curl 8.22.0's configure questions, answered by Harbour probes and compared
# against what curl's own cmake concluded on this platform.
#
# This is the canary that means the most for the probe subsystem, and the one
# that would catch it silently returning constants: 13 of the 106 answers
# differ between macOS and Linux, in both directions. An oracle that agreed
# everywhere would be satisfied by a subsystem that measured nothing.
#
# Unlike the other canaries it fetches nothing: curl's *questions* are what
# is under test here, and the canary that builds curl itself is
# `ci/canary/curl/`. See `regenerate.md` for how `expected.json` was produced
# and for the questions that are still not asked, each with a stated reason.
#
# Usage: ci/canary/curl-config/run.sh [work-dir]
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
. "$here/../lib.sh"

repo="$(canary_repo_root "$here")"
harbour="$(canary_harbour "$repo")"
work="${1:-${TMPDIR:-/tmp}/harbour-canary-curl-config}"

case "$(uname -s)" in
  Darwin) platform=mac ;;
  Linux) platform=linux ;;
  *)
    echo "== skipped: no curl configure oracle for $(uname -s)" >&2
    exit 0
    ;;
esac

rm -rf "$work"
mkdir -p "$work"
cp -r "$here" "$work/pkg"
cd "$work/pkg" || exit 1

echo "== answering curl's configure questions on $platform"
"$harbour" build

header="$(find .harbour -name curl_config.h | head -1)"
if [ -z "$header" ]; then
  echo "== canary FAILED: no curl_config.h was generated" >&2
  exit 1
fi
echo "== generated $header ($(grep -c '' "$header") lines)"

# The whole comparison is one python script because the interesting output is
# a per-question table, and `diff` on two files with different orders and
# comment styles would report noise rather than disagreements.
python3 - "$header" "$here/expected.json" "$platform" <<'PY'
import json, re, sys

hdr, expected_path, platform = sys.argv[1], sys.argv[2], sys.argv[3]
expected = json.load(open(expected_path))

got = {}
for line in open(hdr):
    m = re.match(r'^#define\s+([A-Za-z_]\w*)\s*(.*)$', line)
    if m:
        v = m.group(2).strip()
        got[m.group(1)] = int(v) if v.isdigit() else True
        continue
    m = re.match(r'^/\*\s*#undef\s+([A-Za-z_]\w*)\s*\*/', line)
    if m:
        got[m.group(1)] = False

def same(want, have):
    # curl writes booleans as `1` / `#undef` and sizes as integers, so a
    # boolean comparison would call SIZEOF_LONG 4 and 8 equal. Compare
    # integers as integers.
    if isinstance(want, int) and not isinstance(want, bool) and want > 1:
        return want == have
    return bool(want) == bool(have)

agree, disagree, missing = [], [], []
for name, answers in sorted(expected.items()):
    want = answers[platform]
    if name not in got:
        missing.append(name)
    elif same(want, got[name]):
        agree.append(name)
    else:
        disagree.append((name, want, got[name]))

print(f"== agree {len(agree)}/{len(expected)}, disagree {len(disagree)}, "
      f"missing {len(missing)}")

# The platform-discriminating rows, always printed. These are the evidence
# that the probes are asking the toolchain, so they belong in the log on a
# pass as well as on a failure.
differ = sorted(n for n, v in expected.items()
                if bool(v['mac']) != bool(v['linux']))
print(f"== the {len(differ)} questions whose answers differ by platform:")
for n in differ:
    want, have = expected[n][platform], got.get(n, '<missing>')
    mark = "  " if same(want, have) else "!!"
    print(f"   {mark} {n:38} curl={want!s:8} harbour={have}")

for n, want, have in disagree:
    print(f"== DISAGREE {n}: curl says {want}, harbour says {have}", file=sys.stderr)
for n in missing:
    print(f"== MISSING  {n}: curl asks it, harbour's header does not answer it",
          file=sys.stderr)

if disagree or missing:
    print("== canary FAILED: harbour's answers do not match curl's configure",
          file=sys.stderr)
    sys.exit(1)
PY

echo "== canary passed (curl-config: 106 of curl's questions, 13 platform-specific)"
