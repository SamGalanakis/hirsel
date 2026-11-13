#!/usr/bin/env python3
"""Run std's source contract oracles through the published native product path.

Example: std_native.py --compiler PATH --runtime PATH --filter file --backend both
Confined filesystem cases run on the host, which needs openat2 (Linux >= 5.6).
"""
import argparse
import dataclasses
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[2]
LANES = {'895': 'core-check-895', '910': 'core-check-calls-910', '912': 'core-check-index-912',
         '913': 'core-check-callables-913', '914': 'core-ops-914'}


@dataclasses.dataclass(frozen=True)
class Contract:
    file: Path
    line: int
    call: str
    expected: str
    waits: bool

    @property
    def name(self):
        return self.file.stem + ':' + self.call.split('(')[0]


def discover(root, filters):
    contracts = []
    for file in sorted((root / 'core/std/tests').glob('*.hirsel')):
        source = file.read_text()
        for line, text in enumerate(source.splitlines(), 1):
            match = re.fullmatch(r'\s*// expect: run (.*?) = (.*?)\s*', text)
            if not match:
                if text.strip().startswith('// expect: run '):
                    raise ValueError(f'{file}:{line}: unreadable run oracle')
                continue
            call, expected = match.groups()
            name = call.split('(')[0]
            header = re.search(r'\bfn\s+' + re.escape(name) + r'\s*\([^)]*\)([^\{]*)\{', source)
            if header is None:
                raise ValueError(f'{file}:{line}: no function for {call}')
            case = Contract(file, line, call, expected, bool(re.search(r'\bwaits\b', header[1])))
            if not filters or any(case.name.startswith(f) or str(file.relative_to(root)).startswith(f) for f in filters):
                contracts.append(case)
    if not contracts:
        raise ValueError(f'no expect: run tests match {filters!r}')
    return contracts


def wrapper(case):
    # The adapter lives in the original module, so private helpers remain private.
    call = case.call.replace('(dir)', '(native_dir)')
    parameters = 'native_dir:Dir' if '(dir)' in case.call else ''
    effect = ' waits' if case.waits else ''
    if case.expected.startswith('fail '):
        body = f'let _ = {call}\nOk(())'
    else:
        body = f'if {call} == {case.expected} {{Ok(())}} else {{Error(Exit(status:1))}}'
    return f'\npub fn native_contract({parameters})->Result[(),Exit]{effect} {{\n{body}\n}}\n'


def manifest(root, case, adapted):
    core = {'name': 'core', 'version': '1'}
    std = {'name': 'std', 'version': '1'}
    def sources(directory, signatures):
        return [{'file': str(p.resolve()), 'modules': [p.stem], 'signatures': signatures}
                for p in sorted(directory.glob('*.hirsel'))]
    files = sources(root / 'core/std', False)
    files.append({'file': str(adapted), 'modules': ['tests', case.file.stem], 'signatures': False})
    return {'core': core, 'packages': [
        {'package': core, 'sources': sources(root / 'core/core', True), 'dependencies': []},
        {'package': std, 'sources': files, 'dependencies': [core]}]}


def owner(message, owners):
    # Rows are copied from the public NOT-BUILT census, not inferred from test success.
    for row in owners:
        if row['message'] in message:
            return f"{row['row']} {LANES[row['owner']]}"
    if any(word in message for word in ['LLVM', 'llvm', 'verify', 'emit release']):
        return 'core-llvm-905'
    if 'cranelift' in message.lower():
        return 'core-cranelift-904'
    return 'unassigned (route first cause)'


def first_stop(product):
    reached = (product.get('reachable') or {}).get('first_stops', [])
    stops = reached or product.get('diagnostics', []) or product.get('incomplete', [])
    if stops:
        stop = stops[0]
        site = stop['primary']
        span = site['span']
        return f"{stop.get('rule', 'GAP')} {site['module']}:{span['start']}..{span['end']}: {stop.get('construct', stop.get('message'))}"
    return product.get('stopped') or 'native product unavailable without a diagnostic'


def wait_load():
    printed = False
    while os.getloadavg()[0] > 32:
        if not printed:
            print('waiting: host load exceeds 32; no build started', flush=True)
            printed = True
        time.sleep(10)


def observation(case, result):
    if case.expected.startswith('fail '):
        kind = case.expected.removeprefix('fail ').encode()
        good = result.returncode == 101 and not result.stdout and re.match(re.escape(kind) + rb'(?:\s|$|:|\()', result.stderr)
    else:
        good = result.returncode == 0 and not result.stdout and not result.stderr
    if good:
        return None
    return f'exit {result.returncode}, stdout {result.stdout!r}, stderr {result.stderr!r}; expected {case.expected}'


def run(root, compiler, runtime, contracts, engines, output, owners, cc=None, timeout=30):
    rows = []
    # Private stores are shared across cases only for immutable checked/native products.
    # Each case's host state is fresh and its executable is removed after observation.
    with tempfile.TemporaryDirectory(prefix='hirsel-std-native-') as temporary:
        work = Path(temporary)
        for engine in engines:
            for index, case in enumerate(contracts):
                scratch = work / f'{engine}-{index}'
                scratch.mkdir()
                host = scratch / 'host'
                host.mkdir()  # Script: every contract receives one fresh, empty confined Dir.
                adapted = scratch / case.file.name
                adapted.write_text(case.file.read_text() + wrapper(case))
                inputs = scratch / 'inputs.json'
                inputs.write_text(json.dumps(manifest(root, case, adapted)))
                binary = scratch / 'program'
                command = [str(compiler), 'build', '--inputs', str(inputs), '--entry',
                           f'std@1/tests/{case.file.stem}/native_contract', '--backend', engine,
                           '--runtime', str(runtime), '--store', str(work / 'store'), '--json',
                           '--dir', str(host), '-o', str(binary)]
                if engine == 'llvm':
                    command.append('--release')
                if cc:
                    command.extend(['--cc', str(cc)])
                wait_load()
                build = subprocess.run(command, capture_output=True)
                row = dict(test=case.name, file=str(case.file.relative_to(root)), line=case.line,
                           engine=engine, profile='release' if engine == 'llvm' else 'dev',
                           call=case.call, expected=case.expected, ran=False, passed=False,
                           cause=None, owner=None)
                try:
                    product = json.loads(build.stdout)
                except ValueError:
                    product = {'stopped': f'build exited {build.returncode}: {build.stdout.decode(errors="replace")} {build.stderr.decode(errors="replace")}'}
                if build.returncode or not product.get('native'):
                    row['cause'] = first_stop(product)
                else:
                    try:
                        wait_load()
                        result = subprocess.run([str(binary)], cwd=host, capture_output=True, timeout=timeout)
                        row['ran'] = True
                        row['cause'] = observation(case, result)
                        row['passed'] = row['cause'] is None
                    except subprocess.TimeoutExpired:
                        row['ran'] = True
                        row['cause'] = f'native execution timed out after {timeout}s'
                if row['cause']:
                    row['owner'] = owner(row['cause'], owners)
                rows.append(row)
                status = 'PASS' if row['passed'] else 'FAIL' if row['ran'] else 'NOT RUN'
                print(f"{engine} {case.name}: {status}" + (f" {row['cause']} [owner {row['owner']}]" if row['cause'] else ''), flush=True)
                if output:
                    output.write_text(json.dumps(rows, indent=2) + '\n')
                if binary.exists():
                    binary.unlink()
    for engine in engines:
        selected = [r for r in rows if r['engine'] == engine]
        print(f"{engine}: {len(selected)} selected, {sum(r['ran'] for r in selected)} ran, {sum(r['passed'] for r in selected)} passed, {sum(not r['ran'] for r in selected)} not run", flush=True)
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compiler', type=Path, required=True)
    parser.add_argument('--runtime', type=Path, required=True)
    parser.add_argument('--root', type=Path, default=ROOT)
    parser.add_argument('--backend', choices=['both', 'cranelift', 'llvm'], default='both')
    parser.add_argument('--filter', action='append', default=[])
    parser.add_argument('--report', type=Path)
    parser.add_argument('--cc', type=Path)
    parser.add_argument('--timeout', type=int, default=30, help='native execution bound; never bounds a build')
    args = parser.parse_args()
    root = args.root.resolve()
    contracts = discover(root, args.filter)
    owners = json.loads(Path(__file__).with_name('std_native_owners.json').read_text())
    engines = ['cranelift', 'llvm'] if args.backend == 'both' else [args.backend]
    rows = run(root, args.compiler.resolve(), args.runtime.resolve(), contracts, engines,
               args.report, owners, args.cc.resolve() if args.cc else None, args.timeout)
    return int(any(not r['passed'] for r in rows))


if __name__ == '__main__':
    try:
        raise SystemExit(main())
    except (ValueError, OSError) as error:
        print(error, flush=True)
        raise SystemExit(2)
