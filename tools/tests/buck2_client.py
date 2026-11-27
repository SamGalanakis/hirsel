#!/usr/bin/env python3
"""The installed Buck entry point refuses stale inventories before invoking the pinned Buck client."""
import argparse
from contextlib import redirect_stdout
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

SCRIPTS = Path(__file__).resolve().parents[1] / 'buck2'
sys.path.insert(0, str(SCRIPTS))
import bootstrap
import generated


class BuckClient(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        scripts = self.root / 'tools/buck2'
        scripts.mkdir(parents=True)
        for name in ('client.py', 'generated.py', 'bootstrap_llvm.py', 'bootstrap_store.py',
                     'llvm-cxx-headers-lock.json'):
            if (SCRIPTS / name).exists():
                shutil.copy(SCRIPTS / name, scripts / name)
        dist = scripts.parent / 'toolchain-dist'
        dist.mkdir()
        for name in ('product.py', 'product.toml'):
            shutil.copy(SCRIPTS.parent / 'toolchain-dist' / name, dist / name)
        # The fake client records exactly when Buck itself was reached.
        binary = '#!/bin/sh\nprintf "%s\\n" "$@" > "$(dirname "$0")/called"\nexit 17\n'
        state = self.root / '.buck2'
        (state / 'bin').mkdir(parents=True)
        pin = {'executable_sha256': hashlib.sha256(binary.encode()).hexdigest()}
        (scripts / 'pins.json').write_text(json.dumps({'buck2': pin}))
        # A release left by an earlier pin, which bootstrap removes.
        for name in ('buck2', f"buck2-{pin['executable_sha256']}", 'buck2-real'):
            file = state / 'bin' / name
            file.write_text(binary)
            file.chmod(0o755)
        with patch.object(bootstrap, 'ROOT', self.root), patch.object(bootstrap, 'PIN', pin), redirect_stdout(io.StringIO()):
            bootstrap.prepare_client(argparse.Namespace(client_only=True), state, -1)
        self.client = state / 'bin/buck2'
        self.called = state / 'bin/called'
        (self.root / 'source').mkdir()
        (self.root / 'source/a.rs').write_text('original')
        for part in generated.PARTS:
            reads = generated.Reads(self.root)
            reads.text(self.root / 'source/a.rs')
            reads.glob(self.root / 'source', '*.rs')
            for output in generated.PARTS[part]:
                file = self.root / output
                file.parent.mkdir(parents=True, exist_ok=True)
                file.write_text(output)
            generated.record(self.root, part, reads)
        # A minimal prepared LLVM tree, using the production receipt check.
        sys.path.insert(0, str(SCRIPTS))
        import bootstrap_llvm
        llvm = scripts / 'toolchains/llvm-files'
        for name in bootstrap_llvm.REQUIRED:
            file = llvm / name
            file.parent.mkdir(parents=True, exist_ok=True)
            file.touch()
        wanted = bootstrap_llvm.identity()
        self.llvm = llvm
        self.receipt = llvm / bootstrap_llvm.RECEIPT
        self.receipt.write_text(json.dumps(wanted))
        # identity() reads this lock relative to the copied checkout.
        lock = self.root / 'tools/native-toolchain/llvm-23.lock'
        lock.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy(bootstrap_llvm.LOCK, lock)

    def run_client(self, *args):
        return subprocess.run([str(self.client), *args], cwd=self.root, capture_output=True, text=True)

    def test_changed_or_added_input_refuses_before_buck(self):
        for change in ('edit', 'add'):
            with self.subTest(change=change):
                if change == 'edit':
                    (self.root / 'source/a.rs').write_text('changed')
                else:
                    (self.root / 'source/a.rs').write_text('original')
                    (self.root / 'source/b.rs').touch()
                result = self.run_client('--isolation-dir', 'test', 'build', '//:new-target')
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertIn('python3 tools/buck2/sync.py --if-stale', result.stderr)
                self.assertEqual(len(result.stderr.splitlines()), 1)
                self.assertFalse(self.called.exists())

    def test_fresh_build_and_test_preserve_arguments_without_regeneration(self):
        before = (self.root / generated.STAMP).read_bytes()
        for command in ('build', 'test'):
            args = ['--isolation-dir=build', command, '//:target', '--', 'argument with spaces']
            result = self.run_client(*args)
            self.assertEqual(result.returncode, 17, result.stderr)
            self.assertEqual(self.called.read_text().splitlines(), args)
            self.assertEqual((self.root / generated.STAMP).read_bytes(), before)

    def test_maintenance_commands_skip_checks_even_when_an_argument_is_build(self):
        (self.root / generated.STAMP).unlink()
        for command in ('kill', 'status', 'log'):
            result = self.run_client('--isolation-dir', 'test', command, 'build')
            self.assertEqual(result.returncode, 17, result.stderr)

    def test_a_changed_pin_refuses_the_installed_release(self):
        # An older release with the stale sweep must not run under a config made for the new pin.
        self.assertFalse((self.root / '.buck2/bin/buck2-real').exists())
        (self.root / 'tools/buck2/pins.json').write_text(json.dumps({'buck2': {'executable_sha256': 'f' * 64}}))
        for command in ('build', 'kill'):
            result = self.run_client(command, '//:target')
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertIn('python3 tools/buck2/prepare.py', result.stderr)
            self.assertFalse(self.called.exists())

    def test_bootstrap_failure_names_the_path_or_pin(self):
        import bootstrap_llvm

        required = self.llvm / bootstrap_llvm.REQUIRED[0]
        wanted = json.loads(self.receipt.read_text())
        actual_hash = '0' * 64
        for problem, diagnostic in (
            ('missing path', f'missing required LLVM path {required}'),
            ('mismatch', f"lock_sha256 mismatch (expected {wanted['lock_sha256']}, actual {actual_hash})"),
            ('malformed', f'malformed LLVM receipt {self.receipt}'),
        ):
            with self.subTest(problem=problem):
                if problem == 'missing path':
                    required.unlink()
                elif problem == 'mismatch':
                    required.touch()
                    self.receipt.write_text(json.dumps({**wanted, 'lock_sha256': actual_hash}))
                else:
                    self.receipt.write_text('{')
                result = self.run_client('test', '//:target')
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertIn(diagnostic, result.stderr)
                self.assertIn('python3 tools/buck2/prepare.py', result.stderr)
                self.assertEqual(len(result.stderr.splitlines()), 1)
                self.assertFalse(self.called.exists())


class PrivateLlvmTree(unittest.TestCase):
    """The compiler trusts the LLVM tree's files by stat, so another checkout's install must move none of it."""

    def test_another_checkout_installing_the_same_pin_moves_no_stat_of_this_tree(self):
        import bootstrap_llvm

        def build(stage, wanted):
            (stage / 'lib').mkdir(parents=True)
            (stage / 'lib/libLLVM.so.23.1').write_bytes(b'llvm')

        def status(path):
            s = os.stat(path)
            return s.st_dev, s.st_ino, s.st_nlink, s.st_size, s.st_mode, s.st_mtime_ns, s.st_ctime_ns

        with tempfile.TemporaryDirectory() as raw, patch.dict(os.environ, HIRSEL_BUCK2_STORE=str(Path(raw) / 'store')), \
                patch.object(bootstrap_llvm, 'build', build):
            first, second = Path(raw) / 'one/llvm-files', Path(raw) / 'two/llvm-files'
            bootstrap_llvm.install(first, {'pin': 1})
            before = status(first / 'lib/libLLVM.so.23.1')
            bootstrap_llvm.install(second, {'pin': 1})
            self.assertEqual(status(first / 'lib/libLLVM.so.23.1'), before)
            self.assertTrue(bootstrap_llvm.private(first))
            # A tree that already shares an inode with another checkout is stale.
            os.link(second / 'lib/libLLVM.so.23.1', Path(raw) / 'shared')
            self.assertFalse(bootstrap_llvm.private(second))


if __name__ == '__main__':
    unittest.main()
