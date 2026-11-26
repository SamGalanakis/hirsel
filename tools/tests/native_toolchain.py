#!/usr/bin/env python3
"""Check native archive links and engine measurement inputs."""
import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import sys
import unittest

sys.dont_write_bytecode = True

SPEC = importlib.util.spec_from_file_location('unpack_native', Path(__file__).resolve().parents[1] / 'buck2/native-toolchain/unpack.py')
unpack = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(unpack)


class ArchiveLinks(unittest.TestCase):
    def test_absolute_usrmerge_loader_comes_from_the_archive(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw) / 'archive'
            library = root / 'usr/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2'
            library.parent.mkdir(parents=True)
            library.write_bytes(b'pinned loader')
            loader = root / 'usr/lib64/ld-linux-x86-64.so.2'
            loader.parent.mkdir()
            loader.symlink_to('/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2')
            unpack.normalize_links(root)
            output = Path(raw) / 'output'
            unpack.copy_tree(root / 'usr', output)
            copied = output / 'lib64/ld-linux-x86-64.so.2'
            self.assertFalse(copied.is_symlink())
            self.assertEqual(copied.read_bytes(), b'pinned loader')

    def test_relative_in_archive_link_is_accepted_and_escape_is_rejected(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw) / 'archive'
            root.mkdir()
            (root / 'tool').write_bytes(b'pinned tool')
            (root / 'alias').symlink_to('tool')
            unpack.normalize_links(root)
            self.assertEqual((root / 'alias').read_bytes(), b'pinned tool')
            (root / 'escape').symlink_to('../../outside')
            with self.assertRaisesRegex(ValueError, 'escapes its root'):
                unpack.normalize_links(root)


class EngineMeasurement(unittest.TestCase):
    def test_refuses_a_missing_or_unavailable_measurement_cpu(self):
        script = Path(__file__).resolve().parents[1] / 'native-toolchain/engine_llvm.py'
        env = os.environ.copy()
        env.pop('HIRSEL_MEASURE_CPU', None)
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            for choice in ({}, {'HIRSEL_MEASURE_CPU': '9999'}):
                with self.subTest(choice=choice):
                    run = subprocess.run(
                        [sys.executable, script, 'guard', root / 'stock', root / 'candidate',
                         root / 'out', '--artifacts', root / 'artifacts.json'],
                        env=env | choice, capture_output=True, text=True,
                    )
                    self.assertNotEqual(run.returncode, 0)
                    self.assertIn('HIRSEL_MEASURE_CPU', run.stderr)


if __name__ == '__main__':
    unittest.main()
