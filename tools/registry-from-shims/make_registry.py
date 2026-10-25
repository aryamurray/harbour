#!/usr/bin/env python3
"""Build a local, git-backed Harbour registry holding real packages.

Each entry is what a registry generator must emit for a real C library:

  * a shim pinning the upstream *git* source by full commit SHA,
  * a patch that adds the shim's `Harbour.toml` (and any overlay files, such as
    libpng's prebuilt `pnglibconf.h`, which upstream's .gitignore hides) to the
    pinned tree,
  * dependencies written as plain registry requirements (`zlib = "^1.3"`), which
    mean "from this registry",
  * the tier-1 index, produced by `harbour registry index` (which fetches each
    source, applies its patch, and reads the dependencies out of the result).

Nothing from upstream is vendored: the registry holds a pin and a patch per
package. The manifests are in `manifests/` (zlib's is the CI canary's).

usage: make_registry.py <new-registry-dir> <harbour-binary> [--cache DIR] [--tarball NAME,...] [--only NAME,...]

By default every package pins its upstream *git* source. `--tarball zlib` pins
zlib's release tarball instead (url + sha256 + strip_prefix, the same pin the CI
canary uses) and carries the manifest as a patch applied to the extracted tree:
the shape every package whose release tarball differs from its git tag needs.
`--only libpng,zlib` publishes just those packages (their dependencies must be
included too).

Needs network on first use (upstream clones are cached under --cache, default
`<new-registry-dir>/../.upstream-cache`). Then, for example:

    HARBOUR_TEST_REGISTRY_URL=file://<new-registry-dir> harbour add libpng
"""
import hashlib
import os
import shutil
import subprocess
import sys
import tarfile
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, "..", ".."))

# (name, version, upstream git url, tag, the commit that tag points at, manifest, overlay)
# `overlay` maps a file added to the tree -> the upstream file it is copied from.
PACKAGES = [
    ("zlib", "1.3.1", "https://github.com/madler/zlib.git", "v1.3.1",
     "51b7f2abdade71cd9bb0e7a373ef2610ec6f9daf",
     os.path.join(REPO, "ci/canary/zlib/Harbour.toml"), {}),
    ("libpng", "1.6.50", "https://github.com/pnggroup/libpng.git", "v1.6.50",
     "2b978915d82377df13fcbb1fb56660195ded868a",
     os.path.join(HERE, "manifests/libpng.Harbour.toml"),
     {"pnglibconf.h": "scripts/pnglibconf.h.prebuilt"}),
    ("freetype", "2.14.3", "https://gitlab.freedesktop.org/freetype/freetype.git", "VER-2-14-3",
     "0a0221a1347e2f1e07c395263540026e9a0aa7c7",
     os.path.join(HERE, "manifests/freetype.Harbour.toml"), {}),
    ("harfbuzz", "15.0.0", "https://github.com/harfbuzz/harfbuzz.git", "15.0.0",
     "35f656a1a2bab548ea7f54a6dd3fba2b09680d14",
     os.path.join(HERE, "manifests/harfbuzz.Harbour.toml"), {}),
    # Tarball-only in practice: the release tarball is what the canary pins, and the
    # git tree is not the same thing (see `TARBALLS`). The rev is the commit the tag
    # points at, recorded for the day a git entry is wanted. The manifest is the CI
    # canary's own, so a change to it is a change to this entry.
    ("openssl", "3.5.4", "https://github.com/openssl/openssl.git", "openssl-3.5.4",
     "c1eeb9406b6142148f267594197d853403d10208",
     os.path.join(REPO, "ci/canary/openssl/Harbour.toml"), {}),
]


# Packages that can be published from their release tarball instead of git:
# name -> (url, sha256, strip_prefix). The pins are the CI canaries' own.
TARBALLS = {
    "zlib": ("https://github.com/madler/zlib/releases/download/v1.3.1/zlib-1.3.1.tar.gz",
             "9a93b2b7dfdac77ceba5a558a580e74667dd6fede4585b91eefb60f03b72df23",
             "zlib-1.3.1"),
    "openssl": ("https://github.com/openssl/openssl/releases/download/openssl-3.5.4/openssl-3.5.4.tar.gz",
                "967311f84955316969bdb1d8d4b983718ef42338639c621ec4c34fddef355e99",
                "openssl-3.5.4"),
}


def fetch_tarball(url, sha256, cache, name):
    """The tarball's bytes on disk, downloaded once and checked against the pin."""
    path = os.path.join(cache, f"{name}.tar.gz")
    if not os.path.exists(path):
        # curl, as the canaries use: a Python from python.org on macOS has no CA
        # bundle, and the checksum below is what actually authenticates the bytes.
        sh("curl", "-fsSL", "--retry", "3", "-o", path, url)
    with open(path, "rb") as f:
        actual = hashlib.sha256(f.read()).hexdigest()
    if actual != sha256:
        sys.exit(f"{name}: {url} is now {actual}, but the shim pins {sha256}")
    return path


def sh(*args, cwd=None):
    r = subprocess.run(args, cwd=cwd, capture_output=True, text=True)
    if r.returncode:
        sys.exit(f"{' '.join(args)}\n{r.stdout}{r.stderr}")
    return r.stdout.strip()


def git(cwd, *args):
    return sh("git", "-c", "user.name=registry", "-c", "user.email=registry@example.invalid", *args, cwd=cwd)


def publish_from_tarball(out, cache, name, version, manifest, overlay):
    """Add `name` to the registry as a tarball source plus a patch."""
    tar_url, tar_sha, strip = TARBALLS[name]
    archive = fetch_tarball(tar_url, tar_sha, cache, name)

    work = tempfile.mkdtemp(prefix=name + "-")
    tree = os.path.join(work, "tree")
    os.makedirs(tree)
    with tarfile.open(archive) as tar:
        for member in tar.getmembers():
            # Mirror the transport's `strip_prefix`: drop the leading directory.
            if member.name == strip or not member.name.startswith(strip + "/"):
                continue
            member.name = member.name[len(strip) + 1:]
            tar.extract(member, tree)
    # A baseline commit, so the patch is a `git diff` against exactly the tree the
    # transport will extract (never hand-written).
    git(tree, "init", "-q")
    git(tree, "add", "-A")
    git(tree, "commit", "-q", "-m", "upstream tarball")
    shutil.copy(manifest, os.path.join(tree, "Harbour.toml"))
    for dest, src in overlay.items():
        shutil.copy(os.path.join(tree, src), os.path.join(tree, dest))
    sh("git", "add", "-f", "-N", "Harbour.toml", *overlay.keys(), cwd=tree)
    patch = sh("git", "diff", "--binary", cwd=tree) + "\n"
    shutil.rmtree(work)

    directory = os.path.join(out, "index", name[0], name)
    os.makedirs(directory)
    patch_file = f"{version}.patch"
    with open(os.path.join(directory, patch_file), "w") as f:
        f.write(patch)
    digest = hashlib.sha256(patch.encode()).hexdigest()
    with open(os.path.join(directory, f"{version}.toml"), "w") as f:
        f.write(f'[package]\nname = "{name}"\nversion = "{version}"\n\n'
                f'[source.tarball]\nurl = "{tar_url}"\nsha256 = "{tar_sha}"\n'
                f'strip_prefix = "{strip}"\n\n'
                f'[[patches]]\nfile = "{patch_file}"\nsha256 = "{digest}"\n')


def main():
    if len(sys.argv) < 3:
        sys.exit(__doc__)
    out, harbour = os.path.abspath(sys.argv[1]), sys.argv[2]
    cache = (os.path.abspath(sys.argv[sys.argv.index("--cache") + 1]) if "--cache" in sys.argv
             else os.path.join(os.path.dirname(out), ".upstream-cache"))
    if os.path.exists(out):
        sys.exit(f"{out} exists; pick a fresh directory")
    os.makedirs(os.path.join(out, "index"))
    os.makedirs(cache, exist_ok=True)
    # Patches are verified by the sha256 of their exact bytes and the registry is
    # cloned with git; where git converts line endings on checkout (Git for
    # Windows does by default) every patch would fail verification.
    with open(os.path.join(out, ".gitattributes"), "w") as f:
        f.write("# Patches are hashed by their exact bytes; never convert their line endings.\n"
                "*.patch -text\n")
    with open(os.path.join(out, "config.toml"), "w") as f:
        f.write('[registry]\nname = "from-shims"\nregistry_version = 1\n'
                'layout = "letter/name/version"\ndefault_branch = "main"\n')

    use_tarball = set()
    if "--tarball" in sys.argv:
        use_tarball = set(sys.argv[sys.argv.index("--tarball") + 1].split(","))
        unknown = use_tarball - set(TARBALLS)
        if unknown:
            sys.exit(f"no tarball pin for: {', '.join(sorted(unknown))}")

    only = (set(sys.argv[sys.argv.index("--only") + 1].split(","))
            if "--only" in sys.argv else None)

    for name, version, url, tag, rev, manifest, overlay in PACKAGES:
        if only is not None and name not in only:
            continue
        if name in use_tarball:
            publish_from_tarball(out, cache, name, version, manifest, overlay)
            continue
        mirror = os.path.join(cache, name)
        if not os.path.exists(mirror):
            sh("git", "clone", "--quiet", "--depth", "1", "--branch", tag, url, mirror)
        head = sh("git", "rev-parse", "HEAD", cwd=mirror)
        if head != rev:
            sys.exit(f"{name}: tag {tag} is now {head}, but the shim pins {rev}")

        work = tempfile.mkdtemp(prefix=name + "-")
        tree = os.path.join(work, "tree")
        sh("git", "clone", "--quiet", mirror, tree)
        shutil.copy(manifest, os.path.join(tree, "Harbour.toml"))
        for dest, src in overlay.items():
            shutil.copy(os.path.join(tree, src), os.path.join(tree, dest))
        # `-f -N`: intent-to-add, even for a file upstream's .gitignore hides.
        sh("git", "add", "-f", "-N", "Harbour.toml", *overlay.keys(), cwd=tree)
        patch = sh("git", "diff", "--binary", cwd=tree) + "\n"
        shutil.rmtree(work)

        directory = os.path.join(out, "index", name[0], name)
        os.makedirs(directory)
        patch_file = f"{version}.patch"
        with open(os.path.join(directory, patch_file), "w") as f:
            f.write(patch)
        digest = hashlib.sha256(patch.encode()).hexdigest()
        with open(os.path.join(directory, f"{version}.toml"), "w") as f:
            f.write(f'[package]\nname = "{name}"\nversion = "{version}"\n\n'
                    f'[source.git]\nurl = "{url}"\nrev = "{rev}"\n\n'
                    f'[[patches]]\nfile = "{patch_file}"\nsha256 = "{digest}"\n')

    git(out, "init", "-q", "-b", "main")
    git(out, "add", "-A")
    git(out, "commit", "-q", "-m", "shims")
    print(sh(harbour, "registry", "index", out))
    git(out, "add", "-A")
    git(out, "commit", "-q", "-m", "index")
    print("registry at", out)


if __name__ == "__main__":
    main()
