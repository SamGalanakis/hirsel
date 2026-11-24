"""Fixtures for the native runner host laws."""
import ctypes
import ctypes.util
from contextlib import contextmanager
import hashlib
import json
import os
import shutil
import struct
import tempfile

from case_progress import run_case
from store_tests import inputs
from store_crash_tests import checked


def cutover_survives_host_loss(root, work, engine, compiler, state, launch_store, register, deploy,
                               running_host, outcome, ready, welcome_n_place):
    with running_host():
        _, registered = outcome(register, 'T1:register')
        old = ready()
        _, deployed = outcome(deploy, 'T1:publish-shared-release')
        new = ready()
        assert registered['assembly'] != deployed['assembly']
        assert deployed['activation'] == 2, deployed

    executable = launch_store / 'place'
    images = state / 'objects' / 'images'
    shutil.copy2(images / old['digest'], executable)
    hosted = 1048576
    bare = 524288

    def blob(raw):
        return struct.pack('<I', len(raw)) + raw

    def native(command, case):
        result = run_case(case, engine, command, cwd=launch_store, capture_output=True)
        assert result.returncode == 0, result.stderr.decode()
        return result.stdout

    def identity(exe, n_place):
        # Describe is two length-prefixed diagnostics, a u64 allowance, then
        # the revision and export list, both already in value-codec form.
        raw = native([str(exe), f'--hirsel-n-place={n_place}', '--hirsel-describe'], 'T1:describe')
        offset = 0
        for _ in range(2):
            size, = struct.unpack_from('<I', raw, offset)
            offset += 4 + size
        allowance, revision_bytes = struct.unpack_from('<QI', raw, offset)
        assert allowance == n_place and revision_bytes == 32
        return hashlib.sha256(b'\x12hirsel.identity.v1' + raw[offset + 8:offset + 44]
                              + raw[offset:offset + 8] + raw[offset + 44:]).digest()

    hosted_id = identity(executable, hosted)
    bare_id = identity(executable, bare)
    assert hosted_id != bare_id and hosted_id == identity(images / new['digest'], hosted)

    # Activations 3 through 9 carry OLD's binding. Bare installs at 10 and 11
    # leave no prepared witness for the failed deploy in the fixture catalog.
    allowance = f'--hirsel-n-place={hosted}'
    for activation in range(3, 10):
        native(['./place', allowance, '--hirsel-install'], f'T1:install-{activation}')
    for activation in (10, 11):
        native(['./place', '--hirsel-install'], f'T1:bare-{activation}')

    platform_inputs = work / 'recovery-catalog.json'
    platform_inputs.write_text(json.dumps(inputs(root, tests=True, package='platform')))
    catalog = work / 'recovery-catalog'
    command = [compiler, 'build', '--inputs', str(platform_inputs), '--source-core',
               '--source-std', '--backend', engine, '--store', str(work / 'publish-cache'),
               '--entry', 'platform@1/platform/hand_install_catalog', '--limit', str(268435456),
               '--dir', str(state / 'catalog'), '-o', str(catalog)]
    for capability in ('Clock', 'Random', 'Tasks', 'Stdin'):
        command += ['--grant', capability]
    checked(command, engine=engine, case='T1:catalog-build')
    fixture = b''.join(map(blob, [bytes.fromhex(registered['assembly']),
                                 bytes.fromhex(deployed['assembly']), hosted_id, bare_id]))
    for mode in ('seed', 'inspect'):
        result = run_case(f'T1:catalog-{mode}', engine, [str(catalog), mode], input=fixture,
                          capture_output=True)
        assert result.returncode == 0, result.stderr.decode()
        if mode == 'seed':
            native(['./place', allowance, '--hirsel-install'], 'T1:hand-install-12')
            with running_host():
                adopted = ready()
                assert adopted['digest'] == old['digest'], adopted
                assert adopted['n_place'] == hosted and welcome_n_place() == hosted


def unpinned(tape, copy):
    # The tape's header block with a fresh tape id, which no catalog has pinned.
    recorded = tape.read_bytes()
    length = int.from_bytes(recorded[12:16], 'little')
    head, payload = recorded[:16], bytearray(recorded[16:16 + length])
    tape_id = os.urandom(16)
    payload[3:19] = tape_id
    xxh3 = ctypes.CDLL(ctypes.util.find_library('xxhash')).XXH3_64bits
    xxh3.argtypes, xxh3.restype = [ctypes.c_void_p, ctypes.c_size_t], ctypes.c_uint64
    checked_bytes = tape_id + head + bytes(payload)
    check = xxh3(checked_bytes, len(checked_bytes))
    copy.write_bytes(head + bytes(payload) + check.to_bytes(8, 'little'))
    return copy


@contextmanager
def bucket_processes(work, engine, compiler, runtime, cc, manifest_path, limit):
    import queue
    import subprocess
    import threading

    executable = work / 'bucket-place'
    native = (['--runtime', str(runtime.resolve())] if runtime else []) + (['--cc', str(cc.resolve())] if cc else [])
    command = [compiler, 'build', '--inputs', str(manifest_path), '--source-core', '--source-std',
               '--backend', engine, *native, '--store', str(work / 'publish-cache'),
               '--entry', 'store_crash@1/runner_crash/bucket_main', '--limit', str(limit),
               '--grant', 'Net=Resolve 127.0.0.1;Listen 127.0.0.0/8 0-65535;Connect 127.0.0.0/8 0-65535',
               '--grant', 'Secret:key=env:HIRSEL_BUCKET_T1_KEY kdf', '-o', str(executable)]
    for capability in ('Clock', 'Random', 'Tasks', 'Process', 'Stderr', 'Stdin', 'Stdout'):
        command += ['--grant', capability]
    checked(command, engine=engine, case='bucket-place:build')
    # The root key lives only in the children's environment; nothing prints it.
    env = dict(os.environ, HIRSEL_BUCKET_T1_KEY=os.urandom(32).hex())
    hosted = '--hirsel-n-place=1048576'

    started = []

    def spawn(args):
        process = subprocess.Popen([str(executable), *args], cwd=work, env=env, stdin=subprocess.PIPE,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, bufsize=1)
        started.append(process)
        lines = queue.Queue()
        threading.Thread(target=lambda: [lines.put(line.rstrip('\n')) for line in process.stdout] + [lines.put(None)],
                         daemon=True).start()
        return process, lines

    def line(lines, case, timeout=120):
        try:
            got = lines.get(timeout=timeout)
        except queue.Empty:
            raise AssertionError(f'{case}: no line within {timeout}s')
        assert got is not None, f'{case}: the process closed its output'
        return got

    def once(args, case):
        result = subprocess.run([str(executable), *args], cwd=work, env=env, capture_output=True, text=True,
                                timeout=120)
        return result.returncode, result.stdout + result.stderr

    def crash(process):
        if process.poll() is None:
            comm = subprocess.check_output(['ps', '-o', 'comm=', '-p', str(process.pid)], text=True).strip()
            assert comm == executable.name[:15], comm
            process.kill()
        return process.wait(timeout=15)

    try:
        yield spawn, line, once, crash, hosted
    finally:
        for process in started:
            crash(process)


@contextmanager
def bucket_relay(port, before=None, after=None):
    """Relay the signed request unchanged, one request per connection."""
    import socket
    from socketserver import BaseRequestHandler, ThreadingTCPServer
    from threading import Thread

    class Connection(BaseRequestHandler):
        def handle(self):
            with self.request.makefile('rb') as wire:
                request_line = wire.readline()
                if not request_line:
                    return
                fields = []
                length = 0
                while (header := wire.readline()) not in (b'\r\n', b''):
                    fields.append(header)
                    if header.lower().startswith(b'content-length:'):
                        length = int(header.split(b':', 1)[1])
                body = wire.read(length)
            method, path, _ = request_line.split()
            if before is not None:
                before(method, path, body)
            with socket.create_connection(('127.0.0.1', port)) as upstream:
                upstream.sendall(request_line + b''.join(fields) + b'\r\n' + body)
                with upstream.makefile('rb') as reply:
                    answer = reply.read()
            if after is not None:
                answer = after(method, path, body, answer)
            self.request.sendall(answer)

    with ThreadingTCPServer(('127.0.0.1', 0), Connection) as server:
        worker = Thread(target=server.serve_forever, daemon=True)
        worker.start()
        try:
            yield f'--bucket=http://127.0.0.1:{server.server_address[1]}'
        finally:
            server.shutdown()
            worker.join()


def bucket_adopts_and_fences(work, engine, compiler, runtime, cc, manifest_path, limit):
    """Native T1 on a Bucket backing: two processes of one executable share the
    loopback simulator bucket (STORE-SPEC v2.65 §12.2). A loopback relay to that
    bucket which drops the proof does not qualify. The bare holder serves
    activation 11; the successor installs 12 under the hosted allowance; the
    holder's next commit is fenced; the relaunched place adopts 12 and commits."""
    stripped = []

    def strip_proof(method, path, body, answer):
        # Keep the initial HEAD response authenticated so the open reaches
        # the separate provider-qualification probe.
        if method != b'GET' or not path.endswith(b'?location='):
            return answer
        head, _, data = answer.partition(b'\r\n\r\n')
        fields = head.split(b'\r\n')
        kept = [field for field in fields if not field.lower().startswith(b'x-hirsel-sim-proof:')]
        stripped.append(len(fields) - len(kept))
        return b'\r\n'.join(kept) + b'\r\n\r\n' + data

    with bucket_processes(work, engine, compiler, runtime, cc, manifest_path, limit) as fixture:
        spawn, line, once, _, hosted = fixture
        sim, sim_lines = spawn(['--sim'])
        port = line(sim_lines, 'T1-bucket:sim')
        assert port.startswith('PORT '), port
        bucket = f'--bucket=http://127.0.0.1:{port[5:]}'
        with bucket_relay(int(port[5:]), after=strip_proof) as unproven:
            code, text = once([hosted, '--hirsel-install', unproven], 'T1-bucket:unproven')
        assert sum(stripped) and code != 0 and 'sim:proof' in text, f'an unproven answer qualified: {code} {text}'

        for activation in range(1, 10):
            code, text = once([hosted, '--hirsel-install', bucket], f'T1-bucket:install-{activation}')
            assert code == 0, f'install {activation}: {text}'
        for activation in (10, 11):
            code, text = once(['--hirsel-install', bucket], f'T1-bucket:bare-{activation}')
            assert code == 0, f'bare install {activation}: {text}'

        holder, held = spawn(['--hold', bucket])
        ready = line(held, 'T1-bucket:holder')
        assert ready.startswith('READY 11 '), ready
        bare_n_place = int(ready.split()[2])

        code, text = once([hosted, '--hirsel-install', bucket], 'T1-bucket:successor-install-12')
        assert code == 0, f'successor install: {text}'
        holder.stdin.write('fence\n')
        holder.stdin.flush()
        ended = line(held, 'T1-bucket:fence')
        assert ended.startswith('ENDED ') and 'another writer' in ended, ended
        assert holder.wait(timeout=60) == 0, holder.stderr.read()

        successor, serving = spawn([hosted, '--hold', bucket])
        ready = line(serving, 'T1-bucket:adopt')
        assert ready == 'READY 12 1048576' and bare_n_place != 1048576, ready
        successor.stdin.write('stop\n')
        successor.stdin.flush()
        assert line(serving, 'T1-bucket:adopt-bump') == 'BUMPED'
        assert line(serving, 'T1-bucket:stop') == 'STOPPED'
        assert successor.wait(timeout=60) == 0, successor.stderr.read()


def bucket_release_barrier(work, engine, compiler, runtime, cc, manifest_path, limit):
    """L56 Runner half through the real Bucket backing and the native wire."""
    import queue
    import threading

    held = queue.Queue()
    release = threading.Event()
    uploaded = set()

    def remember_entry(method, path, data, answer):
        if method == b'PUT' and path.startswith(b'/bucket-t1/e-') and answer.startswith(b'HTTP/1.1 200 '):
            # The effect is the only commit at version 7. A one-commit group
            # starts immediately after the 72-byte HEADER and 40-byte LINK.
            if len(data) >= 140 and struct.unpack_from('<H', data, 112)[0] == 0x0011:
                branch, version = struct.unpack_from('<IQ', data, 128)
                if branch == 0 and version == 7:
                    uploaded.add(path)
        return answer

    def hold_head(method, path, data):
        if method != b'PUT' or path != b'/bucket-t1/head' or len(data) < 152:
            return
        seq, epoch, _, digest = struct.unpack_from('<QQQQ', data, 120)
        if f'/bucket-t1/e-{seq:020d}-{epoch:020d}-{digest:016x}'.encode() in uploaded:
            held.put('EFFECT 7')
            release.wait()

    with bucket_processes(work, engine, compiler, runtime, cc, manifest_path, limit) as fixture:
        spawn, line, once, crash, hosted = fixture
        _, listening = spawn(['--sim'])
        addr = line(listening, 'L56:sim')
        assert addr.startswith('PORT '), addr
        port = int(addr[5:])
        direct = f'--bucket=http://127.0.0.1:{port}'
        # Installs take versions 1 through 5. The next call admits at 6 and
        # commits its reply and one counter increment together at 7.
        for version in range(1, 6):
            code, text = once([hosted, '--hirsel-install', direct], f'L56:install-{version}')
            assert code == 0, text
        with bucket_relay(port, before=hold_head, after=remember_entry) as delayed:
            try:
                holder, output = spawn([hosted, '--hold', '--release-check', delayed])
                ready = line(output, 'L56:ready')
                assert ready == 'READY 5 1048576', ready
                holder.stdin.write('write\n')
                holder.stdin.flush()
                assert line(held, 'L56:entry-complete') == 'EFFECT 7'
                code, text = once(['--follow', direct], 'L56:follower-before')
                assert code == 0 and text.strip() == 'FOLLOW 6 0', text
                assert output.empty(), 'L56: a client received a reply while the HEAD PUT was held'
                release.set()
                published = line(output, 'L56:reply')
                assert published == 'BUMPED 7', published
                assert crash(holder) < 0
            finally:
                release.set()

        code, text = once(['--follow', direct], 'L56:follower-after-crash')
        assert code == 0 and text.strip() == 'FOLLOW 7 1', text
        next_holder, output = spawn([hosted, '--hold', '--release-check', direct])
        ready = line(output, 'L56:successor-ready')
        assert ready == 'READY 5 1048576', ready
        next_holder.stdin.write('write\n')
        next_holder.stdin.flush()
        published = line(output, 'L56:successor-reply')
        assert published == 'BUMPED 9', published
        assert crash(next_holder) < 0


BUCKET_HOST_LAWS = {
    'bucket_adopts_and_fences': bucket_adopts_and_fences,
    'bucket_release_barrier': bucket_release_barrier,
}


def runner_idle_stop(args, work, engines, row, base, native_run, dir_for):
    """P1 laws. Only the child runs here. Host wake rows are covered in P3."""
    for engine in engines:
        if engine == 'evaluator':
            continue
        store = work / f'idle-stop-store-{engine}'
        for law in ('idle_keeps_runs', 'stop_unstarted'):
            definition = f'runner@1/runner_calls/test_{law}'
            code, report = native_run(args, engine, base, store, definition)
            tests = report.get('tests', [])
            row(engine=engine, law=law,
                passed=code == 0 and [test['definition'] for test in tests] == [definition]
                and all(test.get('passed') for test in tests), result=report)
        child = work / f'wake-report-{engine}'
        command = [str(args.compiler.resolve()), 'build', '--inputs', str(base), '--source-core',
                   '--source-std', '--backend', engine, '--runtime', str(args.runtime.resolve()),
                   '--store', str(store), '--entry', 'runner@1/runner_calls/wake_report_main',
                   '--limit', str(64 * 1024 * 1024), '--dir', str(dir_for(store)),
                   '--grant', 'Net=Listen 127.0.0.0/8 0-65535', '-o', str(child)]
        for capability in ('Clock', 'Tasks', 'Random', 'Process', 'Stderr'):
            command += ['--grant', capability]
        if engine == 'llvm':
            command.append('--release')
        if args.cc:
            command += ['--cc', str(args.cc.resolve())]
        result = run_case('wake-report:build', engine, command, capture_output=True, text=True)
        if result.returncode:
            row(engine=engine, law='wake_report', passed=False, result=result.stderr)
            continue
        for mode, exit_code, expected in (
            ('clean', 0, struct.pack('<I', 0)),
            ('due', 0, struct.pack('<IQ', 1, 123456)),
            ('forced', 1, b''),
            ('unstarted', 0, b''),
            ('error', 1, b''),
        ):
            launch = [str(child), '--hirsel-wake', '--hirsel-idle=30', mode]
            with tempfile.TemporaryFile() as report, tempfile.TemporaryFile() as errors:
                pid = os.posix_spawn(str(child), launch, os.environ, file_actions=[
                    (os.POSIX_SPAWN_DUP2, report.fileno(), 5),
                    (os.POSIX_SPAWN_DUP2, errors.fileno(), 2),
                ])
                _, status = os.waitpid(pid, 0)
                report.seek(0)
                output = report.read()
                errors.seek(0)
                diagnostic = errors.read().decode(errors='replace')
            actual = os.waitstatus_to_exitcode(status)
            line = ('place: forced at the drain deadline, 2 sends abandoned\n' if mode == 'forced'
                    else 'place: missing or invalid test\n' if mode == 'error' else '')
            row(engine=engine, law=f'wake_report_{mode}',
                passed=(actual, output, diagnostic) == (exit_code, expected, line),
                result={'status': actual, 'bytes': output.hex(), 'stderr': diagnostic})
