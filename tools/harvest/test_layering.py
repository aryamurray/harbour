#!/usr/bin/env python3
"""Tests for `merge`'s platform layering.

Run: python3 tools/harvest/test_layering.py

Layering is the part of `merge` that decides which `when` block each source
and define lands in, and it is the part that fails invisibly: a manifest keyed
one axis too narrowly still builds every platform that was harvested, and only
breaks on the platform nobody tried. Both shapes below are real -- openssl's
deltas cut across OS along the architecture axis, libuv's cut across
architecture along the OS axis -- and a scheme that handles one and not the
other looks correct until the next package.
"""

from __future__ import annotations

import sys
import pathlib

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import harvest  # noqa: E402


def h(os_name, arch, sources, defines, target="lib"):
    return {
        "platform": {"os": os_name, "arch": arch},
        "backend": "test",
        "targets": {target: {
            "sources": sources,
            "defines": defines,
            "include_dirs": ["include"],
        }},
        "external_include_dirs": [],
        "generated": [],
    }


def blocks(text: str) -> list[str]:
    """The `when` blocks of a merged manifest, as raw text."""
    return [b for b in text.split("[[targets.lib.when]]")[1:]]


def block_with(text: str, *conditions: str) -> str:
    """The single `when` block whose conditions are exactly `conditions`."""
    matches = []
    for b in blocks(text):
        conds = [
            line.strip() for line in b.strip().splitlines()
            if line.startswith(("os =", "arch =", "env =", "compiler ="))
        ]
        if conds == list(conditions):
            matches.append(b)
    assert len(matches) == 1, (
        f"expected exactly one block conditioned on {list(conditions)}, "
        f"found {len(matches)}\n{text}"
    )
    return matches[0]


def test_arch_axis_openssl_shaped():
    """Assembly shared by two OSes on one arch belongs to an `arch` layer."""
    arm_asm = ["crypto/aesv8-armx.S", "crypto/sha1-armv8.S"]
    text = harvest.merge(
        [
            h("macos", "aarch64", ["a.c", "b.c", *arm_asm, "mac_only.c"],
              ["SHARED=1", "ARMV8=1"]),
            h("linux", "aarch64", ["a.c", "b.c", *arm_asm], ["SHARED=1", "ARMV8=1"]),
            h("linux", "x86_64", ["a.c", "b.c", "crypto/aesni-x86_64.S"],
              ["SHARED=1", "AESNI=1"]),
        ],
        package="p", version="1.0.0",
    )

    arm = block_with(text, 'arch = "aarch64"')
    for src in arm_asm:
        assert src in arm, f"{src} should be in the arch layer:\n{text}"
        assert text.count(src) == 1, f"{src} emitted more than once:\n{text}"
    assert "ARMV8=1" in arm

    # What is genuinely per-platform stays per-platform.
    assert "mac_only.c" in block_with(text, 'os = "macos"', 'arch = "aarch64"')
    assert "aesni-x86_64.S" in block_with(text, 'os = "linux"', 'arch = "x86_64"')


def test_os_axis_libuv_shaped():
    """Sources shared by two arches on one OS belong to an `os` layer.

    Without this the two Linux blocks are keyed `os`+`arch`, and a third Linux
    architecture matches neither -- it compiles the intersection and fails to
    link on the event loop, which is exactly what the arch case above is
    written to avoid.
    """
    linux_only = ["src/unix/linux.c", "src/unix/procfs-exepath.c"]
    text = harvest.merge(
        [
            h("macos", "aarch64", ["core.c", "src/unix/kqueue.c"],
              ["SHARED=1", "_DARWIN_USE_64_BIT_INODE=1"]),
            h("linux", "aarch64", ["core.c", *linux_only], ["SHARED=1", "_GNU_SOURCE"]),
            h("linux", "x86_64", ["core.c", *linux_only], ["SHARED=1", "_GNU_SOURCE"]),
        ],
        package="p", version="1.0.0",
    )

    linux = block_with(text, 'os = "linux"')
    for src in linux_only:
        assert src in linux, f"{src} should be in the os layer:\n{text}"
        assert text.count(src) == 1, f"{src} emitted more than once:\n{text}"
    assert "_GNU_SOURCE" in linux
    # No architecture is named in the Linux layer, so an unharvested Linux
    # architecture still matches it.
    assert "arch" not in linux

    assert "src/unix/kqueue.c" in block_with(text, 'os = "macos"', 'arch = "aarch64"')


def test_common_is_the_intersection():
    """Anything every platform compiles stays out of the `when` blocks."""
    text = harvest.merge(
        [
            h("macos", "aarch64", ["core.c"], ["SHARED=1"]),
            h("linux", "x86_64", ["core.c"], ["SHARED=1"]),
        ],
        package="p", version="1.0.0",
    )
    assert "'core.c'" in text.split("[[targets.lib.when]]")[0]
    assert blocks(text) == [], f"nothing should be conditional here:\n{text}"


def test_single_platform_needs_no_layer():
    text = harvest.merge(
        [h("linux", "x86_64", ["core.c", "x.S"], ["SHARED=1"])],
        package="p", version="1.0.0",
    )
    assert blocks(text) == [], f"one platform is all unconditional:\n{text}"


def test_generated_sources_are_refused():
    """A harvest with unresolved generated objects must not silently merge."""
    bad = h("linux", "x86_64", ["core.c"], ["SHARED=1"])
    bad["generated"] = [
        {"object": "crypto/aes.o", "expected_source": "crypto/aes.s",
         "generated_from": ["crypto/asm/aes.pl"]}
    ]
    try:
        harvest.merge([bad], package="p", version="1.0.0")
    except SystemExit as e:
        assert "do not exist yet" in str(e), str(e)
    else:
        raise AssertionError("merge accepted a harvest with generated sources")


def main() -> int:
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_")]
    failed = 0
    for t in tests:
        try:
            t()
        except AssertionError as e:
            failed += 1
            print(f"FAIL {t.__name__}: {e}")
        else:
            print(f"ok   {t.__name__}")
    print(f"{len(tests) - failed}/{len(tests)} passed")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
