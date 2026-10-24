import unittest

import churn


class ParseVersion(unittest.TestCase):
    def test_accepts_common_tag_shapes(self):
        for tag, want in [
            ("v1.2.3", (1, 2, 3)),
            ("1.2.13", (1, 2, 13)),
            ("curl-8_5_0", (8, 5, 0)),
            ("REL_16_1", (16, 1)),
            ("release-3.4", (3, 4)),
            ("version-3.45.0", (3, 45, 0)),
            ("zlib-1.2.11.1", (1, 2, 11, 1)),
            ("pcre2-10.49", (10, 49)),
            ("VER-2-15-0", (2, 15, 0)),
            ("json-c-0.19-20260627", (0, 19)),
            ("release-2.1.13-stable", (2, 1, 13)),
            ("n6.1", (6, 1)),
        ]:
            self.assertEqual(churn.parse_version(tag), want, tag)

    def test_rejects_non_final_and_floating_tags(self):
        for tag in ["v1.2.3-rc1", "v2.0.0-beta", "v2", "OpenSSL_1_1_1w",
                    "nightly", "v1.2.3+build", "latest", "pcre2-10.48-RC1",
                    "release-2.2.2-alpha", "v2.1.ROLLING"]:
            self.assertIsNone(churn.parse_version(tag), tag)


class BumpKind(unittest.TestCase):
    def test_kinds(self):
        self.assertEqual(churn.bump_kind((1, 2, 3), (1, 2, 4)), "patch")
        self.assertEqual(churn.bump_kind((1, 2, 3), (1, 3, 0)), "minor")
        self.assertEqual(churn.bump_kind((1, 9, 9), (2, 0, 0)), "major")
        self.assertEqual(churn.bump_kind((1, 2), (1, 2, 1)), "patch")


class ClassifyChanges(unittest.TestCase):
    def run_one(self, *changes):
        return churn.classify_changes(changes)

    def test_source_added_is_a_source_change(self):
        r = self.run_one(("A", "src/new.c"), ("M", "src/old.c"))
        self.assertTrue(r["src_changed"])
        self.assertEqual(r["src_added"], 1)

    def test_modified_source_is_not(self):
        r = self.run_one(("M", "src/old.c"), ("M", "include/x.h"))
        self.assertFalse(r["src_changed"])
        self.assertFalse(r["build_touched"])

    def test_noise_dirs_and_files_ignored(self):
        r = self.run_one(
            ("A", "tests/new.c"), ("A", "examples/demo/main.c"),
            ("D", "src/foo_test.c"), ("A", "deps/lua/CMakeLists.txt"),
            ("A", "docs/Makefile"),
        )
        self.assertFalse(r["src_changed"])
        self.assertFalse(r["build_touched"])
        self.assertEqual(r["files_changed"], 5)

    def test_build_file_edit(self):
        r = self.run_one(("M", "CMakeLists.txt"), ("M", "cmake/Foo.cmake"))
        self.assertEqual(r["build_paths"], ["CMakeLists.txt", "cmake/Foo.cmake"])
        self.assertFalse(r["src_changed"])

    def test_buildsys_switch_only_at_root_add_or_delete(self):
        self.assertTrue(self.run_one(("A", "meson.build"))["buildsys_switch"])
        self.assertFalse(self.run_one(("M", "meson.build"))["buildsys_switch"])
        self.assertFalse(self.run_one(("A", "sub/meson.build"))["buildsys_switch"])

    def test_assembly_counts_as_source(self):
        self.assertTrue(self.run_one(("A", "lib/decompress/huf_x86_64.S"))["src_changed"])


if __name__ == "__main__":
    unittest.main()
