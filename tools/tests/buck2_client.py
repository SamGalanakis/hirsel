#!/usr/bin/env python3
"""The installed Buck entry point refuses stale inventories before invoking the pinned Buck client."""
import argparse
from contextlib import redirect_stdout
import hashlib
import io
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
                     'llvm-cxx-headers-lock.json', 'llvm-runtime-lock.json'):
            if (SCRIPTS / name).exists():
                shutil.copy(SCRIPTS / name, scripts / name)
        # The fake client records exactly when Buck itself was reached.
        binary = '#!/bin/sh\nprintf "%s\\n" "$@" > "$(dirname "$0")/called"\nexit 17\n'
        state = self.root / '.buck2'
        (state / 'bin').mkdir(parents=True)
        for name in ('buck2', 'buck2-real'):
            file = state / 'bin' / name
            file.write_text(binary)
            file.chmod(0o755)
        pin = {'executable_sha256': hashlib.sha256(binary.encode()).hexdigest()}
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
        import json
        wanted = bootstrap_llvm.identity()
        (llvm / bootstrap_llvm.RECEIPT).write_text(json.dumps(wanted))
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

    def test_missing_headers_name_prepare(self):
        header = self.root / 'tools/buck2/toolchains/llvm-files/sysroot/usr/include/c++/13/vector'
        header.unlink()
        result = self.run_client('test', '//:target')
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn('python3 tools/buck2/prepare.py', result.stderr)
        self.assertEqual(len(result.stderr.splitlines()), 1)
        self.assertFalse(self.called.exists())


if __name__ == '__main__':
    unittest.main()
