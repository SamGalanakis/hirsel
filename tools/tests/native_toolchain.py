#!/usr/bin/env python3
"""Check native archive links and engine measurement inputs."""
from contextlib import redirect_stderr, redirect_stdout
import importlib.util
import io
import json
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
    def test_warm_guard_uses_measured_band_for_both_baselines(self):
        script = Path(__file__).resolve().parents[1] / 'native-toolchain/engine_llvm.py'
        spec = importlib.util.spec_from_file_location('engine_llvm', script)
        engine = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(engine)

        # Each library varies by a factor of 21/20. The combined factor is 1.1025.
        # The previous pin's upper bound is 0.98 * 1.1025 = 1.08045.
        cases = [
            (420, 'same', 'inconclusive', 'inconclusive', 0),  # 1.05
            (440, 'same', 'inconclusive', 'worse', 1),        # 1.10
            (460, 'same', 'worse', 'worse', 1),               # 1.15
            (340, 'same', 'better', 'better', 0),             # 0.85
            (420, 'changed', 'inconclusive', 'inconclusive', 1),
        ]
        with tempfile.TemporaryDirectory() as raw:
            out = Path(raw)
            for warm, text, stock_verdict, pin_verdict, status in cases:
                with self.subTest(warm=warm, text=text):
                    sides = {
                        'stock': {'case': dict(warm=400, warm_repeat=420, cold=800, text='same')},
                        'candidate': {'case': dict(warm=warm, warm_repeat=warm * 21 // 20, cold=800, text=text)},
                    }
                    stdout = io.StringIO()
                    with redirect_stdout(stdout), redirect_stderr(io.StringIO()):
                        self.assertEqual(engine.report_guard(sides, out, 0.98), status)
                    comparisons = json.loads((out / 'guard.json').read_text())['warm_comparisons']
                    self.assertEqual(comparisons['stock']['verdict'], stock_verdict)
                    self.assertEqual(comparisons['previous pin']['verdict'], pin_verdict)
                    self.assertAlmostEqual(comparisons['stock']['upper'], 1.1025)
                    self.assertAlmostEqual(comparisons['previous pin']['upper'], 1.08045)
                    self.assertIn(f'{stock_verdict} (band [', stdout.getvalue())

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
