"""Controls for result measurements rejecting checked runtime artifacts."""
import hashlib
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import measurement_artifacts as run


class ToolchainProfileTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.compiler = self.root / 'compiler'
        self.compiler.write_bytes(b'release compiler fixture')
        self.checked = self.root / 'checked.a'
        self.checked.write_bytes(b'checked runtime fixture')
        self.ordinary = self.root / 'ordinary-release.a'
        self.ordinary.write_bytes(b'ordinary release O3 sixteen CGUs fixture')
        self.shipping = self.root / 'shipping.a'
        self.shipping.write_bytes(b'shipping fat-LTO runtime fixture')
        view = self.root / 'buck-out/v2/art/root/profile/__hirsel-core-rt-archives__/hirsel-core-rt-archives/x86_64-unknown-linux-gnu/libhirsel_core_rt.a'
        view.parent.mkdir(parents=True)
        view.symlink_to(self.shipping)
        self.view = view
        self.addCleanup(patch.stopall)
        patch.object(run, 'ROOT', self.root).start()

    def test_checked_archive_refused_even_with_release_compiler_label(self):
        with self.assertRaisesRegex(ValueError, 'shipping fat-LTO runtime'):
            run.identity(self.compiler, self.checked, 'release', 'checked')

    def test_ordinary_release_staticlib_refused(self):
        with self.assertRaisesRegex(ValueError, 'shipping fat-LTO runtime'):
            run.identity(self.compiler, self.ordinary, 'release', 'ordinary-release')

    def test_shipping_view_and_resolved_archive_record_same_identity(self):
        left = run.identity(self.compiler, self.view, 'release')
        right = run.identity(self.compiler, self.shipping, 'release')
        self.assertEqual(left, right)
        self.assertEqual(left['runtime_archive_profile'], 'shipping-release-fat-lto')
        self.assertEqual(left['runtime_archive_sha256'], hashlib.sha256(self.shipping.read_bytes()).hexdigest())
        self.assertFalse(left['diagnostic'])

    def test_checked_compiler_refused(self):
        with self.assertRaises(ValueError):
            run.identity(self.compiler, self.shipping, 'checked')

    def test_explicit_diagnostic_retains_checked_identity(self):
        value = run.identity(self.compiler, self.checked, 'checked', 'checked', True)
        self.assertTrue(value['diagnostic'])
        self.assertEqual(value['compiler_build_profile'], 'checked')
        self.assertEqual(value['runtime_archive_profile'], 'checked')
        self.assertEqual(value['runtime_archive_sha256'], hashlib.sha256(self.checked.read_bytes()).hexdigest())

    def test_replaced_archive_has_different_identity(self):
        before = run.identity(self.compiler, self.shipping, 'release')
        self.shipping.write_bytes(b'changed shipping archive')
        after = run.identity(self.compiler, self.shipping, 'release')
        self.assertNotEqual(before, after)


if __name__ == '__main__':
    unittest.main()
