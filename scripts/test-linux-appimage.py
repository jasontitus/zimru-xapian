#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Exercise Debian payload admission with small in-memory package descriptions."""
import importlib.util
import io
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("appimage", Path(__file__).with_name("linux-appimage.py"))
appimage = importlib.util.module_from_spec(spec)
spec.loader.exec_module(appimage)


class DebianPayload(unittest.TestCase):
    def read(self, source, entries):
        contents = io.BytesIO()
        with tarfile.open(fileobj=contents, mode="w") as archive:
            for name, kind in entries:
                item = tarfile.TarInfo(name)
                item.type = kind
                if kind == tarfile.SYMTYPE:
                    item.linkname = str(source)
                archive.addfile(item)
        contents.seek(0)
        with patch.object(appimage.subprocess, "Popen") as process:
            reader = process.return_value.__enter__.return_value
            reader.stdout = contents
            reader.wait.return_value = 0
            appimage.read_deb(Path("fixture.deb"), source.parent / "stage", source, source)

    def test_unknown_linked_duplicate_and_missing_members_preserve_source(self):
        cases = [
            ([("usr", tarfile.SYMTYPE)], "Unexpected Debian payload entry"),
            ([("AppRun", tarfile.SYMTYPE)], "Unexpected Debian payload entry"),
            ([("usr/share/extra", tarfile.REGTYPE)], "Unexpected Debian payload entry"),
            ([("usr/share/applications/org.kiwix.workbench.desktop", tarfile.REGTYPE)] * 2,
             "Unexpected Debian payload entry"),
            ([("usr", tarfile.DIRTYPE)], "Missing Debian payload files"),
        ]
        for members, message in cases:
            with self.subTest(members=members), tempfile.TemporaryDirectory() as tmp:
                source = Path(tmp) / "source"
                source.write_bytes(b"unchanged")
                with self.assertRaisesRegex(ValueError, message):
                    self.read(source, members)
                self.assertEqual(source.read_bytes(), b"unchanged")


if __name__ == "__main__":
    unittest.main()
