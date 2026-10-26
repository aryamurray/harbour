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

Needs Python 3.11 or newer (it reads manifests with `tomllib`).

usage: make_registry.py <new-registry-dir> <harbour-binary> [--cache DIR] [--tarball NAME,...] [--only NAME,...]

By default every package pins its upstream *git* source. `--tarball zlib` pins
zlib's release tarball instead (url + sha256 + strip_prefix, the same pin the CI
canary uses) and carries the manifest as a patch applied to the extracted tree:
the shape every package whose release tarball differs from its git tag needs.
`--only libpng,zlib` publishes just those packages (their dependencies must be
included too). cjson, libuv, zstd and curl have no git pin at all: they are always
published from their release tarball.

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
import tomllib
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, "..", ".."))

def cpython_overlay():
    """CPython's overlay: the generator scripts (shared with the CI canary), and the
    pieces that replace its three vendored pyconfig.h copies. See
    cpython/derive.py for where the latter come from."""
    canary = os.path.join(REPO, "ci/canary/cpython/overlay/harbour")
    mine = os.path.join(HERE, "cpython")
    overlay = {f"harbour/{f}": os.path.join(canary, f)
               for f in ("config.sh", "freeze1.sh", "freeze2.sh")}
    overlay["harbour/pyconfig.sh"] = os.path.join(mine, "pyconfig.sh")
    overlay["harbour/empty.c"] = os.path.join(mine, "empty.c")
    for f in sorted(os.listdir(os.path.join(mine, "pyconfig"))):
        overlay[f"harbour/pyconfig/{f}"] = os.path.join(mine, "pyconfig", f)
    return overlay


# (name, version, upstream git url, tag, the commit that tag points at, manifest, overlay)
# `overlay` maps a file added to the tree -> the file it is copied from: a path
# relative to the upstream tree, or an absolute path of a file kept in this
# repository (see `overlay_dir`). A package published only from a tarball has
# `None` for the git fields.
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
    # Tarball-only packages (no git url/tag/rev): published from the release
    # tarball pinned in TARBALLS. The manifests are the CI canaries' own.
    ("cjson", "1.7.18", None, None, None,
     os.path.join(REPO, "ci/canary/cjson/Harbour.toml"), {}),
    ("libuv", "1.51.0", None, None, None,
     os.path.join(REPO, "ci/canary/libuv/Harbour.toml"), {}),
    ("zstd", "1.5.7", None, None, None,
     os.path.join(REPO, "ci/canary/zstd/Harbour.toml"), {}),
    ("curl", "8.22.0", None, None, None,
     os.path.join(REPO, "ci/canary/curl/Harbour.toml"), {}),
    ("cpython", "3.14.9", None, None, None,
     os.path.join(HERE, "manifests/cpython.Harbour.toml"),
     cpython_overlay()),
    ("openssl", "3.5.4", None, None, None,
     os.path.join(REPO, "ci/canary/openssl/Harbour.toml"), {}),
]


# Packages that can be published from their release tarball instead of git:
# name -> (url, sha256, strip_prefix). The pins are the CI canaries' own.
TARBALLS = {
    "zlib": ("https://github.com/madler/zlib/releases/download/v1.3.1/zlib-1.3.1.tar.gz",
             "9a93b2b7dfdac77ceba5a558a580e74667dd6fede4585b91eefb60f03b72df23",
             "zlib-1.3.1"),
    "cjson": ("https://github.com/DaveGamble/cJSON/archive/refs/tags/v1.7.18.tar.gz",
              "3aa806844a03442c00769b83e99970be70fbef03735ff898f4811dd03b9f5ee5",
              "cJSON-1.7.18"),
    "libuv": ("https://dist.libuv.org/dist/v1.51.0/libuv-v1.51.0.tar.gz",
              "5f0557b90b1106de71951a3c3931de5e0430d78da1d9a10287ebc7a3f78ef8eb",
              "libuv-v1.51.0"),
    "zstd": ("https://github.com/facebook/zstd/releases/download/v1.5.7/zstd-1.5.7.tar.gz",
             "eb33e51f49a15e023950cd7825ca74a4a2b43db8354825ac24fc1b7ee09e6fa3",
             "zstd-1.5.7"),
    "curl": ("https://github.com/curl/curl/releases/download/curl-8_22_0/curl-8.22.0.tar.gz",
             "d54dd598bf05927a726deb38df31c6a255ba83ff1de57c5d1464dac3ed8f44a1",
             "curl-8.22.0"),
    # python.org publishes MD5s and signatures, not SHA-256: this is the hash of
    # the file as downloaded, the same pin as ci/canary/cpython.
    "cpython": ("https://www.python.org/ftp/python/3.14.9/Python-3.14.9.tgz",
                "8b47e85ee67e93945e93620a051a8f44bfc1e9735ed67075d166f4a74b7a060b",
                "Python-3.14.9"),
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


def apply_overlay(tree, overlay):
    """Copy the overlay into `tree`; returns the paths added, for `git add -f -N`.

    `overlay` is a dict {dest: src} (src relative to the tree, or absolute for a
    file kept in this repository) or "overlay_dir:<dir>", meaning every file under
    <dir> lands at the same relative path.
    """
    if isinstance(overlay, str) and overlay.startswith("overlay_dir:"):
        base = overlay[len("overlay_dir:"):]
        overlay = {os.path.relpath(os.path.join(root, f), base): os.path.join(root, f)
                   for root, _, files in os.walk(base) for f in files}
    for dest, src in overlay.items():
        src = src if os.path.isabs(src) else os.path.join(tree, src)
        os.makedirs(os.path.dirname(os.path.join(tree, dest)) or tree, exist_ok=True)
        shutil.copy(src, os.path.join(tree, dest))
    return sorted(overlay)


def sh(*args, cwd=None):
    r = subprocess.run(args, cwd=cwd, capture_output=True, text=True)
    if r.returncode:
        sys.exit(f"{' '.join(args)}\n{r.stdout}{r.stderr}")
    return r.stdout.strip()


def git(cwd, *args):
    return sh("git", "-c", "user.name=registry", "-c", "user.email=registry@example.invalid", *args, cwd=cwd)


def metadata_block(manifest, upstream_url=None):
    """The shim's `[metadata]` table: the upstream license, which a registry entry must declare.

    Read from the manifest the entry ships, so the two cannot disagree; a manifest
    with no `license` stops the publish rather than producing an entry the index
    step would reject later.
    """
    with open(manifest, "rb") as f:
        license = tomllib.load(f).get("package", {}).get("license")
    if not license:
        sys.exit(f"{manifest}: [package] has no `license` (an SPDX expression); registry entries must declare one")
    block = f'[metadata]\nlicense = "{license}"\n'
    if upstream_url:
        block += f'upstream_url = "{upstream_url}"\n'
    return block + "\n"


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
    added = apply_overlay(tree, overlay)
    sh("git", "add", "-f", "-N", "Harbour.toml", *added, cwd=tree)
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
                f'{metadata_block(manifest)}'
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
        if name in use_tarball or url is None:
            publish_from_tarball(out, cache, name, version, manifest, overlay)
            continue
        if url is None:
            sys.exit(f"{name} has no git pin: publish it with --tarball {name}")
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
        added = apply_overlay(tree, overlay)
        # `-f -N`: intent-to-add, even for a file upstream's .gitignore hides.
        sh("git", "add", "-f", "-N", "Harbour.toml", *added, cwd=tree)
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
                    f'{metadata_block(manifest, url)}'
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
