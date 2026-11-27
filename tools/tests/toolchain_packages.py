#!/usr/bin/env python3
"""Pack a toolchain member whose workspace dependency carries authority."""

import os
from pathlib import Path
import subprocess
import sys
import tempfile
import tomllib
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
                (package / 'hirsel.toml').write_text(f'''[package]
name = "{package.name}"
version = "1.0.0"
charge = "charge-v1"

[sources]
"{package.name}.hirsel" = "{package.name}"

[dependencies]
std = {{ major = 1 }}
''' + ('' if package == store else '''
[dependencies."store"]
authority = ["Dir", "Clock"]
path = "../store"
major = 1
package = "store"

[tool]
store = { package = "store", major = 1, path = "../store" }
'''), encoding='utf-8')
            (store / 'store.hirsel').write_text('pub fn pack_fixture_value() -> u64 { 7u64 }\n')
            (runner / 'runner.hirsel').write_text('pub fn load() -> u64 { store.pack_fixture_value() }\n')

            compiler = Path(os.environ['HIRSEL_PACK_COMPILER']).resolve()
            programs = Path(os.environ['HIRSEL_PACK_PROGRAMS']).resolve()
            subprocess.run([sys.executable, programs, 'pack', compiler, sources, output, work,
                            'store', 'runner'], cwd=root, check=True)

            manifest = tomllib.loads((work / 'runner/hirsel.toml').read_text(encoding='utf-8'))
            self.assertEqual(manifest['dependencies'], {'std': {'major': 1}, 'store': {'major': 1}})
            self.assertEqual(manifest['tool']['store'], {'package': 'store', 'major': 1, 'path': '../store'})
            self.assertEqual({path.name for path in output.iterdir()}, {'store-1.hpkg', 'runner-1.hpkg'})


if __name__ == '__main__':
    unittest.main()
