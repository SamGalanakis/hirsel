#!/usr/bin/env python3
"""Declaration growth is a LANG-12 refusal; function growth still reaches a named resource refusal."""
import argparse
import json
import os
from pathlib import Path
import re
import tempfile

from case_progress import run_case

ROOT = Path(__file__).resolve().parents[2]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compiler', type=Path, required=True)
    parser.add_argument('--runtime', type=Path, default=os.environ.get('HIRSEL_CORE_RT_ARCHIVE'))
    parser.add_argument('--only', choices=['forest', 'function'])
    # The combined core gate asks every harness for serial bodies; accept exactly that.
    parser.add_argument('--test-threads', type=int, choices=[1], default=1)
    args = parser.parse_args()
    core = dict(name='core', version='1')
    core_package = dict(package=core, dependencies=[], sources=[
        dict(file=str(p), modules=[p.stem], signatures=True) for p in sorted((ROOT / 'core/core').glob('*.hirsel'))])
    with tempfile.TemporaryDirectory(prefix='hirsel-recursion-') as tmp:
        directory = Path(tmp)
        def run(name, source, build=False):
            path = directory / (name + '.hirsel')
            path.write_text(source)
            package = dict(name='growth', version='1')
            packages = [core_package, dict(package=package, dependencies=[core], sources=[
                dict(file=str(path), modules=['types'], signatures=False)])]
            if not build:
                # The instantiating use is in another package. The diagnostic must
                # name the definition's payload, not this use or a resource limit.
                use = directory / 'use.hirsel'
                use.write_text('type Forest { nests: List[growth.types.Nest[u64]] }')
                packages.append(dict(package=dict(name='app', version='1'), dependencies=[core, package],
                    sources=[dict(file=str(use), modules=['main'], signatures=False)]))
            inputs = directory / (name + '.json')
            inputs.write_text(json.dumps(dict(core=core, packages=packages)))
            command = [str(ROOT / 'tools/capped'), str(args.compiler), 'build' if build else 'check',
                       '--inputs', str(inputs), '--store', str(directory / (name + '-store')), '--json']
            if build:
                command += ['--entry', 'growth@1/types/main', '--backend', 'cranelift',
                            '--runtime', str(args.runtime), '-o', str(directory / 'unused-program')]
            observed = run_case(name, 'cranelift' if build else 'checker', command, capture_output=True, env=os.environ | {
                'HIRSEL_MEMORY_LIMIT': '256M', 'HIRSEL_JOBS': '1'}, cwd=ROOT)
            stdout, stderr = observed.stdout.decode(), observed.stderr.decode()
            print(name, 'exit', observed.returncode, stderr.strip(), flush=True)
            peak = re.search(r'capped: peak (\d+) MiB of 256M', stderr)
            if peak is None or int(peak[1]) > 256:
                raise AssertionError(f'{name}: no completed, bounded peak observation: {stderr}')
            return observed.returncode, stdout, stderr
        if args.only != 'function':
            status, stdout, stderr = run('forest', 'pub type Nest[T] = Leaf(T) | Deeper(List[Nest[(T, T)]])')
            if status != 1:
                raise AssertionError(f'Forest must be refused at its declaration, got {status}: {stdout} {stderr}')
            product = json.loads(stdout)
            diagnoses = product['diagnostics']
            if len(diagnoses) != 1 or diagnoses[0]['rule'] != 'LANG-12' or product['incomplete'] or product['resources']:
                raise AssertionError(product)
            refusal = diagnoses[0]
            if refusal['primary']['span']['start'] != len('pub type Nest[T] = Leaf(T) | Deeper('):
                raise AssertionError(refusal)
            if refusal['primary']['module'] != 'forest.hirsel' or 'Nest' not in refusal['message'] or ' -> ' not in refusal['message']:
                raise AssertionError(refusal)
            print('pass Forest: one declaration refusal naming its growing cycle', flush=True)
        if args.only != 'forest':
            status, stdout, stderr = run('function', '''fn f[T](take x: T) { let y = (take x, ()); f(take y) }
pub fn main() -> Result[(), Exit] { f(1u64); Ok(()) }
''', build=True)
            if status != 71 or 'memory limit 256M reached' not in stderr or 'while instantiating function f' not in stderr:
                raise AssertionError(f'function must stop with a named resource refusal, not a rule, kill or hang: {status} {stdout} {stderr}')
            print('pass growing function: named resource refusal at unchanged 256M limit', flush=True)


if __name__ == '__main__':
    main()
