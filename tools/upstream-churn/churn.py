#!/usr/bin/env python3
"""Measure how often an upstream release forces a manifest update.

For each repo in repos.txt this makes a blobless bare clone, lists the release
tags, and diffs each consecutive pair with `git diff --name-status`. No GitHub
API is used, so there is no rate limit and no token is needed for public repos.

Two signals are recorded per release pair, both counted outside test, doc,
example, vendored and similar directories:

  src_changed    a compiled source (.c .cc .cpp .cxx .S .s .asm) was added or
                 deleted. This is what breaks an explicit source list.
  build_changed  a build-system file (CMakeLists.txt, *.cmake, Makefile*,
                 configure.ac, meson.build, BUILD.bazel, ...) was touched. This
                 is the proxy for "the build recipe moved", and it over-counts:
                 most edits to those files do not affect a Harbour manifest.

So read src_changed as the likelier break and build_changed as an upper bound.
Neither sees a changed define or a new HAVE_ probe that lives inside an
unchanged file. That is the main thing this under-counts.

Usage:
    churn.py --cache DIR [--repos repos.txt] [--since-years 3] [--jobs 6]
             [--out pairs.jsonl]
"""

import argparse
import concurrent.futures as cf
import json
import os
import re
import statistics
import subprocess
import sys
import time
from pathlib import Path

# The version is the numeric tail of the tag, after a separator (or the start)
# and at most two letters of prefix ("v", "n"): v1.2.3, curl-8_5_0, REL_16_1,
# pcre2-10.49, n6.1. 2-4 components, so a floating "v2" is not a release.
# A trailing -YYYYMMDD (json-c) or -stable (libevent) is allowed; "-rc1", "w",
# "-alpha" fail the match on purpose, since only final releases count.
VERSION_RE = re.compile(
    r"(?:^|[^0-9A-Za-z.])[A-Za-z]{0,2}(\d+(?:[._]\d+){1,3})(?:-\d{8}|-stable)?$"
)
# freetype's VER-2-15-0 separates components with dashes.
DASHED_RE = re.compile(r"^[A-Za-z]+-(\d+(?:-\d+){1,3})$")

BUILD_NAMES = {
    "CMakeLists.txt", "CMakePresets.json", "Makefile", "GNUmakefile",
    "Makefile.am", "configure.ac", "configure.in", "meson.build",
    "meson_options.txt", "BUILD", "BUILD.bazel", "WORKSPACE", "MODULE.bazel",
    "SConstruct", "SConscript", "build.ninja",
}
BUILD_SUFFIXES = (".cmake", ".mk", ".gyp", ".gypi", ".bzl")
BUILD_PREFIXES = ("Makefile.",)
# A root-level add or delete of one of these is a build-system switch.
ROOT_BUILDSYS = {
    "CMakeLists.txt", "meson.build", "configure.ac", "configure.in",
    "BUILD.bazel", "BUILD", "Makefile", "SConstruct",
}
SRC_SUFFIXES = (".c", ".cc", ".cpp", ".cxx", ".S", ".s", ".asm")

NOISE_DIR = re.compile(
    r"^(tests?|testing|unittests?|unit_tests?|t|docs?|examples?|samples?|"
    r"demos?|bench(marks?)?|fuzz(ers?|ing)?|contrib|third_party|thirdparty|"
    r"vendor|deps|external|tools?|scripts?|\.github|ci)$",
    re.IGNORECASE,
)
NOISE_FILE = re.compile(
    r"^(test|tests|bench|example|demo|fuzz)[_-]|[_-](test|tests|bench|fuzz|example|demo)\.\w+$",
    re.IGNORECASE,
)


def parse_version(tag):
    m = VERSION_RE.search(tag) or DASHED_RE.match(tag)
    if not m:
        return None
    return tuple(int(p) for p in re.split(r"[._-]", m.group(1)))


def bump_kind(a, b):
    pa = (tuple(a) + (0, 0, 0))[:3]
    pb = (tuple(b) + (0, 0, 0))[:3]
    if pa[0] != pb[0]:
        return "major"
    if pa[1] != pb[1]:
        return "minor"
    return "patch"


def is_noise(path):
    parts = path.split("/")
    if any(NOISE_DIR.match(p) for p in parts[:-1]):
        return True
    return bool(NOISE_FILE.search(parts[-1]))


def is_build_file(path):
    name = path.rsplit("/", 1)[-1]
    return (
        name in BUILD_NAMES
        or name.endswith(BUILD_SUFFIXES)
        or name.startswith(BUILD_PREFIXES)
    )


def classify_changes(changes):
    """changes: iterable of (status, path). Returns the per-pair counters."""
    r = {"src_added": 0, "src_deleted": 0, "buildsys_switch": 0,
         "files_changed": 0}
    build_paths = []
    for status, path in changes:
        r["files_changed"] += 1
        if status in "AD" and path in ROOT_BUILDSYS:
            r["buildsys_switch"] += 1
        if is_noise(path):
            continue
        if path.endswith(SRC_SUFFIXES):
            if status == "A":
                r["src_added"] += 1
            elif status == "D":
                r["src_deleted"] += 1
        elif is_build_file(path):
            build_paths.append(path)
    r["src_changed"] = r["src_added"] + r["src_deleted"] > 0
    r["build_touched"] = bool(build_paths)
    r["build_paths"] = build_paths
    r["buildsys_switch"] = r["buildsys_switch"] > 0
    return r


def git(bare, *args, timeout=900):
    env = dict(os.environ, GIT_TERMINAL_PROMPT="0")
    p = subprocess.run(
        ["git", "-C", str(bare), *args],
        capture_output=True, text=True, timeout=timeout, env=env,
    )
    if p.returncode != 0:
        raise RuntimeError(p.stderr.strip() or f"git {args[0]} failed")
    return p.stdout


def ensure_clone(slug, cache):
    bare = cache / (slug.replace("/", "__") + ".git")
    env = dict(os.environ, GIT_TERMINAL_PROMPT="0")
    if bare.exists():
        git(bare, "fetch", "--quiet", "--force", "origin", "+refs/tags/*:refs/tags/*")
        return bare
    p = subprocess.run(
        ["git", "clone", "--bare", "--filter=blob:none", "--quiet",
         f"https://github.com/{slug}.git", str(bare)],
        capture_output=True, text=True, timeout=1800, env=env,
    )
    if p.returncode != 0:
        raise RuntimeError(p.stderr.strip() or "clone failed")
    return bare


def release_versions(bare):
    out = git(bare, "for-each-ref", "--format=%(refname:strip=2)\t%(creatordate:unix)",
              "refs/tags")
    seen = {}
    for line in out.splitlines():
        name, _, ts = line.partition("\t")
        v = parse_version(name)
        if v is None or not ts:
            continue
        seen.setdefault(v, (name, int(ts)))
    return sorted((v, n, ts) for v, (n, ts) in seen.items())


def tag_changes(bare, a, b):
    out = git(bare, "diff", "--name-status", "--no-renames", "-z",
              f"refs/tags/{a}", f"refs/tags/{b}")
    tok = out.split("\0")
    return [(tok[i][0], tok[i + 1]) for i in range(0, len(tok) - 1, 2)]


def build_lines(bare, a, b, paths):
    """Lines added plus deleted across `paths`. A release that only bumps the
    version string touches CMakeLists.txt, Makefile.am and configure.ac for a
    line or two each; counting the files alone would call every release a
    build change."""
    total = 0
    for i in range(0, len(paths), 200):
        out = git(bare, "diff", "--numstat", "--no-renames",
                  f"refs/tags/{a}", f"refs/tags/{b}", "--", *paths[i:i + 200])
        for line in out.splitlines():
            added, deleted, _ = line.split("\t", 2)
            total += (int(added) if added != "-" else 0)
            total += (int(deleted) if deleted != "-" else 0)
    return total


def analyze_repo(slug, cache, cutoff, max_pairs, build_threshold):
    bare = ensure_clone(slug, cache)
    vs = release_versions(bare)
    pairs = [(vs[i - 1], vs[i]) for i in range(1, len(vs)) if vs[i][2] >= cutoff]
    pairs = pairs[-max_pairs:]
    rows = []
    for (va, na, ta), (vb, nb, tb) in pairs:
        row = classify_changes(tag_changes(bare, na, nb))
        paths = row.pop("build_paths")
        row["build_lines"] = build_lines(bare, na, nb, paths) if paths else 0
        row["build_changed"] = row["build_lines"] >= build_threshold
        row["any_changed"] = row["src_changed"] or row["build_changed"]
        row.update(repo=slug, **{"from": na, "to": nb},
                   kind=bump_kind(va, vb), days=max(0, (tb - ta) // 86400), ts=tb)
        rows.append(row)
    return rows


def pct(n, d):
    return f"{100 * n / d:5.1f}%" if d else "    - "


def summarize(rows, repos_ok, repos_skipped, years):
    print(f"\n{len(rows)} release pairs across {len(repos_ok)} repos "
          f"(window: last {years:g} years)\n")
    print(f"{'bump':<8}{'pairs':>6}{'src A/D':>9}{'build file':>12}{'either':>9}{'root buildsys +/-':>19}")
    for kind in ("patch", "minor", "major", None):
        rs = [r for r in rows if kind is None or r["kind"] == kind]
        n = len(rs)
        print(f"{kind or 'all':<8}{n:>6}"
              f"{pct(sum(r['src_changed'] for r in rs), n):>9}"
              f"{pct(sum(r['build_changed'] for r in rs), n):>12}"
              f"{pct(sum(r['any_changed'] for r in rs), n):>9}"
              f"{pct(sum(r['buildsys_switch'] for r in rs), n):>19}")

    # The number that sizes the maintenance job: how many manifest-affecting
    # releases per package per year.
    per_repo = {}
    for r in rows:
        d = per_repo.setdefault(r["repo"], {"rel": 0, "src": 0, "any": 0})
        d["rel"] += 1
        d["src"] += r["src_changed"]
        d["any"] += r["any_changed"]
    if per_repo:
        rel = [d["rel"] / years for d in per_repo.values()]
        src = [d["src"] / years for d in per_repo.values()]
        anyc = [d["any"] / years for d in per_repo.values()]
        print("\nPer package per year (median / mean):")
        print(f"  releases                  {statistics.median(rel):5.1f} / {statistics.mean(rel):5.1f}")
        print(f"  with source add/delete    {statistics.median(src):5.1f} / {statistics.mean(src):5.1f}")
        print(f"  with source or build edit {statistics.median(anyc):5.1f} / {statistics.mean(anyc):5.1f}")
        print(f"\nAcross all {len(per_repo)} packages, per year:")
        print(f"  releases                  {sum(rel):6.0f}")
        print(f"  with source add/delete    {sum(src):6.0f}")
        print(f"  with source or build edit {sum(anyc):6.0f}")
        quiet = sum(1 for d in per_repo.values() if d["src"] == 0)
        print(f"\n{quiet} of {len(per_repo)} packages had no source add/delete "
              f"in the window.")

        print("\nMost churn (source add/delete releases / releases):")
        top = sorted(per_repo.items(), key=lambda kv: (-kv[1]["src"], kv[0]))[:10]
        for slug, d in top:
            print(f"  {slug:<34}{d['src']:>3} / {d['rel']}")

    if repos_skipped:
        print(f"\nSkipped {len(repos_skipped)}:")
        for slug, why in repos_skipped:
            print(f"  {slug}: {why.splitlines()[0][:90] if why else 'unknown'}")


def read_repos(path):
    out = []
    for line in Path(path).read_text().splitlines():
        line = line.split("#", 1)[0].strip()
        if line:
            out.append(line)
    return out


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--repos", default=str(Path(__file__).with_name("repos.txt")))
    ap.add_argument("--cache", required=True, help="where bare clones live")
    ap.add_argument("--since-years", type=float, default=3.0)
    ap.add_argument("--max-pairs", type=int, default=60, help="per repo")
    ap.add_argument("--jobs", type=int, default=6)
    ap.add_argument("--build-threshold", type=int, default=10,
                    help="build-file lines changed before it counts as a recipe change")
    ap.add_argument("--only", help="comma-separated slugs, overrides --repos")
    ap.add_argument("--out", help="write every pair as JSON lines")
    args = ap.parse_args()

    cache = Path(args.cache)
    cache.mkdir(parents=True, exist_ok=True)
    repos = args.only.split(",") if args.only else read_repos(args.repos)
    cutoff = int(time.time() - args.since_years * 365.25 * 86400)

    rows, ok, skipped = [], [], []
    with cf.ThreadPoolExecutor(max_workers=args.jobs) as ex:
        futs = {ex.submit(analyze_repo, s, cache, cutoff, args.max_pairs, args.build_threshold): s for s in repos}
        for i, fut in enumerate(cf.as_completed(futs), 1):
            slug = futs[fut]
            try:
                got = fut.result()
            except Exception as e:  # one bad repo must not sink the run
                skipped.append((slug, str(e)))
                print(f"[{i}/{len(repos)}] {slug}: SKIP", file=sys.stderr)
                continue
            if got:
                rows.extend(got)
                ok.append(slug)
            else:
                skipped.append((slug, "no release pairs in window"))
            print(f"[{i}/{len(repos)}] {slug}: {len(got)} pairs", file=sys.stderr)

    if args.out:
        with open(args.out, "w") as f:
            for r in sorted(rows, key=lambda r: (r["repo"], r["ts"])):
                f.write(json.dumps(r) + "\n")
    summarize(rows, ok, sorted(skipped), args.since_years)


if __name__ == "__main__":
    main()
