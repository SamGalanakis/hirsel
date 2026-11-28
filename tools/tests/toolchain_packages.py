#!/usr/bin/env python3
"""Pack a toolchain member whose workspace dependency carries authority."""

import os
from pathlib import Path
import subprocess
import sys
import tempfile
import json
import unittest

class ToolchainPackages(unittest.TestCase):
    def test_pack_member_with_authority(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            sources = root / 'sources'
            output = root / 'archives'
            work = root / 'work'
            store = sources / 'store'
            runner = sources / 'runner'
            store.mkdir(parents=True)
            runner.mkdir()
            for package in (store, runner):
                deps = ['("std", Toolchain(package: "std", major: 1, authority: []))']
                if package == runner:
                    deps.append('("store", Path(package: "store", major: 1, path: "../store", authority: [Dir, Clock]))')
                (package / 'hirsel.hcfg').write_text(
                    f'pkg_v1.Package(name: "{package.name}", version: "1.0.0", charge: ChargeV1, '
                    f'sources: [("{package.name}.hirsel", "{package.name}")], dependencies: [{", ".join(deps)}])\n',
                    encoding='utf-8')
            (store / 'store.hirsel').write_text('pub fn pack_fixture_value() -> u64 { 7u64 }\n')
            (runner / 'runner.hirsel').write_text('pub fn load() -> u64 { store.pack_fixture_value() }\n')

            compiler = Path(os.environ['HIRSEL_PACK_COMPILER']).resolve()
            programs = Path(os.environ['HIRSEL_PACK_PROGRAMS']).resolve()
            subprocess.run([sys.executable, programs, 'pack', compiler, sources, output, work,
                            'store', 'runner'], cwd=root, check=True)

            staged = subprocess.check_output([compiler, 'config', 'export', work / 'runner/hirsel.hcfg', '--json'])
            bindings = dict(json.loads(staged)['value']['dependencies'])
            self.assertEqual(bindings, {name: {'case': 'Toolchain', 'fields': {
                'package': name, 'major': 1, 'authority': []}} for name in ('std', 'store')})
            self.assertEqual({path.name for path in output.iterdir()}, {'store-1.hpkg', 'runner-1.hpkg'})


if __name__ == '__main__':
    unittest.main()
