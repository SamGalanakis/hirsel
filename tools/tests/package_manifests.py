#!/usr/bin/env python3
"""Check every package and example manifest against the current compiler and std.

    package_manifests.py --hirsel <compiler> [--shard <i>/<n> --costs <table>]

`--shard i/n` checks the i-th of n parts of the manifests, dealt longest first
by the measured seconds in the `--costs` table (`manifest<TAB>seconds`); a
manifest without a row costs the mean of the measured ones, so every shard
computes the same assignment and every manifest is checked by exactly one.
Each check prints `manifest-cost<TAB><manifest><TAB><seconds>`; refresh the
table from a run of every shard with

    .buck2/bin/buck2 test //:test-package-manifests-shard-1 ... 2>&1 | grep -o 'manifest-cost.*' |
      cut -f2,3 | sort > tools/tests/package_manifests.tsv
"""
import concurrent.futures
import json
import os
from pathlib import Path
import subprocess
import sys
import time
import unittest

ROOT = Path(__file__).resolve().parents[2]


def option(name):
    if name not in sys.argv:
        return None
    index = sys.argv.index(name)
    value = sys.argv[index + 1]
    del sys.argv[index:index + 2]
    return value


COMPILER = Path(option('--hirsel')).resolve()
SHARD = option('--shard')
COSTS = option('--costs')
# These packages are the negative inputs of the named conformance cases.
REJECTIONS = {
    'packages/fixtures/opaque-truth/bad/hirsel.json': 'CAP-2',
    'packages/fixtures/std-collision-bad/hirsel.json': 'LANG-4',
}


def deal(manifests, shard, costs):
    """The manifests of the 1-based shard `i/n`: longest first onto the least-loaded shard."""
    index, count = (int(part) for part in shard.split('/'))
    if not 1 <= index <= count:
        raise ValueError(f'--shard {shard}: the index must be in 1..{count}')
    mean = sum(costs.values()) / len(costs) if costs else 1.0
    loads, mine = [0.0] * count, []
    for name in sorted(manifests, key=lambda name: (-costs.get(name, mean), name)):
        best = loads.index(min(loads))
        loads[best] += costs.get(name, mean)
        if best == index - 1:
            mine.append(name)
    return sorted(mine)


def read_costs(path):
    rows = {}
    for line in Path(path).read_text().splitlines():
        name, seconds = line.split('\t')
        rows[name] = float(seconds)
    return rows


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
        if SHARD:
            names = deal([str(path.relative_to(ROOT)) for path in manifests], SHARD,
                         read_costs(ROOT / COSTS) if COSTS else {})
            manifests = [ROOT / name for name in names]

        def check(path):
            start = time.monotonic()
            result = subprocess.run(
                [str(COMPILER), 'check', '--json', str(path)],
                cwd=ROOT, capture_output=True, text=True,
                env=dict(os.environ, HIRSEL_DAEMON='off', HIRSEL_JOBS='1',
                         HIRSEL_RESULTS='none@1', HIRSEL_MEMORY_LIMIT='2G'))
            print(f'manifest-cost\t{path.relative_to(ROOT)}\t{time.monotonic() - start:.1f}', flush=True)
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
        print(f'Checked {len(results)} package and example manifests' + (f' (shard {SHARD})' if SHARD else ''), flush=True)


if __name__ == '__main__':
    unittest.main(verbosity=2)
