#!/usr/bin/env python3
"""Exercise the affected entry point with real Git diffs and recorded Buck commands."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

TOOLS = Path(__file__).resolve().parents[1]


class AffectedPackages(unittest.TestCase):
    def setUp(self):
        scratch = tempfile.TemporaryDirectory()
        self.addCleanup(scratch.cleanup)
        self.root = Path(scratch.name)
        self.write('tools/affected', (TOOLS / 'affected').read_text()).chmod(0o755)
        self.write('tools/consumers', '''#!/bin/sh
if [ -n "$FIXTURE_TEST" ]; then
  echo "$FIXTURE_TEST"
else
  for argument do [ "$argument" = --allow-empty ] && exit 0; done
  exit 2
fi
''').chmod(0o755)
        self.write('.buck2/bin/buck2', '''#!/usr/bin/env python3
import json, os, sys
with open('.buck2/commands', 'a') as log:
    print(json.dumps(sys.argv[1:]), file=log)
if sys.argv[1] == 'uquery' and os.environ['FIXTURE_TEST'] == '//:gate-distribution-notices':
    print('root//:toolchain-packages')
''').chmod(0o755)
        self.write('.gitignore', '.buck2/\n')
        self.write('core/programs/toolchain-packages.toml', 'packages = ["runner"]\n')
        self.manifest = 'core/programs/runner/hirsel.toml'
        self.write(self.manifest, '[dependencies]\n')
        self.git('init', '-q')
        self.git('add', '.')
        self.git('-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '-qm', 'base')
        self.git('update-ref', 'refs/remotes/origin/main', 'HEAD')

    def git(self, *arguments):
        subprocess.run(['git', *arguments], cwd=self.root, check=True)

    def write(self, name, content):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content)
        return path

    def affected(self, target=''):
        log = self.root / '.buck2/commands'
        log.unlink(missing_ok=True)
        subprocess.run([self.root / 'tools/affected'], cwd=self.root, check=True,
                       env=os.environ | {'FIXTURE_TEST': target}, capture_output=True)
        return [json.loads(line) for line in log.read_text().splitlines()]

    def test_packer_manifests_and_install_load_inputs_build_packages(self):
        for path in ('tools/toolchain-dist/toolchain_programs.py', self.manifest,
                     'crates/hirsel-core-driver/src/installation.rs', 'crates/hirsel-core-driver/src/package_store.rs'):
            with self.subTest(path=path):
                source = self.write(path, 'changed\n')
                self.assertEqual(self.affected(), [['build', '//:toolchain-packages', '--materializations', 'none']])
                if path == self.manifest:
                    self.git('checkout', 'HEAD', '--', path)
                else:
                    source.unlink()
        (self.root / self.manifest).unlink()
        self.assertEqual(self.affected(), [['build', '//:toolchain-packages', '--materializations', 'none']])

    def test_reuse_package_action_only_when_the_selected_test_depends_on_it(self):
        self.write(self.manifest, 'changed\n')
        notices = '//:gate-distribution-notices'
        self.assertEqual(self.affected(notices), [
            ['uquery', 'set(//:toolchain-packages) intersect deps(set(//:gate-distribution-notices))'], ['test', notices],
        ])
        fixture = '//:test-toolchain-packages'
        self.assertEqual(self.affected(fixture), [
            ['uquery', 'set(//:toolchain-packages) intersect deps(set(//:test-toolchain-packages))'],
            ['build', '//:toolchain-packages', '--materializations', 'none'], ['test', fixture],
        ])


if __name__ == '__main__':
    unittest.main()
