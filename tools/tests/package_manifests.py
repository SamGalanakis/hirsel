#!/usr/bin/env python3
"""Check every package and example manifest against the current compiler and std."""
import concurrent.futures
import json
import os
from pathlib import Path
import subprocess
import sys
import unittest

ROOT = Path(__file__).resolve().parents[2]
index = sys.argv.index('--hirsel')
COMPILER = Path(sys.argv[index + 1]).resolve()
del sys.argv[index:index + 2]
# These packages are the negative inputs of the named conformance cases.
REJECTIONS = {
    'packages/fixtures/opaque-truth/bad/hirsel.json': 'CAP-2',
    'packages/fixtures/std-collision-bad/hirsel.json': 'LANG-4',
}


class PackageManifests(unittest.TestCase):
    def test_packages_and_examples_check(self):
        # GUIDE.md requires this ignored source before checking the JSON kit.
        hooks = ROOT / 'examples/typed-json-kit/hooks.hir'
        if not hooks.exists():
            generated = subprocess.run(
                [sys.executable, str(ROOT / 'tools/json-hooks'),
                 'Item', 'Snapshot', '--root', 'Snapshot'],
                check=True, capture_output=True)
            hooks.write_bytes(generated.stdout)
            self.addCleanup(hooks.unlink)
        manifests = []
        for directory in ('packages', 'examples'):
            for path in sorted((ROOT / directory).rglob('*.json')):
                if path.name == 'hirsel.json' or path.name.endswith('.hir.json'):
                    manifests.append(path)
                    continue
                try:
                    value = json.loads(path.read_text())
                except (ValueError, UnicodeError):
                    continue
                if isinstance(value, dict) and 'package' in value and 'sources' in value:
                    manifests.append(path)
            self.assertTrue(any(path.is_relative_to(ROOT / directory) for path in manifests), directory)

        def check(path):
            result = subprocess.run(
                [str(COMPILER), 'check', '--json', str(path)],
                cwd=ROOT, capture_output=True, text=True,
                env=dict(os.environ, HIRSEL_DAEMON='off', HIRSEL_JOBS='1',
                         HIRSEL_RESULTS='none@1', HIRSEL_MEMORY_LIMIT='2G'))
            return path.relative_to(ROOT), result

        with concurrent.futures.ThreadPoolExecutor(
                max_workers=int(os.environ.get('HIRSEL_TEST_CPUS', '2'))) as pool:
            results = list(pool.map(check, manifests))
        for path, result in results:
            with self.subTest(manifest=str(path)):
                expected = REJECTIONS.get(str(path))
                self.assertEqual(result.returncode, 1 if expected else 0,
                                 f'{path}\n{result.stdout}\n{result.stderr}')
                if expected:
                    diagnostics = [json.loads(line) for line in result.stdout.splitlines()]
                    errors = [item['rule'] for item in diagnostics
                              if item.get('severity') == 'error']
                    self.assertIn(expected, errors, f'{path}\n{result.stdout}')
        print(f'Checked {len(results)} package and example manifests', flush=True)


if __name__ == '__main__':
    unittest.main(verbosity=2)
