#!/usr/bin/env python3
"""Run the contract laws supported by the selected native engines.

Laws marked `// test-backend: evaluator` run in the evaluator suite.
Dir laws need the host's openat2, so these tests run on the invoking host.
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

from std_native import ROOT, discover, run


def ffi_laws(args, root, backend):
    grant = 'Foreign=ffi_laws@1/laws/Laws'
    with tempfile.TemporaryDirectory(prefix='hirsel-ffi-laws-') as store:
        fixture = Path(store) / 'core/programs/ffi_laws'
        shutil.copytree(root / 'core/programs/ffi_laws', fixture)
        native = [
            '--backend', backend,
            '--runtime', str(args.runtime.resolve()),
            '--cc', str(args.cc.resolve()),
            '--libraries', str(root / 'core'), '--source-core', '--source-std',
            '--store', str(Path(store) / 'build'), '--grant', grant,
        ]
        compiler = str(args.compiler.resolve())
        command = [compiler, 'test', '.', *native, '-o', str(Path(store) / 'tests'),
                   '--test-timeout', '20', '--json']
        env = os.environ | {'XDG_CACHE_HOME': str(Path(store) / 'cache')}
        def request(extra):
            output = subprocess.run(command + extra, cwd=fixture, env=env,
                                    capture_output=True, text=True, check=False)
            if output.returncode:
                raise AssertionError(output.stdout + output.stderr)
            return json.loads(output.stdout)['tests']

        cold = request([])
        warm = request(['--filter', 'fl6_'])
        for rows, control in ((cold, 'executed'), (warm, 'cached')):
            sources = {row['definition'].rsplit('/', 1)[-1]: row['source'] for row in rows}
            if sources.get('fl6_parameterless_control') != control:
                raise AssertionError(sources)
            if any(sources.get(name) != 'executed' for name in
                   ('fl6_reaches_without_calling', 'fl6_reaches_in_finalizer')):
                raise AssertionError(sources)
        queue = next(row for row in cold if row['definition'].endswith('/fl18_full_helper_pool'))
        if queue['stderr'].splitlines() != ['freed 2', 'freed 1']:
            raise AssertionError(queue)
        output = subprocess.run(
            [compiler, 'run', '.', *native, '-o', str(Path(store) / 'release')],
            cwd=fixture, env=env, capture_output=True, text=True, check=False,
        )
        if output.returncode or output.stderr.splitlines()[-3:] != ['freed 3', 'freed 2', 'freed 1']:
            raise AssertionError(output.stdout + output.stderr)
        return len(cold) + len(warm) + 1


def sqlite_laws(args, root, backend):
    with tempfile.TemporaryDirectory(prefix='hirsel-sqlite-laws-') as store:
        fixture = Path(store) / 'core/programs/sqlite'
        shutil.copytree(root / 'core/programs/sqlite', fixture)
        command = [
            str(args.compiler.resolve()), 'test', '.', '--backend', backend,
            '--runtime', str(args.runtime.resolve()), '--cc', str(args.cc.resolve()),
            '--libraries', str(root / 'core'), '--source-core', '--source-std',
            '--store', str(Path(store) / 'build'), '-o', str(Path(store) / 'tests'),
            '--grant', 'Foreign=sqlite@1/sqlite/Sqlite', '--test-timeout', '20', '--json',
        ]
        env = os.environ | {'XDG_CACHE_HOME': str(Path(store) / 'cache')}

        def request(extra):
            output = subprocess.run(command + extra, cwd=fixture, env=env,
                                    capture_output=True, text=True, check=False)
            if output.returncode:
                raise AssertionError(output.stdout + output.stderr)
            return json.loads(output.stdout)['tests']

        first = request([])
        second = request(['--filter', 'fl10_random'])
        initial = next(row for row in first if row['definition'].endswith('/fl10_random'))
        if len(second) != 1 or not initial['stdout'] or initial['stdout'] != second[0]['stdout']:
            raise AssertionError((initial, second))
        return len(first) + len(second)


def stun_ice_laws(args, root, backend):
    # NET-18's address-class law (S1); the package's other laws are not wired here.
    with tempfile.TemporaryDirectory(prefix='hirsel-stun-ice-laws-') as store:
        fixture = Path(store) / 'core/programs/stun_ice'
        shutil.copytree(root / 'core/programs/stun_ice', fixture)
        command = [
            str(args.compiler.resolve()), 'test', '.', '--backend', backend,
            '--runtime', str(args.runtime.resolve()), '--cc', str(args.cc.resolve()),
            '--libraries', str(root / 'core'), '--source-core', '--source-std',
            '--store', str(Path(store) / 'build'), '-o', str(Path(store) / 'tests'),
            '--filter', 'public_class_table', '--test-timeout', '20', '--json',
        ]
        env = os.environ | {'XDG_CACHE_HOME': str(Path(store) / 'cache')}
        output = subprocess.run(command, cwd=fixture, env=env, capture_output=True, text=True, check=False)
        if output.returncode:
            raise AssertionError(output.stdout + output.stderr)
        rows = json.loads(output.stdout)['tests']
        if [row['definition'].rsplit('/', 1)[-1] for row in rows] != ['public_class_table']:
            raise AssertionError(rows)
        return len(rows)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compiler', type=Path, required=True)
    parser.add_argument('--evaluator', type=Path, required=True)
    parser.add_argument('--node', type=Path, required=True)
    parser.add_argument('--schedule-runtime', type=Path)
    parser.add_argument('--runtime', type=Path, default=os.environ.get('HIRSEL_CORE_RT_ARCHIVE'))
    parser.add_argument('--record-runtime', type=Path, default=os.environ.get('HIRSEL_CORE_RT_RECORD_ARCHIVE'))
    parser.add_argument('--cc', type=Path, default=(Path(os.environ['LLVM_SYS_231_PREFIX']) / 'bin/clang')
                        if 'LLVM_SYS_231_PREFIX' in os.environ else None)
    parser.add_argument('--backend', action='append', choices=['cranelift', 'llvm'])
    parser.add_argument('--root', type=Path, default=ROOT)
    parser.add_argument('--filter', action='append', default=[])
    parser.add_argument('--test-threads', type=int, choices=[1], default=1)
    args = parser.parse_args()
    engines = args.backend or ['cranelift', 'llvm']

    class Laws(unittest.TestCase):
        def test_every_native_runnable_law(self):
            root = args.root.resolve()
            contracts = [c for c in discover(root, args.filter) if not c.evaluator_only]
            self.assertTrue(contracts, 'zero native-runnable laws selected')
            owners = json.loads(Path(__file__).with_name('std_native_owners.json').read_text())
            rows = run(root, args.compiler.resolve(), args.runtime.resolve(), contracts, engines, None, owners,
                       args.cc.resolve() if args.cc else None,
                       evaluator=args.evaluator.resolve(), node=args.node.resolve(),
                       schedule_runtime=args.schedule_runtime.resolve() if args.schedule_runtime else None,
                       record_runtime=args.record_runtime.resolve() if args.record_runtime else None)
            ffi = sum(ffi_laws(args, root, engine) + sqlite_laws(args, root, engine)
                      + stun_ice_laws(args, root, engine) for engine in engines)
            findings = [f"{row['engine']} {row['file']}:{row['line']} {row['test']}: {row['cause']}"
                        for row in rows if row['applicable'] and not row['passed']]
            # tools/sweep counts this line: one body per law and engine.
            print(f"native laws: {ffi + sum(row['applicable'] and row['passed'] for row in rows)} passed, "
                  f"{len(findings)} failed", flush=True)
            self.assertFalse(findings, '\n' + '\n'.join(findings))
            self.assertTrue(any(row['applicable'] for row in rows), 'zero laws ran')

    result = unittest.TextTestRunner(verbosity=2).run(unittest.defaultTestLoader.loadTestsFromTestCase(Laws))
    return int(not result.wasSuccessful())


if __name__ == '__main__':
    raise SystemExit(main())
