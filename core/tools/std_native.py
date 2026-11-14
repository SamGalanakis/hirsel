#!/usr/bin/env python3
"""Run core and std source contract oracles on native engines and the evaluator.

Example: std_native.py --compiler PATH --runtime PATH --filter file --backend both
Confined filesystem cases run on the host, which needs openat2 (Linux >= 5.6).
"""
import argparse
import dataclasses
import json
import os
from pathlib import Path
import re
import shlex
import shutil
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
    stdout: bytes = b""
    stderr: bytes = b""
    first_write: int | None = None
    test_of: str | None = None
    host_op: int = 0
    package: str = "std"
    evaluator_only: str | None = None

    @property
    def name(self):
        prefix = 'core.' if self.package == 'core' else ''
        return prefix + self.file.stem + ':' + self.call.split('(')[0]


def masked_source(source):
    return re.sub(r'//[^\n]*|/\*.*?\*/|"(?:\\.|[^"\\])*"|\'(?:\\.|[^\'\\])*\'',
                    lambda m: ' ' * len(m[0]), source, flags=re.S)


def declared_names(source):
    # Inspect only named module-level declarations, not callable parameter types.
    masked = masked_source(source)
    depth = 0
    names = []
    tokens = list(re.finditer(r'[A-Za-z_]\w*|[{}]', masked))
    for i, token in enumerate(tokens):
        text = token[0]
        if text == '{':
            depth += 1
        elif text == '}':
            depth -= 1
        elif depth == 0 and text in ('fn', 'type', 'const') and i + 1 < len(tokens) and re.match(r'\s+[A-Za-z_]', masked[token.end():]):
            names.append((text, tokens[i + 1][0]))
    return names


def test_module(root, file, source):
    first = source.splitlines()[0] if source.splitlines() else ''
    if not first.startswith('// test-of:'):
        return None
    match = re.fullmatch(r'// test-of: (core|std)\.([a-z][a-z0-9_]*)', first)
    if not match:
        raise ValueError(f'{file}: invalid first-line test-of marker')
    package, module = match.groups()
    if file.parent != root / 'core' / package / 'tests':
        raise ValueError(f'{file}: test-of must name its own library package')
    library = root / 'core' / package / (module + '.hirsel')
    if not library.is_file():
        raise ValueError(f'{file}: test-of module does not exist')
    if not re.search(r'^// test-reason: \S.+', source, re.M):
        raise ValueError(f'{file}: marked test needs // test-reason: why public API cannot express it')
    names = declared_names(source)
    if any(kind != 'fn' for kind, _ in names) or re.search(r'^\s*pub\b', source, re.M):
        raise ValueError(f'{file}: marked test may add only private test/helper functions')
    masked = masked_source(source)
    functions = list(re.finditer(r'\bfn\s+[A-Za-z_]\w*', masked))
    if any('{' not in masked[f.end():functions[i+1].start() if i+1 < len(functions) else len(masked)]
           for i, f in enumerate(functions)):
        raise ValueError(f'{file}: marked helper functions must have bodies')
    original = {name for _, name in declared_names(library.read_text())}
    additions = [name for _, name in names]
    if len(additions) != len(set(additions)) or original.intersection(additions):
        raise ValueError(f'{file}: test-of declaration name collision')
    if 'native_contract' in original or 'native_contract' in additions:
        raise ValueError(f'{file}: reserved adapter name collision')
    return module


def discover(root, filters):
    contracts = []
    files = [p for package in ['core', 'std'] for p in (root / 'core' / package / 'tests').glob('*.hirsel')]
    for file in sorted(files):
        source = file.read_text()
        test_of = test_module(root, file, source)
        backend = re.findall(r'^// test-backend: (.+)$', source, re.M)
        if backend and backend != ['evaluator']:
            raise ValueError(f'{file}: only // test-backend: evaluator is supported')
        reason = re.search(r'^// test-reason: (\S.+)$', source, re.M)
        if backend and not reason:
            raise ValueError(f'{file}: evaluator-only law needs // test-reason:')
        evaluator_only = reason[1] if backend else None
        stdout = stderr = b""
        first_write = None
        host_op = 0
        for line, text in enumerate(source.splitlines(), 1):
            if text.startswith("// expect: stdout "):
                stdout = json.loads(text.removeprefix("// expect: stdout ")).encode()
            if text.startswith("// expect: stderr "):
                stderr = json.loads(text.removeprefix("// expect: stderr ")).encode()
            if text.startswith("// host: first_write "):
                words = text.removeprefix("// host: first_write ").split()
                first_write = int(words[0])
                host_op = 2 if words[1:] == ["file"] else 0
            if text == "// host: remove_tree_prefix a":
                first_write, host_op = 0, 9
            match = re.fullmatch(r'\s*// expect: run (.*?) = (.*?)\s*', text)
            if not match:
                if text.strip().startswith('// expect: run '):
                    raise ValueError(f'{file}:{line}: unreadable run oracle')
                continue
            call, expected = match.groups()
            if expected.startswith('fault ') and not evaluator_only:
                raise ValueError(f'{file}:{line}: fault oracle requires // test-backend: evaluator')
            if expected.startswith('fault '):
                if not isinstance(json.loads(expected.removeprefix('fault ')), str):
                    raise ValueError(f'{file}:{line}: fault oracle must be an exact JSON string')
            if evaluator_only and (not call.endswith('()') or first_write is not None):
                raise ValueError(f'{file}:{line}: evaluator-only laws take no host arguments')
            name = call.split('(')[0]
            header = re.search(r'\bfn\s+' + re.escape(name) + r'\s*\([^)]*\)([^\{]*)\{', source)
            if header is None:
                raise ValueError(f'{file}:{line}: no function for {call}')
            case = Contract(file, line, call, expected, bool(re.search(r'\bwaits\b', header[1])), stdout, stderr, first_write, test_of, host_op, file.relative_to(root).parts[1], evaluator_only)
            stdout = stderr = b""
            first_write = None
            host_op = 0
            if not filters or any(case.name.startswith(f) or ('std.' + case.name).startswith(f) and case.package == 'std' or str(file.relative_to(root)).startswith(f) for f in filters):
                contracts.append(case)
    if not contracts:
        raise ValueError(f'no expect: run tests match {filters!r}')
    return contracts


def host_parameter(case):
    for argument, kind in [('dir', 'Dir'), ('out', 'Stdout'), ('err', 'Stderr')]:
        if '(' + argument + ')' in case.call:
            name = 'native_' + argument
            return name + ':' + kind, name
    return '', ''


def wrapper(case, evaluator=False):
    # The adapter lives in the original module, so private helpers remain private.
    call = case.call.replace('(dir)', '(native_dir)').replace('(out)', '(native_out)').replace('(err)', '(native_err)')
    parameters, _ = host_parameter(case)
    effect = ' waits' if case.waits else ''
    success = '1u64' if evaluator else 'Ok(())'
    failure = '0u64' if evaluator else 'Error(())'
    result = 'u64' if evaluator else 'Result[(),()]'
    if case.expected.startswith(('fail ', 'fault ')):
        body = f'let _ = {call}\n{success}'
    else:
        body = f'if {call} == {case.expected} {{{success}}} else {{{failure}}}'
    marker = '' if evaluator else '@test\n'
    return f'\n{marker}pub fn native_contract({parameters})->{result}{effect} {{\n{body}\n}}\n'


def manifest(root, case, adapted):
    core = {'name': 'core', 'version': '1'}
    std = {'name': 'std', 'version': '1'}
    def sources(directory, signatures):
        return [{'file': str(p.resolve()), 'modules': [p.stem], 'signatures': signatures}
                for p in sorted(directory.glob('*.hirsel'))]
    packages = [
        {'package': core, 'sources': sources(root / 'core/core', True), 'dependencies': []},
        {'package': std, 'sources': sources(root / 'core/std', False), 'dependencies': [core]}]
    files = next(p['sources'] for p in packages if p['package']['name'] == case.package)
    if case.test_of:
        original = next(f for f in files if f['modules'] == [case.test_of])
        original.update(file=str(adapted), signatures=case.package == 'core')
    else:
        files.append({'file': str(adapted), 'modules': ['tests', case.file.stem], 'signatures': False})
    return {'core': core, 'packages': packages}


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
        good = result.returncode == 101 and result.stdout == case.stdout and re.match(re.escape(kind) + rb'(?:\s|$|:|\()', result.stderr)
    else:
        good = result.returncode == 0 and result.stdout == case.stdout and result.stderr == case.stderr
    if good:
        return None
    return f'exit {result.returncode}, stdout {result.stdout!r}, stderr {result.stderr!r}; expected {case.expected}'


def test_observation(case, result, entry):
    """Keep source failure/output oracles while executing PROG-4 test runs."""
    try:
        product = json.loads(result.stdout)
    except ValueError:
        return False, f'test command exited {result.returncode}: {result.stdout!r} {result.stderr!r}'
    tests = product.get('tests', [])
    if len(tests) != 1 or tests[0].get('definition') != entry:
        return False, f'test discovery must execute exactly {entry}: {product!r}'
    test = tests[0]
    if 'stopped' in test:
        return False, test['stopped']
    if test.get('timed_out'):
        return True, 'native test execution timed out'
    if result.returncode != int(not test['passed']):
        return False, f'test aggregate disagrees with its outcome: exit {result.returncode}, {test!r}'
    output = subprocess.CompletedProcess([], test['status'], test['stdout'].encode(), test['stderr'].encode())
    return True, observation(case, output)


def evaluator_observation(case, result):
    if result.returncode:
        return False, f'evaluator backend exited {result.returncode}: {result.stdout!r} {result.stderr!r}'
    try:
        report = json.loads(result.stdout)
    except ValueError:
        return False, f'evaluator backend did not return JSON: {result.stdout!r}'
    if report.get('error'):
        return False, report['error']
    outcome = report.get('outcome')
    if case.expected.startswith('fault '):
        expected = dict(kind='fault', message=json.loads(case.expected.removeprefix('fault ')))
    elif case.expected.startswith('fail '):
        expected = dict(kind='failed', causes=case.expected.removeprefix('fail ').split(','))
    else:
        expected = dict(kind='returned', values=[1])
    if outcome != expected:
        return True, f'evaluator got {outcome!r}; expected {expected!r}'
    if expected['kind'] != 'fault':
        if bytes(report.get('stdout', [])) != case.stdout or bytes(report.get('stderr', [])) != case.stderr:
            return True, 'evaluator output differs'
        if expected['kind'] == 'returned' and report.get('used') != 0:
            return True, f"evaluator returned with {report.get('used')} bytes admitted"
    return True, None


def save_row(rows, row, output):
    rows.append(row)
    status = 'NOT APPLICABLE' if not row['applicable'] else 'PASS' if row['passed'] else 'FAIL' if row['ran'] else 'NOT RUN'
    print(f"{row['engine']} {row['test']}: {status}" + (f" {row['cause']}" if row['cause'] else '') +
          (f" ({row['seconds']:.3f}s of {row['bound']}s)" if 'seconds' in row else '') +
          (f" [owner {row['owner']}]" if row['owner'] else ''), flush=True)
    if output:
        output.write_text(json.dumps(rows, indent=2) + '\n')


def run(root, compiler, runtime, contracts, engines, output, owners, cc=None, timeout=30, evaluator=None, target=None, emulator=None, sysroot=None):
    rows = []
    requested_engines = set(engines)
    engines = list(engines)
    if any(c.evaluator_only for c in contracts) and 'evaluator' not in engines:
        engines.insert(0, 'evaluator')
    if 'evaluator' in engines and evaluator is None:
        raise ValueError('evaluator laws require --evaluator <hirsel-core-contract-eval>')
    # Private stores are shared across cases only for immutable checked/native products.
    # Each case's host state is fresh and its executable is removed after observation.
    with tempfile.TemporaryDirectory(prefix='hirsel-std-native-') as temporary:
        work = Path(temporary)
        for engine in engines:
            started = time.monotonic()
            for index, case in enumerate(contracts):
                # Preserve the historical automatic evaluator-only runs for native
                # selections; ordinary laws run here only when explicitly selected.
                if engine == 'evaluator' and 'evaluator' not in requested_engines and not case.evaluator_only:
                    continue
                row = dict(test=case.name, file=str(case.file.relative_to(root)), line=case.line,
                           engine=engine, profile=None if engine == 'evaluator' else 'release' if engine == 'llvm' else 'dev',
                           call=case.call, expected=case.expected, ran=False, passed=False, applicable=True,
                           cause=None, owner=None, native_only=False)
                if engine == 'evaluator' and host_parameter(case)[0]:
                    row.update(applicable=False, passed=None, native_only=True,
                               cause='native-only: entry takes a host capability')
                    save_row(rows, row, output)
                    continue
                if engine != 'evaluator' and case.evaluator_only:
                    row.update(applicable=False, passed=None, cause=case.evaluator_only)
                    save_row(rows, row, output)
                    continue
                evaluating = engine == 'evaluator'
                scratch = work / f'{engine}-{index}'
                scratch.mkdir()
                host = scratch / 'host'
                host.mkdir()  # Script: every contract receives one fresh, empty confined Dir.
                adapted = scratch / case.file.name
                original = (root / 'core' / case.package / (case.test_of + '.hirsel')).read_text() + '\n' if case.test_of else ''
                adapted.write_text(original + case.file.read_text() + wrapper(case, evaluator=evaluating))
                inputs = scratch / 'inputs.json'
                inputs.write_text(json.dumps(manifest(root, case, adapted)))
                entry = f'{case.package}@1/{case.test_of or ("tests/" + case.file.stem)}/native_contract'
                command = [str(compiler), 'test', '--inputs', str(inputs), '--backend', engine,
                           '--runtime', str(runtime), '--store', str(work / 'store'), '--json', '--limit', str(64 * 1024 * 1024),
                           '--test-timeout', str(timeout), '--dir', str(host), '-o', str(scratch / 'programs')]
                if evaluating:
                    # No load admission here: an evaluator law starts no build, and
                    # the gate's pool reservation already admitted this process. The
                    # timed interval is the law's own evaluator process alone.
                    law_started = time.monotonic()
                    try:
                        result = subprocess.run([str(evaluator), str(inputs), entry],
                                                capture_output=True, timeout=timeout)
                        row['ran'], row['cause'] = evaluator_observation(case, result)
                    except subprocess.TimeoutExpired:
                        row['ran'] = True
                        row['cause'] = f'evaluator execution timed out after {timeout}s'
                    row['seconds'] = round(time.monotonic() - law_started, 3)
                    row['bound'] = timeout
                    row['passed'] = row['cause'] is None
                    if row['cause']:
                        row['owner'] = ('core-check-895' if not row['ran'] else
                                        'core-ops-914' if 'external functions do not run here' in row['cause'] else
                                        'core-form-883')
                    save_row(rows, row, output)
                    continue
                if target:
                    command.extend(['--target', target])
                if emulator:
                    command.extend(['--emulator', str(emulator), '--sysroot', str(sysroot)])
                if engine == 'llvm':
                    command.append('--release')
                if case.package == 'core':
                    # A temporary core @test belongs to this source request;
                    # published dependencies deliberately omit test bodies.
                    command.append('--source-core')
                if '(out)' in case.call:
                    command.extend(['--grant', 'Stdout'])
                if '(err)' in case.call:
                    command.extend(['--grant', 'Stderr'])
                linker = cc
                if case.first_write is not None:
                    linker = scratch / 'scripted-cc'
                    actual = str(cc or Path(shutil.which('cc')))
                    source = root / 'core/tools/std_host.c'
                    include = root / 'crates/hirsel-core-rt/include'
                    linker.write_text('#!/bin/sh\nexec ' + shlex.quote(actual) + ' \"$@\" ' + shlex.quote(str(source)) + ' -I' + shlex.quote(str(include)) + f' -DFIRST={case.first_write} -DFIRST_OP={case.host_op} -Wl,--wrap=hirsel_core_host_op\n')
                    linker.chmod(0o755)
                if linker:
                    command.extend(['--cc', str(linker)])
                wait_load()
                result = subprocess.run(command, cwd=host, capture_output=True)
                row['ran'], row['cause'] = test_observation(case, result, entry)
                row['passed'] = row['cause'] is None
                if row['cause']:
                    row['owner'] = owner(row['cause'], owners)
                save_row(rows, row, output)
                shutil.rmtree(scratch / 'programs', ignore_errors=True)
            selected = [r for r in rows if r['engine'] == engine]
            print(f"{engine}: {len(selected)} selected, {sum(r['ran'] for r in selected)} ran, {sum(bool(r['passed']) for r in selected)} passed, {sum(r['applicable'] and not r['ran'] for r in selected)} not run, {sum(not r['applicable'] for r in selected)} not applicable, {sum(r['native_only'] for r in selected)} native-only; {time.monotonic() - started:.3f}s", flush=True)
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compiler', type=Path)
    parser.add_argument('--runtime', type=Path)
    parser.add_argument('--evaluator', type=Path, help='test-only hirsel-core-contract-eval backend')
    parser.add_argument('--root', type=Path, default=ROOT)
    parser.add_argument('--backend', choices=['all', 'evaluator', 'both', 'cranelift', 'llvm'], default='both')
    parser.add_argument('--filter', action='append', default=[])
    parser.add_argument('--report', type=Path)
    parser.add_argument('--cc', type=Path)
    parser.add_argument('--target')
    parser.add_argument('--emulator', type=Path)
    parser.add_argument('--sysroot', type=Path)
    parser.add_argument('--timeout', type=int, default=30, help='native execution bound; never bounds a build')
    args = parser.parse_args()
    if bool(args.emulator) != bool(args.sysroot):
        parser.error("--emulator and --sysroot must be supplied together")
    root = args.root.resolve()
    contracts = discover(root, args.filter)
    owners = json.loads(Path(__file__).with_name('std_native_owners.json').read_text())
    engines = ['evaluator', 'cranelift', 'llvm'] if args.backend == 'all' else ['cranelift', 'llvm'] if args.backend == 'both' else [args.backend]
    if args.backend != 'evaluator' and (args.compiler is None or args.runtime is None):
        parser.error('native backends require --compiler and --runtime')
    rows = run(root, args.compiler.resolve() if args.compiler else None, args.runtime.resolve() if args.runtime else None, contracts, engines,
               args.report, owners, args.cc.resolve() if args.cc else None, args.timeout, args.evaluator.resolve() if args.evaluator else None, args.target,
               args.emulator.resolve() if args.emulator else None, args.sysroot.resolve() if args.sysroot else None)
    return int(any(r['applicable'] and not r['passed'] for r in rows))


if __name__ == '__main__':
    try:
        raise SystemExit(main())
    except (ValueError, OSError) as error:
        print(error, flush=True)
        raise SystemExit(2)
