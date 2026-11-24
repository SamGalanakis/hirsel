#!/usr/bin/env python3
"""Run core and std source contract oracles on native engines and the evaluator.

Example: std_native.py --compiler PATH --runtime PATH --filter file --backend both
Confined filesystem cases run on the host, which needs openat2 (Linux >= 5.6).
"""
import argparse
import base64
import contextlib
import dataclasses
from concurrent.futures import Future, ThreadPoolExecutor
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import socket
import ssl
import struct
import subprocess
import sys
import tempfile
import threading
import time
import tomllib

from std_contracts import Contract, discover, host_parameter, matches_filter, test_module
from case_progress import Case

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'tools'))
from loopback_peer import accept_from

LOOPBACK_RULES = ('Connect 127.0.0.0/8 1-65535', 'Listen 127.0.0.0/8 1-65535', 'Tls *.test 0-65535 System')


def fixture_source(file):
    """A contract file's source, with its generated vectors when it opts in."""
    source = file.read_text()
    if source.startswith('// test-vectors\n'):
        generator = file.parent / 'vectors' / file.stem / 'generate.py'
        source += '\n' + subprocess.check_output([sys.executable, generator], text=True)
    return source


def simulation_support(module):
    """Hirsel declarations shared by source-only simulation adapters."""
    return f"""
type ContractSimulationWorker: send {{
    port: Served[NetOp],
    clock: Clock,
    window: u64,
    seed: u64
}}

fn contract_simulation_clock(take input: {module}.world.World) waits {{
    let mut world = take input
    {module}.world.run(mut world)
}}

fn contract_simulation_network(take input: ContractSimulationWorker) waits {{
    let ContractSimulationWorker(port: take port, clock: time, window: window, seed) = take input
    let config = {module}.net.Config(queue_bytes: window, fragment_bytes: window, latency_ns: 0)
    let mut plan = {module}.world.plan(seed)
    let _ = {module}.net.serve({module}.net.new(config), mut plan, time, take port)
}}

fn start_contract_simulation(tasks: Tasks, window: u64) -> (Net, Clock, Task[()], Task[()]) {{
    start_seeded_simulation(tasks, window, 1)
}}

fn start_seeded_simulation(tasks: Tasks, window: u64, seed: u64) -> (Net, Clock, Task[()], Task[()]) {{
    let (world, time) = {module}.world.new(seed, 0, 0)
    let (network, port) = provide.net(bytes 65536)
    let driver = task.spawn(tasks, bytes 65536, take world, contract_simulation_clock)
    let server = task.spawn(tasks, bytes 4194304,
        ContractSimulationWorker(port: take port, clock: time, window: window, seed), contract_simulation_network)
    (network, time, take server, take driver)
}}
"""


def network_adapter(call, expected, module):
    return simulation_support(module) + f"""
@test
pub fn native_contract(tasks: Tasks) -> Result[(), ()] waits {{
    let (network, time, server, driver) = start_contract_simulation(tasks, 8)
    let verdict = {call}
    result.unwrap(task.join(take server))
    result.unwrap(task.join(take driver))
    if verdict == {expected} {{ Ok(()) }} else {{ Error(()) }}
}}
"""


def network_case(case):
    return '(network)' in case.call


LIVE_UDP_CASES = ('net:udp_gro_first_truncates', 'net:udp_group_recycles_a_full_member_capture',
                  'net:udp_pool_charge_outlives_its_helper', 'net:udp_group_retirement_is_an_input')


def unit_socket_case(case):
    return case.name in ('net:socket_served_by_receiving_unit', 'net:inbox_seal_closes_socket',
                         *LIVE_UDP_CASES)


def simulated_network_case(case, evaluating=False):
    return (network_case(case) and not unit_socket_case(case)
            and (evaluating or case.call != 'net_parity(network)'))


def loopback_grants(case):
    rules = LOOPBACK_RULES
    if case.name in LIVE_UDP_CASES:
        rules += ('UdpBind 127.0.0.0/8 1-65535', 'UdpSend 127.0.0.0/8 1-65535')
    return ['--grant', 'Net=' + ';'.join(rules), '--grant', 'Clock', '--grant', 'Tasks']

def exec_fixture(scratch, host):
    """Pin a host executable with a known exit and derive its EXEC-1 approval."""
    image = Path('/usr/bin/true').resolve()
    contents = image.read_bytes()
    pin = hashlib.sha256(contents)
    policy = scratch / 'exec.toml'
    policy.write_text(f'[parity]\nimage = {json.dumps(str(image))}\nsha256 = "{pin.hexdigest()}"\n'
                      f'argv0 = "true"\nargs = [[]]\nenv = {{}}\n'
                      f'dirs = {{ work = {json.dumps(str(host))} }}\n')

    def field(data):
        return struct.pack('<I', len(data)) + data

    # One profile, one empty form, no environment keys, one directory.
    canonical = (b'hirsel.exec-policy.v1' + struct.pack('<I', 1)
                 + field(b'parity') + field(os.fsencode(image)) + pin.digest() + field(b'true')
                 + struct.pack('<IIIII', 1, 0, 0, 0, 1) + field(b'work') + field(os.fsencode(host)))
    digest = hashlib.sha256(canonical).hexdigest()
    interpreter = b'none'
    start = struct.unpack_from('<Q', contents, 32)[0]
    stride, count = struct.unpack_from('<HH', contents, 54)
    for at in range(start, start + stride * count, stride):
        if struct.unpack_from('<I', contents, at)[0] == 3:  # ELF64 PT_INTERP
            offset, _, _, length = struct.unpack_from('<QQQQ', contents, at + 8)
            interpreter = contents[offset:offset + length].rstrip(b'\0')
            break
    approval = (f'exec: parity: image {image}, sha256 {pin.hexdigest()}, interpreter '.encode()
                + interpreter + f', policy {digest}\n'.encode())
    return f'Exec={policy} parity', approval


LANES = {'895': 'core-check-895', '910': 'core-check-calls-910', '912': 'core-check-index-912',
         '913': 'core-check-callables-913', '914': 'core-ops-914'}


@contextlib.contextmanager
def https_fixture(root):
    """TLS-2, TL10, TL11 and TL17 use one real peer and literal HTTP/1.1 wire oracles."""
    certs = root / 'crates/hirsel-core-rt/testdata/tls'
    config = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    config.load_cert_chain(certs / 'alpha.pem', certs / 'alpha.key.pem')
    config.set_alpn_protocols(['http/1.1'])
    raw = os.urandom(32)
    token = raw.hex().encode()
    client = Future()

    def exchange(stream, request, response):
        remaining = memoryview(request)
        while remaining:
            data = stream.recv(len(remaining))
            if not data or data != remaining[:len(data)]:
                raise ValueError('HTTPS request differs from the wire oracle')
            remaining = remaining[len(data):]
        stream.sendall(response)

    def refuse(listener):
        # TL11: a refused body secret sends nothing after the handshake.
        connection = accept_from(listener, client.result(), descendants=True)
        with connection, config.wrap_socket(connection, server_side=True) as stream:
            with contextlib.suppress(ssl.SSLError, OSError):
                if stream.recv(1):
                    raise ValueError('a refused request sent bytes')

    def serve(listener):
        connection = accept_from(listener, client.result(), descendants=True)
        # The second connection must not wait for this one's teardown.
        refused = ThreadPoolExecutor(max_workers=1)
        second = refused.submit(refuse, listener)
        refused.shutdown(wait=False)
        with connection, config.wrap_socket(connection, server_side=True) as stream:
            if stream.selected_alpn_protocol() != 'http/1.1':
                raise ValueError('HTTPS client did not negotiate HTTP/1.1')
            exchange(stream, b'POST /upload HTTP/1.1\r\nHost: alpha.test\r\nContent-Length: 5\r\n\r\nhello',
                     b'HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nworld\r\n0\r\n\r\n')
            # WHATWG URL 5.2 by hand: the sealed "a b+c&d=e%f" and 0xFF is
            # a+b%2Bc%26d%3De%25f%FF (22 bytes); the hex token is unchanged.
            # Content-Length: 39 + 64 + 15 + 22 = 140; the chunk is 2 + 22 = 0x18.
            ok = b'HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok'
            exchange(stream, b'POST /token HTTP/1.1\r\nHost: alpha.test\r\nContent-Type: application/x-www-form-urlencoded'
                     b'\r\nContent-Length: 140\r\n\r\ngrant_type=refresh_token&refresh_token=' + token
                     + b'&client_secret=a+b%2Bc%26d%3De%25f%FF', ok)
            exchange(stream, b'POST /chunk HTTP/1.1\r\nHost: alpha.test\r\nTransfer-Encoding: chunked\r\n\r\n'
                     b'18\r\nv=a+b%2Bc%26d%3De%25f%FF\r\n0\r\n\r\n', ok)
            exchange(stream, b'GET /reject HTTP/1.1\r\nHost: alpha.test\r\nAuthorization: Bearer ' + token
                     + b'\r\nX-Trace: enabled\r\nX-Second: ' + token + b'\r\nContent-Length: 0\r\n\r\n',
                     b'HTTP/1.1 200 OK\r\nContent-Length: 1\r\nTransfer-Encoding: chunked\r\n\r\n')
            with contextlib.suppress(OSError):
                stream.unwrap().close()
        second.result()

    with socket.socket() as listener, ThreadPoolExecutor(max_workers=1) as executor:
        listener.bind(('127.0.0.1', 0))
        listener.listen()
        peer = executor.submit(serve, listener)
        try:
            yield listener.getsockname()[1], (certs / 'ca.pem').resolve(), raw, token, peer, client
        finally:
            client.cancel()
            listener.shutdown(socket.SHUT_RDWR)


def https_observation(case, result, tape, raw, token, peer):
    sealed = tape.read_bytes() if tape.is_file() else b''
    for value in (raw, token):
        for form in (value, base64.b64encode(value)):
            if any(form in output for output in (result.stdout, result.stderr, sealed)):
                return bool(sealed), 'a Secret representation escaped'
    ran, cause = entry_observation(case, result)
    if not ran or cause:
        return ran and bool(sealed), cause
    if not sealed:
        return False, 'HTTPS run did not record a tape'
    try:
        peer.result()
    except (OSError, ValueError) as error:
        return True, str(error)
    return True, None


def entry_observation(case, result):
    diagnostics, _, stdout = result.stdout.partition(b'\n')
    try:
        product = json.loads(diagnostics)
    except ValueError:
        return False, f'run did not report diagnostics: {result.stdout!r} {result.stderr!r}'
    if not product.get('native') or product['diagnostics']:
        return False, first_stop(product) or f'run did not build: {result.stderr!r}'
    return True, observation(case, subprocess.CompletedProcess([], result.returncode, stdout, result.stderr))


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

    def evaluate(self, manifest, entry, program_entry=False, instructions=None, sdk_out=None, sdk_describe=None, events=False, progress=None):
        request = dict(manifest=str(manifest), entry=entry, program_entry=program_entry)
        if events:
            # CONC-10 events are recorded only on request (22f95498).
            request["events"] = True
        if instructions is not None:
            request["instructions"] = instructions
        if sdk_out is not None:
            request.update(sdk_out=str(sdk_out), sdk_describe=str(sdk_describe))
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
    call = call.replace('(clock, tasks)', '(native_clock,native_tasks)')
    call = call.replace('(unix, dir, tasks)', '(native_net,native_dir,native_tasks)')
    call = call.replace('(exec, tasks)', '(native_exec,native_tasks)')
    call = call.replace('(granted, tasks)', '(native_granted,native_tasks)')
    parameters, _ = host_parameter(case)
    effect = ' waits' if case.waits else ''
    if case.argv is not None:
        call = call.replace('(args', '(native_args', 1).replace(', env', ', native_env', 1)
    if port is not None:
        address = f'Addr(ip: V4(2130706433u32), port: {port}u16)'
        call = call.replace('(https)', f'(native_net,native_clock,{address},token,native_out,native_err)')
    if port is not None or case.argv is not None:
        return f'\npub fn native_contract({parameters})->Result[(),Exit]{effect} {{\nif {call} == {case.expected} {{Ok(())}} else {{Error(Exit(status: 1))}}\n}}\n'
    if case.expected.startswith(('fail ', 'fault ', 'stop ')):
        body = f'let _ = {call}\nOk(())'
    else:
        body = f'if {call} == {case.expected} {{Ok(())}} else {{Error(())}}'
    return f'\n@test\npub fn native_contract({parameters})->Result[(),()]{effect} {{\n{body}\n}}\n'


def simulated_case(root, case, source, adapted, scratch):
    module = case.test_of or 'tests/' + case.file.stem
    call = case.call.replace('(network)', '(network,time,tasks)')
    if case.package != 'core':
        adapted.write_text(source + network_adapter(call, case.expected, 'sim' if case.package == 'std' else 'std.sim'))
        return manifest(root, case, adapted), f'{case.package}@1/{module}/native_contract'

    entry = case.call.split('(')[0]
    adapted.write_text(source + f"""
pub fn simulation_case(n: Net, c: Clock, tasks: Tasks) -> bool waits {{
    {entry}(n,c,tasks)
}}
""")
    bridge = scratch / 'sim_contract.hirsel'
    bridge_call = f"core.{module.replace('/', '.')}.simulation_case(network,time,tasks)"
    bridge.write_text(network_adapter(bridge_call, case.expected, 'sim'))
    inputs = manifest(root, case, adapted)
    std = next(p for p in inputs['packages'] if p['package']['name'] == 'std')
    std['sources'].append({'file': str(bridge), 'modules': ['tests', 'sim_contract'], 'signatures': False})
    return inputs, 'std@1/tests/sim_contract/native_contract'


def manifest(root, case, adapted):
    if case.test_of:
        return source_manifest(root, case.package, [case.test_of], adapted, replace=True)
    return source_manifest(root, case.package, ['tests', case.file.stem], adapted)


def library_sources(directory, signatures):
    return [
        {'file': str(p.resolve()), 'modules': list(relative.with_suffix('').parts), 'signatures': signatures}
        for p in sorted(directory.rglob('*.hirsel'))
        if (relative := p.relative_to(directory)).parts[0] != 'tests'
    ]


def source_manifest(root, package, modules, source, *, replace=False):
    core = {'name': 'core', 'version': '1'}
    std = {'name': 'std', 'version': '1'}
    packages = [
        {'package': core, 'sources': library_sources(root / 'core/core', True), 'dependencies': []},
        {'package': std, 'sources': library_sources(root / 'core/std', False), 'dependencies': [core]}]
    if package == 'runner':
        from store_tests import inputs
        packages = inputs(root, package='runner')['packages']
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


def expected_stop_stderr(case):
    name = case.expected.removeprefix('stop ').encode()
    return case.stderr + b'run stop: ' + name + b'\n'


def observation(case, result):
    if case.expected.startswith('fail '):
        kind = case.expected.removeprefix('fail ').encode()
        diagnostics = result.stderr[len(case.stderr):]
        good = (result.returncode == 101 and result.stdout == case.stdout
                and result.stderr.startswith(case.stderr)
                and re.match(re.escape(kind) + rb'(?:\s|$|:|\()', diagnostics))
    elif case.expected.startswith('stop '):
        good = (result.returncode == 102 and result.stdout == case.stdout
                and result.stderr == expected_stop_stderr(case))
    else:
        good = result.returncode == 0 and result.stdout == case.stdout and result.stderr == case.stderr
    if good:
        return None
    return f'exit {result.returncode}, stdout {result.stdout!r}, stderr {result.stderr!r}; expected {case.expected}'


def test_observation(case, result, entry, *, replay=False):
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
    if replay and (result.returncode != 0 or test['status'] != 0):
        return True, f"replay did not seal: test exited {test['status']}: {test['stderr']}"
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
    elif case.expected.startswith('stop '):
        expected = dict(kind='stopped', name=case.expected.removeprefix('stop '))
    else:
        expected = dict(kind='returned', values=[1])
    if outcome != expected:
        return True, f'evaluator got {outcome!r}; expected {expected!r}'
    if expected['kind'] != 'fault':
        stderr = expected_stop_stderr(case) if expected['kind'] == 'stopped' else case.stderr
        if bytes(report.get('stdout', [])) != case.stdout or bytes(report.get('stderr', [])) != stderr:
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


def run(root, compiler, runtime, contracts, engines, output, owners, cc=None, timeout=30, evaluator=None, target=None, emulator=None, sysroot=None, compare_revisions=False, typescript=None, node='node', schedule_runtime=None, record_runtime=None, llvm_profile='release', replay=False):
    rows = []
    requested_engines = set(engines)
    engines = list(engines)
    typescript_override = typescript
    typescript = typescript or root / 'crates/hirsel-core-driver/src/sdk_codec.js'
    frame_codegen = typescript_override is None and any(c.package == 'runner' and c.call.startswith('ts_frame_') for c in contracts)
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
                           engine=engine, profile=None if engine == 'evaluator' else llvm_profile if engine == 'llvm' else 'dev',
                           call=case.call, expected=case.expected, ran=False, passed=False, applicable=True,
                           cause=None, owner=None, native_only=False)
                if engine == 'evaluator' and case.native_only:
                    row.update(applicable=False, passed=None, native_only=True, cause=case.native_only)
                    save_row(rows, row, output)
                    continue
                # The evaluator models tasks and the clock (virtual time); a law
                # whose entry takes another host capability runs natively only.
                if (engine == 'evaluator' or emulator) and '(exec, tasks)' in case.call:
                    row.update(applicable=False, passed=None, native_only=True,
                               cause='native-only: sealed Exec image oracle')
                    save_row(rows, row, output)
                    continue
                if engine == 'evaluator' and '(unix, dir, tasks)' in case.call:
                    row.update(applicable=False, passed=None, native_only=True,
                               cause='native-only: Unix socket and directory oracle')
                    save_row(rows, row, output)
                    continue
                if engine == 'evaluator' and unit_socket_case(case):
                    row.update(applicable=False, passed=None, native_only=True,
                               cause='stay on host walker until R8b')
                    save_row(rows, row, output)
                    continue
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
                if case.name == 'dir:walkdir_preorder':
                    (host / 'link').symlink_to('a', target_is_directory=True)
                adapted = scratch / case.file.name
                original = (root / 'core' / case.package / (case.test_of + '.hirsel')).read_text() + '\n' if case.test_of else ''
                source = original + fixture_source(case.file)
                if case.call.startswith(('ts_codec_vectors(', 'ts_hirsel_differential(', 'ts_frame_vectors(', 'ts_frame_differential(')):
                    frame = case.package == 'runner'
                    if frame and protocol_codec is None and typescript_override is None:
                        description_file = work / 'description.bin'
                        def count(n):
                            return n.to_bytes(4, 'little')
                        def blob(b):
                            return count(len(b)) + b
                        def exported(kind, identity, args, result, args_fp, result_fp, schemas=b""):
                            result_schema = result if kind == 2 else bytes([3, 0, 0, 0, 0]) + result
                            return (count(kind) + blob(identity) + blob(bytes([3, 0, 0, 0, 0]) + args)
                                    + blob(result_schema)
                                    + args_fp.to_bytes(8, 'little') + result_fp.to_bytes(8, 'little') + blob(bytes(32)) + (schemas or bytes(8)))
                        def record_definition(name, field, ty):
                            return (blob(name.encode()) + count(0) + bytes([0])
                                    + count(1) + bytes([0]) + blob(field.encode()) + ty)
                        # Scripted Describe entries exercise the generator, with Frame from std.codec.
                        returned = (bytes([3]) + count(2) + record_definition('fixture.main.Reply', 'n', bytes([6]))
                                    + record_definition('fixture.main.Problem', 'message', bytes([13]))
                                    + bytes([17, 19]) + count(0) + bytes([19]) + count(1))
                        transaction = (count(1) + blob(b'\x01fixture@1/main/commit(take u64)->core@1/core/Result[fixture@1/main/Reply,fixture@1/main/Problem]')
                                       + blob(bytes([3, 0, 0, 0, 0, 6])) + blob(returned)
                                       + (5).to_bytes(8, 'little') + (9).to_bytes(8, 'little') + blob(bytes(32)) + bytes(8))
                        unit = blob(bytes([3, 0, 0, 0, 0, 0]))
                        slots = (count(1) + blob(b"rows") + count(0) + blob(bytes([4, 0, 0, 0, 0, 6]))
                                 + blob(bytes([3, 0, 0, 0, 0, 6]))
                                 + blob(bytes([3, 0, 0, 0, 0, 14, 3])))
                        events_identity = b'\x01fixture@1/main/events(take ())->EventValue'
                        event_value = bytes([3]) + count(1) + blob(b'fixture.main.EventValue') + count(0) + bytes([0]) + count(0) + bytes([19]) + count(0)
                        def event_slot(name, value):
                            return (blob(name) + count(0) + blob(bytes([4, 0, 0, 0, 0, 5]))
                                    + blob(bytes([3, 0, 0, 0, 0, 5])) + blob(bytes([3, 0, 0, 0, 0]) + value))
                        event_slots = count(2) + event_slot(b'unit', bytes([0])) + event_slot(b'option', bytes([16, 5]))
                        event_entry = (count(2) + blob(events_identity) + unit + blob(bytes([3, 0, 0, 0, 0, 0]))
                                       + (11).to_bytes(8, 'little') + (12).to_bytes(8, 'little') + blob(bytes(32)) + event_slots + blob(event_value))
                        event_binding = (blob(b'events') + count(0) + blob(b'/events')
                                         + blob(events_identity + (11).to_bytes(8, 'little') + (12).to_bytes(8, 'little'))
                                         + count(0) + count(1) + (200).to_bytes(2, 'little') + (422).to_bytes(2, 'little')
                                         + b''.join(n.to_bytes(8, 'little') for n in [0, 4096, 1000, 10000]) + count(0))
                        description = (blob(bytes([1, 2])) + blob(b't') + (16777216).to_bytes(8, 'little') + blob(bytes(32)) + count(4)
                                       + exported(0, b'\x01fixture@1/main/toggle(take u64)->u64', b'\x06', b'\x06', 9007199254740993, 7)
                                       + exported(2, b'\x01fixture@1/main/rows(take u32)->Bytes', b'\x05', unit + slots, 2, 3, slots + unit)
                                       + transaction + event_entry + count(0) + count(1) + event_binding)
                        description_file.write_bytes(description)
                        (work / 'package.json').write_text('{"type":"commonjs"}\n')
                        generated = output.with_name(output.stem + '-sdk') if output else work / 'sdk'
                        probe = work / 'frame-schema.hirsel'
                        probe.write_text(case.file.read_text())
                        probe_inputs = work / 'frame-schema.json'
                        probe_inputs.write_text(json.dumps(manifest(root, case, probe)))
                        built = resident.evaluate(probe_inputs, 'runner@1/tests/codec_frames/ts_frame_schema', progress=progress,
                                                  sdk_out=generated, sdk_describe=description_file)
                        report = json.loads(built.stdout)
                        if built.returncode or report.get('error'):
                            resident.errors.seek(0)
                            detail = resident.errors.read().decode(errors='replace')
                            raise ValueError('Frame SDK generation failed: ' + str(report) + '\n' + detail)
                        protocol_codec = generated / 'codec.js'
                    module = (typescript_override or protocol_codec) if frame else typescript
                    law = 'vectors' if case.call.startswith(('ts_codec_vectors(', 'ts_frame_vectors(')) else 'differential'
                    partner_module = root / ('core/programs/runner/tests/codec_frames.mjs' if frame else 'core/std/tests/codec.mjs')
                    program = (f'import * as c from {json.dumps(module.resolve().as_uri())};'
                               f'import {{{law}}} from {json.dumps(partner_module.as_uri())};')
                    if frame:
                        program += f'import * as frame from {json.dumps(module.with_name("frame.js").resolve().as_uri())};'
                        program += f'import * as entries from {json.dumps(module.with_name("entries.js").resolve().as_uri())};'
                    arguments = 'c'
                    if frame:
                        http_url = module.with_name("http.js").resolve().as_uri()
                        arguments = f'c,frame,entries,new URL({json.dumps(http_url)})'
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
                    port, roots, raw, token, peer, client = active.enter_context(https_fixture(root))
                simulated = simulated_network_case(case, evaluating)
                if simulated:
                    source_rows, entry = simulated_case(root, case, source, adapted, scratch)
                else:
                    adapted.write_text(source + wrapper(case, port if https else None))
                    source_rows = manifest(root, case, adapted)
                    entry = f'{case.package}@1/{case.test_of or ("tests/" + case.file.stem)}/native_contract'
                inputs = scratch / 'inputs.json'
                inputs.write_text(json.dumps(source_rows))
                command = [str(compiler), 'run' if https or case.argv is not None else 'test', '--inputs', str(inputs), '--backend', engine,
                           '--runtime', str((record_runtime or runtime) if https or replay else runtime),
                           '--store', str(work / 'store'), '--json', '--limit', str(case.root_limit), '-o', str(scratch / 'programs')]
                if replay:
                    command.extend(['--replay', '--expect-verdict', 'Sealed', '--rerun'])
                environment = None
                if https:
                    tape = scratch / 'https.tape'
                    net = f'Net=Connect 127.0.0.1/32 {port}-{port};Tls alpha.test {port}-{port} System'
                    command.extend(['--entry', entry, '--record', str(tape), '--grant', net, '--grant', 'Clock',
                                    '--grant', 'Stdout', '--grant', 'Stderr',
                                    '--grant', 'Secret:token=env:HIRSEL_TL10_TOKEN to alpha.test'])
                    environment = {**os.environ, 'SSL_CERT_FILE': str(roots), 'HIRSEL_TL10_TOKEN': token.decode()}
                    environment.pop('SSL_CERT_DIR', None)
                elif case.argv is not None:
                    command.extend(['--entry', entry])
                else:
                    command.extend(['--test-timeout', str(timeout), '--dir', str(host)])
                if evaluating:
                    law_started = time.monotonic()
                    result = resident.evaluate(inputs, entry, instructions=case.evaluator_instructions, progress=progress)
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
                        for capability in ('Tasks', 'Clock'):
                            if simulated and capability == 'Tasks' or not simulated and ':' + capability in host_parameter(case)[0]:
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
                # Runner program modules bring their own standalone @tests.
                if case.package == 'runner':
                    command.extend(['--filter', entry])
                if target:
                    command.extend(['--target', target])
                if emulator:
                    command.extend(['--emulator', str(emulator), '--sysroot', str(sysroot)])
                if engine == 'llvm' and llvm_profile == 'release':
                    command.append('--release')
                # All engines read this manifest's checkout sources, as the
                # resident evaluator does, including temporary @test bodies.
                command.append('--source-core')
                if '(out)' in case.call:
                    command.extend(['--grant', 'Stdout'])
                if '(err)' in case.call:
                    command.extend(['--grant', 'Stderr'])
                if '(clock)' in case.call or '(clock, tasks)' in case.call:
                    command.extend(['--grant', 'Clock'])
                if any(args in case.call for args in ('(tasks)', '(dir, tasks)', '(clock, tasks)')):
                    command.extend(['--grant', 'Tasks'])
                if '(exec, tasks)' in case.call:
                    grant, approval = exec_fixture(scratch, host)
                    command.extend(['--grant', 'Tasks', '--grant', grant])
                    case = dataclasses.replace(case, stderr=approval + case.stderr)
                if '(granted, tasks)' in case.call:
                    command.extend(['--grant', 'Tasks', '--grant', 'Secret:native_granted=env:HIRSEL_GRANTED_SECRET to alpha.test kdf'])
                    environment = {**os.environb, b'HIRSEL_GRANTED_SECRET': b'granted-token'}
                if ':Env' in host_parameter(case)[0]:
                    command.extend(['--grant', 'Env=' + ','.join(name for name, _ in case.env_fixture)])
                if case.env_fixture:
                    environment = os.environb.copy()
                    for name, value in case.env_fixture:
                        if value is None:
                            environment.pop(name.encode(), None)
                        else:
                            environment[name.encode()] = value
                linker = cc
                scripts = []
                if simulated:
                    command.extend(['--grant', 'Tasks'])
                elif network_case(case):
                    command.extend(loopback_grants(case))
                elif '(unix, dir, tasks)' in case.call:
                    grant = f'Net=UnixRoot parity {host};UnixBind parity packet;UnixSend parity packet'
                    command.extend(['--grant', grant, '--grant', 'Tasks'])
                if case.first_write is not None:
                    wrapped = ['hirsel_core_host_op' if case.host_op else 'hirsel_core_host_io']
                    if case.host_op == 119:
                        wrapped.extend(['hirsel_core_host_rename', 'getrandom'])
                    scripts.append(('std_host.c', [f'-DFIRST={case.first_write}', f'-DFIRST_OP={case.host_op}'],
                                    wrapped))
                if case.schedule:
                    seeds, most = case.schedule
                    if schedule_runtime is not None:
                        command[command.index('--runtime') + 1] = str(schedule_runtime)
                    scripts.append(('std_schedule.c', [f'-DSEEDS={seeds}'] + ([f'-DMAX_EVENTS={most}u'] if most is not None else []),
                                    ['hirsel_core_os_run_with_sources']))
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
                        extra += [shlex.quote(str(support)), *['-Wl,--wrap=' + symbol for symbol in wrapped]]
                    linker.write_text('#!/bin/sh\nexec ' + shlex.quote(actual) + ' \"$@\" ' + ' '.join(extra) + '\n')
                    linker.chmod(0o755)
                if linker:
                    command.extend(['--cc', str(linker)])
                if case.argv is not None:
                    command.extend(['--', *case.argv])
                result = progress.run(command, cwd=host, capture_output=True, env=environment,
                                      on_start=client.set_result if https else None)
                if https:
                    row['ran'], row['cause'] = https_observation(case, result, tape, raw, token, peer)
                elif case.argv is not None:
                    row['ran'], row['cause'] = entry_observation(case, result)
                else:
                    row['ran'], row['cause'] = test_observation(case, result, entry, replay=replay)
                    for name, value in case.run_env if row['cause'] is None else ():
                        test_environment = {**(os.environb if environment is None else environment),
                                            os.fsencode(name): os.fsencode(value)}
                        again = progress.run(command, cwd=host, capture_output=True, env=test_environment)
                        _, cause = test_observation(case, again, entry, replay=replay)
                        if cause:
                            row['cause'] = f'with {name}={value}: {cause}'
                            break
                row['passed'] = row['cause'] is None
                if replay and row['passed']:
                    row['replay'] = 'Sealed'
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
    parser.add_argument('--replay', action='store_true', help='record and replay selected native contracts, requiring Sealed')
    parser.add_argument('--schedule-runtime', type=Path)
    parser.add_argument('--evaluator', type=Path, help='test-only hirsel-core-contract-eval backend')
    parser.add_argument('--root', type=Path, default=ROOT)
    parser.add_argument('--backend', choices=['all', 'evaluator', 'both', 'cranelift', 'llvm'], default='both')
    parser.add_argument('--llvm-profile', choices=['dev', 'release'], default='release')
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
    if args.replay:
        if args.backend in ('all', 'evaluator') or args.record_runtime is None or not args.filter:
            parser.error('--replay requires a native backend, --record-runtime and a filter')
        if any('(https)' in case.call or case.schedule or case.argv is not None for case in contracts):
            parser.error('--replay requires contracts that run through hirsel test without a scripted scheduler')
    rows = run(root, args.compiler.resolve() if args.compiler else None, args.runtime.resolve() if args.runtime else None, contracts, engines,
               args.report, owners, args.cc.resolve() if args.cc else None, args.timeout, args.evaluator.resolve() if args.evaluator else None, args.target,
               args.emulator.resolve() if args.emulator else None, args.sysroot.resolve() if args.sysroot else None,
               typescript=args.typescript, node=args.node,
               schedule_runtime=args.schedule_runtime.resolve() if args.schedule_runtime else None,
               record_runtime=args.record_runtime.resolve() if args.record_runtime else None,
               llvm_profile=args.llvm_profile, replay=args.replay)
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
