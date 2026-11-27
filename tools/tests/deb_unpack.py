#!/usr/bin/env python3
import bz2
import gzip
import io
import lzma
import os
from pathlib import Path
import stat
import sys
import tarfile
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "buck2/native-toolchain"))
from deb_unpack import unpack_deb


def ar_entry(name, data):
    header = f"{name:<16}{0:<12}{0:<6}{0:<6}{'100644':<8}{len(data):<10}`\n".encode()
    return header + data + (b"\n" if len(data) % 2 else b"")


def tar_bytes(entries):
    output = io.BytesIO()
    with tarfile.open(fileobj=output, mode="w:") as archive:
        for name, kind, value in entries:
            member = tarfile.TarInfo(name)
            member.type = kind
            if kind == tarfile.REGTYPE:
                member.mode = 0o751
                member.size = len(value)
                archive.addfile(member, io.BytesIO(value))
            else:
                member.linkname = value
                archive.addfile(member)
    return output.getvalue()


def deb(compression, layout, entries):
    control = tar_bytes([("./control", tarfile.REGTYPE, b"Package: sample\nVersion: 1\n"
                          b"Architecture: all\nMaintainer: Sample <sample@example.org>\n"
                          b"Description: Synthetic unpacker test\n")])
    package = b"!<arch>\n" + ar_entry("debian-binary/", b"2.0\n")
    package += ar_entry("_ignored/", b"a")  # Odd-sized skipped member.
    package += ar_entry("control.tar.gz/", gzip.compress(control, mtime=0))
    name = f"data.tar.{compression}"
    raw = tar_bytes(entries)
    compress = {"xz": lzma.compress, "bz2": bz2.compress, "gz": lambda raw: gzip.compress(raw, mtime=0)}
    data = compress[compression](raw)
    if layout == "gnu":
        package += ar_entry("//", f"name-of-another-member/\n{name}/\n".encode())
        return package + ar_entry("/24", data)
    if layout == "bsd":
        filename = name.encode() + b"\0" * 3
        return package + ar_entry(f"#1/{len(filename)}", filename + data)
    return package + ar_entry(name + "/", data)


class DebUnpack(unittest.TestCase):
    def unpack(self, directory, entries, compression="xz", layout="plain", **options):
        package = directory / "sample.deb"
        package.write_bytes(deb(compression, layout, entries))
        output = directory / "output"
        unpack_deb(package, output, **options)
        return output

    def test_compression_and_ar_layouts(self):
        entries = [
            ("./usr/bin/tool", tarfile.REGTYPE, b"tool contents"),
            ("./usr/lib/alias", tarfile.SYMTYPE, "../bin/tool"),
            ("./usr/bin/hardlink", tarfile.LNKTYPE, "./usr/bin/tool"),
        ]
        for compression, layout in (("xz", "plain"), ("gz", "gnu"), ("bz2", "bsd")):
            with self.subTest(compression=compression, layout=layout), tempfile.TemporaryDirectory() as scratch:
                output = self.unpack(Path(scratch), entries, compression, layout)
                tool = output / "usr/bin/tool"
                alias = output / "usr/lib/alias"
                self.assertEqual(tool.read_bytes(), b"tool contents")
                self.assertEqual(stat.S_IMODE(tool.stat().st_mode), 0o751)
                self.assertEqual(os.readlink(alias), "../bin/tool")
                self.assertEqual(alias.read_bytes(), b"tool contents")
                self.assertTrue(tool.samefile(output / "usr/bin/hardlink"))

    def test_escaping_paths_are_refused(self):
        cases = [
            ("../outside", tarfile.REGTYPE, b"changed"),
            ("usr/../inside", tarfile.REGTYPE, b"changed"),
            ("/outside", tarfile.REGTYPE, b"changed"),
            ("alias", tarfile.SYMTYPE, "../outside"),
            ("alias", tarfile.SYMTYPE, "/outside"),
            ("hardlink", tarfile.LNKTYPE, "../outside"),
        ]
        for entry in cases:
            with self.subTest(entry=entry), tempfile.TemporaryDirectory() as scratch:
                root = Path(scratch)
                outside = root / "outside"
                outside.write_bytes(b"outside")
                with self.assertRaises((ValueError, tarfile.FilterError)):
                    self.unpack(root, [entry])
                self.assertEqual(outside.read_bytes(), b"outside")

    def test_member_cannot_follow_a_preexisting_outside_symlink(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            output = root / "output"
            output.mkdir()
            (output / "alias").symlink_to("..")
            with self.assertRaises(tarfile.FilterError):
                self.unpack(root, [("alias/outside", tarfile.REGTYPE, b"escaped")])
            self.assertFalse((root / "outside").exists())

    def test_rooted_symlink_stays_in_package(self):
        with tempfile.TemporaryDirectory() as scratch:
            output = self.unpack(Path(scratch), [
                ("usr/lib/tool", tarfile.REGTYPE, b"tool"),
                ("usr/lib64/tool", tarfile.SYMTYPE, "/usr/lib/tool"),
            ], rooted_links=True)
            alias = output / "usr/lib64/tool"
            self.assertEqual(os.readlink(alias), "../lib/tool")
            self.assertEqual(alias.read_bytes(), b"tool")


if __name__ == "__main__":
    unittest.main()
