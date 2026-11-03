import importlib.util
import pathlib
import tempfile
import unittest
from unittest import mock

SPEC = importlib.util.spec_from_file_location("omb_remote_helper", pathlib.Path(__file__).resolve().parents[1] / "src" / "helper.py")
HELPER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(HELPER)


class RecursiveHelperTests(unittest.TestCase):
    def test_recursive_snapshot_and_shallow_mode(self):
        with tempfile.TemporaryDirectory(prefix="omb-tree-") as directory:
            root = pathlib.Path(directory)
            (root / "nested" / "deep").mkdir(parents=True)
            (root / "root.txt").write_text("root", encoding="utf-8")
            (root / "nested" / "deep" / "quoted ' name.txt").write_text("data", encoding="utf-8")
            (root / ".hbsync").mkdir()
            (root / ".hbsync" / "state.json").write_text("{}")
            files = HELPER.snapshot(directory)
            self.assertEqual({f["name"] for f in files}, {"root.txt", "nested/deep/quoted ' name.txt"})
            self.assertTrue(all(isinstance(f["mtime_ns"], int) for f in files))
            self.assertEqual([f["name"] for f in HELPER.snapshot(directory, False)], ["root.txt"])
            self.assertEqual(HELPER.safe_path(directory, "root.txt"), str(root / "root.txt"))

    def test_path_escape_and_nonregular_sources_are_rejected(self):
        with tempfile.TemporaryDirectory(prefix="omb-safe-path-") as directory:
            (pathlib.Path(directory) / "folder").mkdir()
            for name in ("../escape", "/absolute", "a/../b", "folder", "a\\b", ""):
                with self.subTest(name=name), self.assertRaises((ValueError, FileNotFoundError)):
                    HELPER.safe_path(directory, name)

    def test_incomplete_scan_raises_instead_of_returning_deletions(self):
        def denied(_root, followlinks, onerror):
            onerror(PermissionError("fixture denial"))
            return iter(())
        with mock.patch.object(HELPER.os, "walk", denied):
            with self.assertRaises(PermissionError):
                HELPER.snapshot("fixture")

    def test_source_links_are_not_followed(self):
        with tempfile.TemporaryDirectory(prefix="omb-links-") as directory:
            root = pathlib.Path(directory)
            (root / "real.txt").write_text("data")
            try:
                (root / "link.txt").symlink_to(root / "real.txt")
            except OSError:
                self.skipTest("host does not permit creating symlinks")
            with self.assertRaises(ValueError):
                HELPER.snapshot(directory)
            with self.assertRaises(ValueError):
                HELPER.safe_path(directory, "link.txt")


if __name__ == "__main__":
    unittest.main()
