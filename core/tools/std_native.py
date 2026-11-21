#!/usr/bin/env python3
"""Run core and std source contract oracles on native engines and the evaluator.

Example: std_native.py --compiler PATH --runtime PATH --filter file --backend both
Confined filesystem cases run on the host, which needs openat2 (Linux >= 5.6).
"""
import argparse
import base64
import contextlib
import dataclasses
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import socket
import ssl
import subprocess
import sys
import tempfile
import threading
import time

from case_progress import Case

ROOT = Path(__file__).resolve().parents[2]
MODEL_NET_RULES = ('Connect 127.0.0.0/8 1-65535', 'Listen 127.0.0.0/8 0-65535', 'Tls *.test 0-65535 System')


def model_network(case):
    return '(network)' in case.call


def model_grants():
    return ['--grant', 'Net=' + ';'.join(MODEL_NET_RULES), '--grant', 'Clock', '--grant', 'Tasks']

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
    # `// engine-fact: <fact>` before the law: an engine states <fact> to its code
    # generator on the strength of this law, so the native suites run it on each engine.
    engine_fact: str | None = None
    # `// schedule: seeds N [max_events M]` before the law: the native run repeats the
    # law under N seeded schedules (core/tools/std_schedule.c).
    schedule: tuple[int, int | None] | None = None
    root_limit: int = 64 << 20
    evaluator_instructions: int = 1_000_000_000
    # File-level `// test-env: NAME=VALUE`: each native law of the file runs once
    # more with NAME=VALUE set, e.g. to select a portable runtime path.
    run_env: tuple[tuple[str, str], ...] = ()
    # Exact Env grant names. None means absent, and byte values can be non-UTF-8.
    env_fixture: tuple[tuple[str, bytes | None], ...] = ()

    @property
    def name(self):
        prefix = self.package + '.' if self.package != 'std' else ''
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


def matches_filter(root, case, selector):
    return (case.name.startswith(selector)
            or case.package == 'std' and ('std.' + case.name).startswith(selector)
            or str(case.file.relative_to(root)).startswith(selector))


def env_fixture(file, source):
    # // test-env-fixture: {"MISSING": null, "TEXT": "value", "RAW": [255]}
    fixtures = re.findall(r'^// test-env-fixture: (.+)$', source, re.M)
    if not fixtures:
        return ()
    if len(fixtures) != 1:
        raise ValueError(f'{file}: expected one Env fixture')
    entries = json.loads(fixtures[0])
    if not isinstance(entries, dict) or not entries:
        raise ValueError(f'{file}: Env fixture must name at least one key')
    result = []
    for name, value in entries.items():
        if not re.fullmatch(r'[A-Z0-9_]+', name):
            raise ValueError(f'{file}: invalid Env fixture key {name!r}')
        if isinstance(value, str):
            value = value.encode()
        elif isinstance(value, list):
            value = bytes(value)
        elif value is not None:
            raise ValueError(f'{file}: invalid Env fixture value for {name}')
        if value is not None and b'\0' in value:
            raise ValueError(f'{file}: NUL in Env fixture value for {name}')
        result.append((name, value))
    return tuple(result)


def discover(root, filters):
    contracts = []
    files = [p for package in ['core', 'std'] for p in (root / 'core' / package / 'tests').glob('*.hirsel')]
    files += list((root / 'core/programs/store/tests').glob('*.hirsel'))
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
        run_env = tuple(tuple(pair) for pair in re.findall(r'^// test-env: ([A-Z0-9_]+)=(\S+)$', source, re.M))
        fixture = env_fixture(file, source)
        stdout = stderr = b""
        first_write = None
        host_op = 0
        engine_fact = None
        schedule = None
        root_limit = 64 << 20
        evaluator_instructions = 1_000_000_000
        for line, text in enumerate(source.splitlines(), 1):
            if text.startswith("// test-limit: "):
                root_limit = int(text.removeprefix("// test-limit: "))
                if not 0 < root_limit < 1 << 64:
                    raise ValueError(f'{file}:{line}: test limit is outside u64')
            if text.startswith("// test-instructions: "):
                evaluator_instructions = int(text.removeprefix("// test-instructions: "))
                if not 0 < evaluator_instructions < 1 << 64:
                    raise ValueError(f'{file}:{line}: evaluator instruction limit is outside u64')
            if text.startswith("// schedule: "):
                words = text.removeprefix("// schedule: ").split()
                if len(words) not in (2, 4) or words[0] != 'seeds' or words[2:3] not in ([], ['max_events']):
                    raise ValueError(f'{file}:{line}: unreadable schedule line')
                schedule = (int(words[1]), int(words[3]) if len(words) == 4 else None)
            if text.startswith("// engine-fact: "):
                engine_fact = text.removeprefix("// engine-fact: ").strip()
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
            if '(env)' in call and not fixture:
                raise ValueError(f'{file}:{line}: Env law needs a fixture')
            if expected.startswith('fault ') and not evaluator_only:
                raise ValueError(f'{file}:{line}: fault oracle requires // test-backend: evaluator')
            if expected.startswith('fault '):
                if not isinstance(json.loads(expected.removeprefix('fault ')), str):
                    raise ValueError(f'{file}:{line}: fault oracle must be an exact JSON string')
            if evaluator_only and (not call.endswith('()') or first_write is not None or schedule):
                raise ValueError(f'{file}:{line}: evaluator-only laws take no host arguments')
            name = call.split('(')[0]
            header = re.search(r'\bfn\s+' + re.escape(name) + r'\s*\([^)]*\)([^\{]*)\{', source)
            if header is None:
                raise ValueError(f'{file}:{line}: no function for {call}')
            package = 'store' if file.parent == root / 'core/programs/store/tests' else file.relative_to(root).parts[1]
            case = Contract(file, line, call, expected, bool(re.search(r'\bwaits\b', header[1])), stdout, stderr, first_write, test_of, host_op, package, evaluator_only, engine_fact, schedule, root_limit, evaluator_instructions, run_env, fixture)
            schedule = None
            root_limit = 64 << 20
            evaluator_instructions = 1_000_000_000
            stdout = stderr = b""
            first_write = None
            host_op = 0
            engine_fact = None
            if not filters or any(matches_filter(root, case, selector) for selector in filters):
                contracts.append(case)
    if not contracts:
        raise ValueError(f'no expect: run tests match {filters!r}')
    return contracts


def host_parameter(case):
    if '(https)' in case.call:
        return 'native_net:Net,native_clock:Clock,token:Secret,native_out:Stdout,native_err:Stderr', ''
    if model_network(case):
        return 'native_net:Net,native_clock:Clock,native_tasks:Tasks', ''
    if '(granted, tasks)' in case.call:  # An entry-grant Secret: native only.
        return 'native_tasks:Tasks,native_granted:Secret', ''
    if '(dir, tasks)' in case.call:  # A host Dir as the oracle beside served ports.
        return 'native_dir:Dir,native_tasks:Tasks', ''
    for argument, kind in [('dir', 'Dir'), ('out', 'Stdout'), ('err', 'Stderr'), ('clock', 'Clock'), ('tasks', 'Tasks'), ('env', 'Env')]:
        if '(' + argument + ')' in case.call:
            name = 'native_' + argument
            return name + ':' + kind, name
    return '', ''


@contextlib.contextmanager
def https_fixture(root):
    """TLS-2 and TL10 use one real peer and literal HTTP/1.1 wire oracles."""
    certs = root / 'crates/hirsel-core-rt/testdata/tls'
    config = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    config.load_cert_chain(certs / 'alpha.pem', certs / 'alpha.key.pem')
    config.set_alpn_protocols(['http/1.1'])
    raw = os.urandom(32)
    token = raw.hex().encode()

    def exchange(stream, request, response):
        remaining = memoryview(request)
        while remaining:
            data = stream.recv(len(remaining))
            if not data or data != remaining[:len(data)]:
                raise ValueError('HTTPS request differs from the wire oracle')
            remaining = remaining[len(data):]
        stream.sendall(response)

    def serve(listener):
        connection, _ = listener.accept()
        with connection, config.wrap_socket(connection, server_side=True) as stream:
            if stream.selected_alpn_protocol() != 'http/1.1':
                raise ValueError('HTTPS client did not negotiate HTTP/1.1')
            exchange(stream, b'POST /upload HTTP/1.1\r\nHost: alpha.test\r\nContent-Length: 5\r\n\r\nhello',
                     b'HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nworld\r\n0\r\n\r\n')
            exchange(stream, b'GET /reject HTTP/1.1\r\nHost: alpha.test\r\nAuthorization: Bearer ' + token
                     + b'\r\nX-Trace: enabled\r\nX-Second: ' + token + b'\r\nContent-Length: 0\r\n\r\n',
                     b'HTTP/1.1 200 OK\r\nContent-Length: 1\r\nTransfer-Encoding: chunked\r\n\r\n')
            with contextlib.suppress(OSError):
                stream.unwrap().close()

    with socket.socket() as listener, ThreadPoolExecutor(max_workers=1) as executor:
        listener.bind(('127.0.0.1', 0))
        listener.listen()
        peer = executor.submit(serve, listener)
        try:
            yield listener.getsockname()[1], (certs / 'ca.pem').resolve(), raw, token, peer
        finally:
            listener.shutdown(socket.SHUT_RDWR)


def https_observation(case, result, tape, raw, token, peer):
    sealed = tape.read_bytes() if tape.is_file() else b''
    for value in (raw, token):
        for form in (value, base64.b64encode(value)):
            if any(form in output for output in (result.stdout, result.stderr, sealed)):
                return bool(sealed), 'a Secret representation escaped'
    diagnostics, _, stdout = result.stdout.partition(b'\n')
    try:
        product = json.loads(diagnostics)
    except ValueError:
        return False, f'HTTPS run did not report diagnostics: {result.stdout!r} {result.stderr!r}'
    if not product.get('native'):
        return False, first_stop(product) or f'HTTPS run did not build: {result.stderr!r}'
    if product['diagnostics']:
        return False, f'HTTPS run reported diagnostics: {product["diagnostics"]!r}'
    cause = observation(case, subprocess.CompletedProcess([], result.returncode, stdout, result.stderr))
    if cause:
        return bool(sealed), cause
    if not sealed:
        return False, 'HTTPS run did not record a tape'
    try:
        peer.result()
    except (OSError, ValueError) as error:
        return True, str(error)
    return True, None


class Evaluator:
    """One checked-product store per gate, one fresh evaluator world per request."""
    def __init__(self, binary):
        self.binary = binary
        self.errors = tempfile.TemporaryFile()
        self.active_case = None
        self.process = subprocess.Popen([str(binary)], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=subprocess.PIPE)
        self.stderr_reader = threading.Thread(target=self.read_errors)
        self.stderr_reader.start()

    def read_errors(self):
        with self.process.stderr as errors:
            while chunk := errors.read1(8192):
                self.errors.write(chunk)
                if progress := self.active_case:
                    progress.output(errors, chunk)

    def __enter__(self):
        return self

    def evaluate(self, manifest, entry, program_entry=False, instructions=None, sdk_out=None, sdk_describe=None, sdk_compiler=None, sdk_native_options=None, network=False, events=False, progress=None):
        request = dict(manifest=str(manifest), entry=entry, program_entry=program_entry)
        if events:
            # CONC-10 events are recorded only on request (22f95498).
            request["events"] = True
        if instructions is not None:
            request["instructions"] = instructions
        if network:
            request["model_net"] = MODEL_NET_RULES
        if sdk_out is not None:
            request.update(sdk_out=str(sdk_out), sdk_describe=str(sdk_describe))
        if sdk_compiler is not None:
            request.update(sdk_compiler=str(sdk_compiler), sdk_native_options=sdk_native_options)
        with (contextlib.nullcontext(progress) if progress else Case(entry, 'evaluator')) as active:
            self.active_case = active
            line = bytearray()
            try:
                with contextlib.suppress(BrokenPipeError):
                    self.process.stdin.write(json.dumps(request).encode() + b'\n')
                    self.process.stdin.flush()
                    while chunk := self.process.stdout.peek(8192):
                        line_end = chunk.find(b'\n') + 1
                        read = self.process.stdout.read(line_end or len(chunk))
                        active.output(self.process.stdout, read)
                        line.extend(read)
                        if line_end:
                            break
                    active.end_stream(self.process.stdout)
                if line:
                    return subprocess.CompletedProcess([str(self.binary)], 0, bytes(line), b'')
                status = self.process.wait()
                self.stderr_reader.join()
                self.errors.seek(0)
                return subprocess.CompletedProcess([str(self.binary)], status or 1, b'', self.errors.read())
            finally:
                self.active_case = None

    def __exit__(self, *exc):
        with contextlib.suppress(BrokenPipeError):
            self.process.stdin.close()
        self.process.stdout.close()
        self.process.wait()
        self.stderr_reader.join()
        self.errors.close()


def wrapper(case, port=None):
    # The adapter lives in the original module, so private helpers remain private.
    call = case.call.replace('(dir)', '(native_dir)').replace('(out)', '(native_out)').replace('(err)', '(native_err)').replace('(clock)', '(native_clock)').replace('(tasks)', '(native_tasks)')
    call = call.replace('(dir, tasks)', '(native_dir,native_tasks)').replace('(network)', '(native_net,native_clock,native_tasks)').replace('(env)', '(native_env)')
    call = call.replace('(granted, tasks)', '(native_granted,native_tasks)')
    parameters, _ = host_parameter(case)
    effect = ' waits' if case.waits else ''
    if port is not None:
        address = f'Addr(ip: V4(2130706433u32), port: {port}u16)'
        call = call.replace('(https)', f'(native_net,native_clock,{address},token,native_out,native_err)')
        return f'\npub fn native_contract({parameters})->Result[(),Exit]{effect} {{\nif {call} == {case.expected} {{Ok(())}} else {{Error(Exit(status: 1))}}\n}}\n'
    if case.expected.startswith(('fail ', 'fault ')):
        body = f'let _ = {call}\nOk(())'
    else:
        body = f'if {call} == {case.expected} {{Ok(())}} else {{Error(())}}'
    return f'\n@test\npub fn native_contract({parameters})->Result[(),()]{effect} {{\n{body}\n}}\n'


def manifest(root, case, adapted):
    if case.test_of:
        return source_manifest(root, case.package, [case.test_of], adapted, replace=True)
    return source_manifest(root, case.package, ['tests', case.file.stem], adapted)


def source_manifest(root, package, modules, source, *, replace=False):
    core = {'name': 'core', 'version': '1'}
    std = {'name': 'std', 'version': '1'}
    def sources(directory, signatures):
        return [
            {'file': str(p.resolve()), 'modules': list(relative.with_suffix('').parts), 'signatures': signatures}
            for p in sorted(directory.rglob('*.hirsel'))
            if (relative := p.relative_to(directory)).parts[0] != 'tests'
        ]
    packages = [
        {'package': core, 'sources': sources(root / 'core/core', True), 'dependencies': []},
        {'package': std, 'sources': sources(root / 'core/std', False), 'dependencies': [core]}]
    if package == 'store':
        packages.append({'package': {'name': 'store', 'version': '1'}, 'dependencies': [core, std],
                         'sources': [{'file': str(root / 'core/programs/store' / (name + '.hirsel')),
                                      'modules': [name], 'signatures': False} for name in ['runner', 'store', 'ingress']]})
    files = next(p['sources'] for p in packages if p['package']['name'] == package)
    if replace:
        original = next(f for f in files if f['modules'] == modules)
        original.update(file=str(source), signatures=package == 'core')
    else:
        files.append({'file': str(source), 'modules': modules, 'signatures': False})
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
        diagnostics = result.stderr[len(case.stderr):]
        good = (result.returncode == 101 and result.stdout == case.stdout
                and result.stderr.startswith(case.stderr)
                and re.match(re.escape(kind) + rb'(?:\s|$|:|\()', diagnostics))
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
          (f" ({row['seconds']:.3f}s, {row['instruction_budget']} instructions)" if 'instruction_budget' in row else
           f" ({row['seconds']:.3f}s of {row['bound']}s)" if 'seconds' in row else '') +
          (f" [owner {row['owner']}]" if row['owner'] else ''), flush=True)
    if output:
        output.write_text(json.dumps(rows, indent=2) + '\n')


def run(root, compiler, runtime, contracts, engines, output, owners, cc=None, timeout=30, evaluator=None, target=None, emulator=None, sysroot=None, compare_revisions=False, typescript=None, node='node', model_runtime=None, schedule_runtime=None, record_runtime=None):
    rows = []
    requested_engines = set(engines)
    engines = list(engines)
    typescript_override = typescript
    typescript = typescript or root / 'crates/hirsel-core-driver/src/sdk_codec.js'
    frame_codegen = typescript_override is None and any(c.package == 'store' and c.call.startswith('ts_frame_') for c in contracts)
    if frame_codegen and evaluator is None:
        raise ValueError('Frame codec laws require --evaluator to derive runner.Frame')
    if any(c.evaluator_only for c in contracts) and 'evaluator' not in engines:
        engines.insert(0, 'evaluator')
    if 'evaluator' in engines and evaluator is None:
        raise ValueError('evaluator laws require --evaluator <hirsel-core-contract-eval>')
    # Private stores are shared across cases only for immutable checked/native products.
    # Each case's host state is fresh and its executable is removed after observation.
    with (tempfile.TemporaryDirectory(prefix='hirsel-std-native-') as temporary,
          contextlib.ExitStack() as processes, contextlib.ExitStack() as active):
        work = Path(temporary)
        protocol_codec = None
        resident = processes.enter_context(Evaluator(evaluator)) if 'evaluator' in engines or frame_codegen else None
        for engine in engines:
            started = time.monotonic()
            for index, case in enumerate(contracts):
                active.close()
                # Preserve the historical automatic evaluator-only runs for native
                # selections; ordinary laws run here only when explicitly selected.
                if engine == 'evaluator' and 'evaluator' not in requested_engines and not case.evaluator_only:
                    continue
                row = dict(test=case.name, file=str(case.file.relative_to(root)), line=case.line,
                           engine=engine, profile=None if engine == 'evaluator' else 'release' if engine == 'llvm' else 'dev',
                           call=case.call, expected=case.expected, ran=False, passed=False, applicable=True,
                           cause=None, owner=None, native_only=False)
                # The evaluator models tasks and the clock (virtual time); a law
                # whose entry takes another host capability runs natively only.
                if engine == 'evaluator' and host_parameter(case)[0] and not host_parameter(case)[0].endswith((':Tasks', ':Clock')):
                    row.update(applicable=False, passed=None, native_only=True,
                               cause='native-only: entry takes a host capability')
                    save_row(rows, row, output)
                    continue
                if engine != 'evaluator' and case.evaluator_only:
                    row.update(applicable=False, passed=None, cause=case.evaluator_only)
                    save_row(rows, row, output)
                    continue
                evaluating = engine == 'evaluator'
                progress = active.enter_context(Case(case.name, engine))
                scratch = work / f'{engine}-{index}'
                scratch.mkdir()
                host = scratch / 'host'
                host.mkdir()  # Script: every contract receives one fresh, empty confined Dir.
                adapted = scratch / case.file.name
                original = (root / 'core' / case.package / (case.test_of + '.hirsel')).read_text() + '\n' if case.test_of else ''
                fixture = case.file.read_text()
                source = original + fixture
                if fixture.startswith('// test-vectors\n'):
                    generator = case.file.parent / 'vectors' / case.file.stem / 'generate.py'
                    source += '\n' + subprocess.check_output([sys.executable, generator], text=True)
                if case.call.startswith(('ts_codec_vectors(', 'ts_hirsel_differential(', 'ts_frame_vectors(', 'ts_frame_differential(')):
                    frame = case.package == 'store'
                    if frame and protocol_codec is None and typescript_override is None:
                        description_file = work / 'description.bin'
                        def count(n):
                            return n.to_bytes(4, 'little')
                        def blob(b):
                            return count(len(b)) + b
                        def exported(kind, identity, args, result, args_fp, result_fp):
                            return (count(kind) + blob(identity) + blob(bytes([3, 0, 0, 0, 0]) + args)
                                    + blob(bytes([3, 0, 0, 0, 0]) + result)
                                    + args_fp.to_bytes(8, 'little') + result_fp.to_bytes(8, 'little') + blob(bytes(32)))
                        def record_definition(name, field, ty):
                            return (blob(name.encode()) + count(0) + bytes([0])
                                    + count(1) + bytes([0]) + blob(field.encode()) + ty)
                        # Scripted entries exercise the generator and stubs without
                        # depending on the launch/Describe producer. Frame comes
                        # from the actual runner declaration through std.codec.
                        returned = (bytes([3]) + count(2) + record_definition('fixture.main.Reply', 'n', bytes([6]))
                                    + record_definition('fixture.main.Problem', 'message', bytes([13]))
                                    + bytes([17, 19]) + count(0) + bytes([19]) + count(1))
                        transaction = (count(1) + blob(b'\x01fixture@1/main/commit(take u64)->core@1/core/Result[fixture@1/main/Reply,fixture@1/main/Problem]')
                                       + blob(bytes([3, 0, 0, 0, 0, 6])) + blob(returned)
                                       + (5).to_bytes(8, 'little') + (9).to_bytes(8, 'little') + blob(bytes(32)))
                        description = (blob(bytes([1, 2])) + blob(b't') + (16777216).to_bytes(8, 'little') + count(3)
                                       + exported(0, b'\x01fixture@1/main/toggle(take u64)->u64', b'\x06', b'\x06', 9007199254740993, 7)
                                       + exported(2, b'\x01fixture@1/main/rows(take u32)->Bytes', b'\x05', b'\x0e\x03', 2, 3) + transaction)
                        description_file.write_bytes(description)
                        (work / 'package.json').write_text('{"type":"commonjs"}\n')
                        generated = output.with_name(output.stem + '-sdk') if output else work / 'sdk'
                        probe = work / 'frame-schema.hirsel'
                        probe.write_text(case.file.read_text())
                        probe_inputs = work / 'frame-schema.json'
                        probe_inputs.write_text(json.dumps(manifest(root, case, probe)))
                        native_options = ['--runtime', str(runtime)] if compiler and runtime else None
                        if native_options and cc:
                            native_options += ['--cc', str(cc)]
                        built = resident.evaluate(probe_inputs, 'store@1/tests/codec_frames/ts_frame_schema', progress=progress,
                                                  sdk_out=generated, sdk_describe=description_file,
                                                  sdk_compiler=compiler if native_options else None,
                                                  sdk_native_options=native_options)
                        report = json.loads(built.stdout)
                        if built.returncode or report.get('error'):
                            resident.errors.seek(0)
                            detail = resident.errors.read().decode(errors='replace')
                            raise ValueError('Frame SDK generation failed: ' + str(report) + '\n' + detail)
                        protocol_codec = generated / 'codec.js'
                    module = (typescript_override or protocol_codec) if frame else typescript
                    law = 'vectors' if case.call.startswith(('ts_codec_vectors(', 'ts_frame_vectors(')) else 'differential'
                    partner_module = root / ('core/programs/store/tests/codec_frames.mjs' if frame else 'core/std/tests/codec.mjs')
                    program = (f'import * as c from {json.dumps(module.resolve().as_uri())};'
                               f'import {{{law}}} from {json.dumps(partner_module.as_uri())};')
                    if frame:
                        program += f'import * as frame from {json.dumps(module.with_name("frame.js").resolve().as_uri())};'
                        program += f'import * as entries from {json.dumps(module.with_name("entries.js").resolve().as_uri())};'
                    arguments = 'c,frame,entries' if frame else 'c'
                    program += f'console.log(JSON.stringify(await {law}({arguments})));'
                    partner = progress.run([str(node), '--input-type=module', '--eval', program], capture_output=True, text=True)
                    if partner.returncode:
                        row.update(ran=True, passed=False, cause='TypeScript codec: ' + partner.stderr, seconds=0, instruction_budget=0)
                        save_row(rows, row, output)
                        continue
                    values = json.loads(partner.stdout)
                    if values.get('cases', 0) <= 0:
                        raise ValueError('TypeScript codec partner executed zero cases')
                    if law == 'differential' and (values['cases'] != 10000 or not values.get('hex')):
                        raise ValueError('TypeScript differential requires 10,000 encoded cases')
                    row['typescript_cases'] = values['cases']
                    if law == 'differential':
                        pattern = (r'(fn ts_frame_bytes\(\) -> String\s*\{\s*)""'
                                   if frame else r'(const TS_DIFFERENTIAL_BYTES: String = )""')
                        source, count = re.subn(pattern, lambda m: m[1] + json.dumps(values['hex']), source)
                        if count != 1:
                            raise ValueError('TypeScript differential must supply exactly one corpus')
                https = '(https)' in case.call
                if https:
                    port, roots, raw, token, peer = active.enter_context(https_fixture(root))
                adapted.write_text(source + wrapper(case, port if https else None))
                inputs = scratch / 'inputs.json'
                inputs.write_text(json.dumps(manifest(root, case, adapted)))
                entry = f'{case.package}@1/{case.test_of or ("tests/" + case.file.stem)}/native_contract'
                command = [str(compiler), 'run' if https else 'test', '--inputs', str(inputs), '--backend', engine,
                           '--runtime', str(record_runtime or runtime) if https else str(runtime), '--store', str(work / 'store'), '--json', '--limit', str(case.root_limit),
                           '-o', str(scratch / 'programs')]
                environment = None
                if https:
                    tape = scratch / 'https.tape'
                    net = f'Net=Connect 127.0.0.1/32 {port}-{port};Tls alpha.test {port}-{port} System'
                    command.extend(['--entry', entry, '--record', str(tape), '--grant', net, '--grant', 'Clock',
                                    '--grant', 'Stdout', '--grant', 'Stderr',
                                    '--grant', 'Secret:token=env:HIRSEL_TL10_TOKEN to alpha.test'])
                    environment = {**os.environ, 'SSL_CERT_FILE': str(roots), 'HIRSEL_TL10_TOKEN': token.decode()}
                    environment.pop('SSL_CERT_DIR', None)
                else:
                    command.extend(['--test-timeout', str(timeout), '--dir', str(host)])
                if evaluating:
                    law_started = time.monotonic()
                    result = resident.evaluate(inputs, entry, network=model_network(case),
                                               instructions=case.evaluator_instructions, progress=progress)
                    row['ran'], row['cause'] = evaluator_observation(case, result)
                    try:
                        row['revision'] = json.loads(result.stdout).get('revision')
                    except ValueError:
                        row['revision'] = None
                    row['seconds'] = round(time.monotonic() - law_started, 3)
                    if compare_revisions:
                        reference = [str(compiler), 'build', '--source-core', '--inputs', str(inputs), '--entry', entry,
                                     '--backend', 'cranelift', '--runtime', str(runtime), '--store', str(work / 'revision-store'),
                                     '--json', '--limit', str(case.root_limit), '-o', str(scratch / 'revision-program')]
                        if model_network(case):
                            reference.extend(model_grants())
                        else:
                            for capability in ('Tasks', 'Clock'):
                                if host_parameter(case)[0].endswith(':' + capability):
                                    reference.extend(['--grant', capability])
                        if cc:
                            reference.extend(['--cc', str(cc)])
                        built = progress.run(reference, capture_output=True, cwd=host)
                        try:
                            product = json.loads(built.stdout)
                        except ValueError:
                            product = {}
                        native_revision = (product.get('native') or {}).get('revision')
                        row['build_revision'] = native_revision
                        row['build_status'] = built.returncode
                        row['revision_matches'] = row['revision'] is not None and row['revision'] == native_revision
                        if not row['revision_matches']:
                            row['revision_cause'] = (f"entry revisions differ: evaluator {row['revision']}, build {native_revision}"
                                                     if native_revision is not None else first_stop(product) if product else
                                                     f'build exited {built.returncode}: {built.stdout!r} {built.stderr!r}')
                            row['cause'] = (row['cause'] + '; L1: ' + row['revision_cause']) if row['cause'] else row['revision_cause']
                        (scratch / 'revision-program').unlink(missing_ok=True)
                    row['instruction_budget'] = case.evaluator_instructions
                    row['passed'] = row['cause'] is None
                    if row['cause']:
                        row['owner'] = ('core-ops-914' if any(message in row['cause'] for message in ('external functions do not run here', 'duplicate closed type rows')) else
                                        'core-check-895' if not row['ran'] else 'core-form-883')
                    save_row(rows, row, output)
                    continue
                # Store program modules bring their own standalone @tests.
                if case.package == 'store':
                    command.extend(['--filter', entry])
                if target:
                    command.extend(['--target', target])
                if emulator:
                    command.extend(['--emulator', str(emulator), '--sysroot', str(sysroot)])
                if engine == 'llvm':
                    command.append('--release')
                # All engines read this manifest's checkout sources, as the
                # resident evaluator does, including temporary @test bodies.
                command.append('--source-core')
                if '(out)' in case.call:
                    command.extend(['--grant', 'Stdout'])
                if '(err)' in case.call:
                    command.extend(['--grant', 'Stderr'])
                if '(clock)' in case.call:
                    command.extend(['--grant', 'Clock'])
                if '(tasks)' in case.call or '(dir, tasks)' in case.call:
                    command.extend(['--grant', 'Tasks'])
                if '(granted, tasks)' in case.call:
                    command.extend(['--grant', 'Tasks', '--grant', 'Secret:native_granted=env:HIRSEL_GRANTED_SECRET to alpha.test'])
                    environment = {**os.environb, b'HIRSEL_GRANTED_SECRET': b'granted-token'}
                if '(env)' in case.call:
                    command.extend(['--grant', 'Env=' + ','.join(name for name, _ in case.env_fixture)])
                    environment = os.environb.copy()
                    for name, value in case.env_fixture:
                        if value is None:
                            environment.pop(name.encode(), None)
                        else:
                            environment[name.encode()] = value
                linker = cc
                scripts = []
                if model_network(case):
                    if model_runtime is None:
                        raise ValueError('native model-network laws require --model-runtime')
                    if case.schedule or case.first_write is not None:
                        raise ValueError('a model-network law uses its own provider and scheduler')
                    command[command.index('--runtime') + 1] = str(model_runtime)
                    command.extend(model_grants())
                    linker = scratch / 'model-net-cc'
                    actual = str(cc or Path(shutil.which('cc')))
                    linker.write_text('#!/bin/sh\nexec ' + shlex.quote(actual) +
                                      ' "$@" -Wl,--wrap=hirsel_core_os_run_with_sources\n')
                    linker.chmod(0o755)
                if case.first_write is not None:
                    wrapped = 'hirsel_core_host_op' if case.host_op else 'hirsel_core_host_io'
                    scripts.append(('std_host.c', [f'-DFIRST={case.first_write}', f'-DFIRST_OP={case.host_op}'],
                                    wrapped))
                if case.schedule:
                    seeds, most = case.schedule
                    if schedule_runtime is not None:
                        command[command.index('--runtime') + 1] = str(schedule_runtime)
                    scripts.append(('std_schedule.c', [f'-DSEEDS={seeds}'] + ([f'-DMAX_EVENTS={most}u'] if most is not None else []),
                                    'hirsel_core_os_run_with_sources'))
                if scripts:
                    (scratch / 'bin').mkdir()
                    linker = scratch / 'bin/scripted-cc'
                    actual = str(cc or Path(shutil.which('cc')))
                    # Prebuilt objects: the direct-link recipe keeps an existing support
                    # object for every C driver, while an appended source is recovered only
                    # from clang's -cc1 lines, never gcc's (the aarch64 cross driver).
                    extra = []
                    for source, defines, wrapped in scripts:
                        support = scratch / source.replace('.c', '.o')
                        built = progress.run([actual, '-c', '-fPIC', str(root / 'core/tools' / source),
                                                '-I' + str(root / 'crates/hirsel-core-rt/include'),
                                                *defines, '-o', str(support)], capture_output=True)
                        if built.returncode:
                            raise RuntimeError(f'{source}: {built.stderr.decode()}')
                        extra += [shlex.quote(str(support)), '-Wl,--wrap=' + wrapped]
                    linker.write_text('#!/bin/sh\nexec ' + shlex.quote(actual) + ' \"$@\" ' + ' '.join(extra) + '\n')
                    linker.chmod(0o755)
                if linker:
                    command.extend(['--cc', str(linker)])
                result = progress.run(command, cwd=host, capture_output=True, env=environment)
                if https:
                    row['ran'], row['cause'] = https_observation(case, result, tape, raw, token, peer)
                else:
                    row['ran'], row['cause'] = test_observation(case, result, entry)
                    for name, value in case.run_env if row['cause'] is None else ():
                        test_environment = {**(os.environb if environment is None else environment),
                                            os.fsencode(name): os.fsencode(value)}
                        again = progress.run(command, cwd=host, capture_output=True, env=test_environment)
                        _, cause = test_observation(case, again, entry)
                        if cause:
                            row['cause'] = f'with {name}={value}: {cause}'
                            break
                row['passed'] = row['cause'] is None
                if row['cause']:
                    row['owner'] = owner(row['cause'], owners)
                save_row(rows, row, output)
                shutil.rmtree(scratch / 'programs', ignore_errors=True)
            selected = [r for r in rows if r['engine'] == engine]
            if compare_revisions and engine == 'evaluator':
                applicable = [r for r in selected if r['applicable']]
                print(f"revision law: {sum(r.get('revision_matches', False) for r in applicable)}/{len(applicable)} match", flush=True)
            print(f"{engine}: {len(selected)} selected, {sum(r['ran'] for r in selected)} ran, {sum(bool(r['passed']) for r in selected)} passed, {sum(r['applicable'] and not r['ran'] for r in selected)} not run, {sum(not r['applicable'] for r in selected)} not applicable, {sum(r['native_only'] for r in selected)} native-only; {time.monotonic() - started:.3f}s", flush=True)
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compiler', type=Path)
    parser.add_argument('--runtime', type=Path)
    parser.add_argument('--record-runtime', type=Path, default=os.environ.get('HIRSEL_CORE_RT_RECORD_ARCHIVE'))
    parser.add_argument('--model-runtime', type=Path)
    parser.add_argument('--schedule-runtime', type=Path)
    parser.add_argument('--evaluator', type=Path, help='test-only hirsel-core-contract-eval backend')
    parser.add_argument('--root', type=Path, default=ROOT)
    parser.add_argument('--backend', choices=['all', 'evaluator', 'both', 'cranelift', 'llvm'], default='both')
    parser.add_argument('--filter', action='append', default=[])
    parser.add_argument('--report', type=Path)
    parser.add_argument('--typescript', type=Path, help='generated codec.js ES module; run its codec corpus partner')
    parser.add_argument('--node', default='node', help='Node >=22 executable, required by the codec corpus')
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
               args.emulator.resolve() if args.emulator else None, args.sysroot.resolve() if args.sysroot else None,
               typescript=args.typescript, node=args.node,
               model_runtime=args.model_runtime.resolve() if args.model_runtime else None,
               schedule_runtime=args.schedule_runtime.resolve() if args.schedule_runtime else None,
               record_runtime=args.record_runtime.resolve() if args.record_runtime else None)
    ran = {(row['test'], row['file']) for row in rows if row['ran']}
    empty = [selector for selector in args.filter
             if not any(matches_filter(root, case, selector) and
                        (case.name, str(case.file.relative_to(root))) in ran for case in contracts)]
    if empty:
        for selector in empty:
            print(f'no test bodies ran for filter {selector!r}', file=sys.stderr)
        return 1
    return int(any(r['applicable'] and not r['passed'] for r in rows))


if __name__ == '__main__':
    try:
        raise SystemExit(main())
    except (ValueError, OSError) as error:
        print(error, flush=True)
        raise SystemExit(2)
