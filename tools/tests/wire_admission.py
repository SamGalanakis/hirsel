#!/usr/bin/env python3
"""W3c laws over real sockets, with independent codec and store-version oracles."""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import socket
import struct
import subprocess
from tempfile import TemporaryDirectory
import time
import traceback

from wire_process import accept_from, stop_listener, stop_process
from wire_wsock import GUID, REQUEST, connect, frame, head, read_frame, wait_ready


def u32(n):
    return struct.pack('<I', n)


def u64(n):
    return struct.pack('<Q', n)


def blob(b):
    return u32(len(b)) + b


def fingerprint(*opcodes):
    # Schema v3, no named definitions; CRC-64-AVRO, SEM-10.
    return crc64(b'\x03\0\0\0\0' + bytes(opcodes))


def crc64(data):
    value = 0xc15d213aa4d7a795
    for byte in data:
        value ^= byte
        for _ in range(8):
            value = (value >> 1) ^ (0xc15d213aa4d7a795 if value & 1 else 0)
    return value


# The query result schema contains the unit value and one named table slot.
READ_RESULT_FP = crc64(
    blob(b'\x03\0\0\0\0\x00') + u32(1) + blob(b'data') + u32(0)
    + blob(b'\x04\0\0\0\0\x06') + blob(b'\x03\0\0\0\0\x05')
)

# PROG-9 encodes the entire function signature, including the transaction's
# Caller parameter. RUNNER-SPEC rule 3 puts those identity bytes in the key.
ENTRIES = {
    'put': (b'\x01wire@1/admission/put(mut store@1/store/Tx,wire@1/admission/H,store@1/store/Caller,take u32)->core@1/core/Result[u64,u32]', fingerprint(5), fingerprint(17, 6, 5)),
    'large': (b'\x01wire@1/admission/large(mut store@1/store/Tx,wire@1/admission/H,store@1/store/Caller,take u32)->core@1/core/Result[core@1/core/List[u8],u32]', fingerprint(5), fingerprint(17, 14, 3, 5)),
    'big': (b'\x01wire@1/admission/big(take u32)->core@1/core/Result[core@1/core/List[u8],core@1/core/Capacity]', fingerprint(5), fingerprint(14, 3)),
    'read': (b'\x01wire@1/admission/read(store@1/store/Tx,wire@1/admission/H,take u32)->core@1/core/Result[runner@1/runner/Answer[core@1/core/()],core@1/core/Capacity]', fingerprint(5), READ_RESULT_FP),
    'gone': (b'retired', 0, 0),
    'unknown': (b'unknown', 0, 0),
}
CREDENTIAL = bytes(16)
TRACE = []
# The compiler, runtime and linker for the hirsel call laws, set by main.
CLI = {}


def hello(names=(), credential=CREDENTIAL):
    option = u32(0) if credential is None else u32(1) + blob(credential)
    wants = b''.join(blob(ENTRIES[n][0]) + u64(ENTRIES[n][1]) + u64(ENTRIES[n][2]) for n in names)
    return u32(0) + option + u32(len(names)) + wants


def call(stream, p, slot=0, arg=b'', deadline=10000):
    return u32(1) + u32(stream) + u64(p) + u32(slot) + u32(deadline) + blob(arg)


class Decode:
    def __init__(self, b):
        self.b, self.at = b, 0

    def word(self, n=4):
        assert self.at + n <= len(self.b)
        value = int.from_bytes(self.b[self.at:self.at+n], 'little')
        self.at += n
        return value

    def blob(self):
        n = self.word()
        assert self.at + n <= len(self.b)
        value = self.b[self.at:self.at+n]
        self.at += n
        return value

    def outcome(self):
        tag = self.word()
        if tag == 0:
            return ('returned', self.blob())
        if tag == 1:
            fault = self.word()
            if fault == 0:
                return ('raised', self.word())
            if fault == 1:
                return ('out_of_bytes', self.word(8), self.word(8))
            if fault == 2:
                return ('out_of_time', self.word(8))
            if fault == 4:
                return ('conflict', self.blob().decode())
            assert fault == 3
            return ('gone',)
        if tag == 2:
            why = self.word()
            if why == 1:
                return ('refused', why, self.word(), self.word(8))
            if why == 4:
                return ('refused', why, self.word(8), self.word(8))
            return ('refused', why)
        assert tag in (3, 4)
        return ('abandoned' if tag == 3 else 'in_doubt',)

    def done(self):
        return (self.word(), self.word(8), self.outcome())

    def read(self):
        tag = self.word()
        if tag == 4:
            out = ('welcome', (self.word(1), self.blob()), self.word(), self.word(8), self.blob(), self.blob(), self.word(8))
        elif tag == 5:
            entry = self.word()
            out = ('placed', entry) if entry == 2 else ('placed', entry, self.word())
            if entry == 1:
                out += (self.blob(),)
        elif tag == 6:
            version = self.word(8)
            complete = [self.word() for _ in range(self.word())]
            groups = []
            for _ in range(self.word()):
                sub, value = self.word(), self.blob()
                rows = []
                for _ in range(self.word()):
                    slot, key = self.word(2), self.blob()
                    present = self.word()
                    assert present in (0, 1)
                    rows.append((slot, key, self.blob() if present else None))
                groups.append((sub, value, rows))
            done = [self.done() for _ in range(self.word())]
            out = ('batch', version, complete, groups, done)
        elif tag == 7:
            out = ('ended', self.word(), self.outcome())
        else:
            assert tag == 8, tag
            out = ('reset', self.word(8), [self.done() for _ in range(self.word())])
        assert self.at == len(self.b), (out, self.at, len(self.b))
        return out


def receive(s):
    op, data = read_frame(s)
    if op == 8:
        return ('close', int.from_bytes(data[:2], 'big'), data[2:])
    assert op == 2, op
    value = Decode(data).read()
    TRACE.append(value)
    if len(TRACE) > 8:
        TRACE.pop(0)
    return value


def upgraded(tail=b''):
    s = connect(18094)
    s.sendall(REQUEST + tail)
    response = head(s)
    assert response.startswith(b'HTTP/1.1 101 '), response
    return s


def welcomed(names=(), credential=CREDENTIAL):
    s = upgraded(frame(2, hello(names, credential)))
    welcome = receive(s)
    assert welcome[:1] == ('welcome',) and welcome[2:4] == (4, 1048576), welcome
    assert welcome[1][0] == 2 and len(welcome[1][1]) == 16
    for slot, name in enumerate(names):
        assert receive(s) == (('placed', 2) if name in ('gone', 'unknown') else ('placed', 0, slot))
    return s, credential


def done(s, expected, version):
    answer = receive(s)
    assert answer == ('batch', version, [], [], [expected]), answer


def hello_order():
    names = ('put', 'gone', 'big')
    tail = frame(2, hello(names)) + frame(2, call(0, 0, arg=u32(7)))
    with upgraded(tail) as s:
        assert receive(s)[:1] == ('welcome',)
        assert receive(s) == ('placed', 0, 0)
        assert receive(s) == ('placed', 2)
        assert receive(s) == ('placed', 0, 2)
        done(s, (0, 0, ('returned', u32(0) + u64(4))), 4)
    with upgraded(frame(2, call(0, 0))) as s:
        assert receive(s)[:2] == ('close', 1008)


def slot_out_of_range():
    s, _ = welcomed(('put', 'unknown'))
    with s:
        for slot in (2, 0xffffffff, 1):
            s.sendall(frame(2, call(0, 0, slot, u32(7))))
            done(s, (0, 0, ('refused', 0)), 0)
        s.sendall(frame(2, call(0, 0, 0, u32(7))))
        done(s, (0, 0, ('returned', u32(0) + u64(4))), 4)


def gone_slot_v5():
    s, _ = welcomed(('gone',))
    with s:
        for p, version in ((0, 3), (0, 3), (1, 5), (1, 5)):
            s.sendall(frame(2, call(77, p)))
            done(s, (77, p, ('gone',)), version)


def second_hello_closes():
    s, _ = welcomed(('put',))
    with s:
        s.sendall(frame(2, hello(('big',), None)))
        assert receive(s)[:2] == ('close', 1008)


def held_request_mismatch():
    s, _ = welcomed(('put',))
    with s:
        s.sendall(frame(2, call(0, 0, arg=u32(7))) + frame(2, call(0, 0, arg=u32(8))))
        assert receive(s)[:2] == ('close', 1008), TRACE


def hello_deadline():
    with upgraded() as s:
        s.settimeout(12)
        assert receive(s)[:2] == ('close', 1008)
    s, _ = welcomed(())
    s.close()


def resume_unknown():
    s, _ = welcomed(('read',))
    with s:
        for sub in (1, 2):
            s.sendall(frame(2, u32(2) + u32(sub) + u32(0) + blob(u32(0)) + u32(1) + blob(b'unknown')))
            assert receive(s) == ('reset', 0, [])
        s.sendall(frame(2, u32(2) + u32(3) + u32(0) + blob(u32(0)) + u32(0)))
        assert receive(s) == ('batch', 2, [3], [(3, b'', [])], [])


def request_mismatch_wire():
    s, _ = welcomed(('put', 'read'))
    with s:
        s.sendall(frame(2, call(0, 0, arg=u32(7))))
        done(s, (0, 0, ('returned', u32(0) + u64(4))), 4)
        s.sendall(frame(2, call(0, 0, arg=u32(8))))
        done(s, (0, 0, ('refused', 6)), 4)
        s.sendall(frame(2, u32(2) + u32(1) + u32(1) + blob(u32(0)) + u32(0)))
        answer = receive(s)
        assert answer == ('batch', 4, [1], [(1, b'', [(0, bytes(8), u32(7))])], []), answer


def resend_same_p():
    def snapshot():
        reader, _ = welcomed(('read',))
        with reader:
            reader.sendall(frame(2, u32(2) + u32(1) + u32(0) + blob(u32(0)) + u32(0)))
            value = receive(reader)
            assert value[0] == 'batch' and value[2] == [1] and value[4] == [], value
            return value[1], [row[2] for sub, _, rows in value[3] if sub == 1 for row in rows]

    peer, principal = welcomed(('put',))
    request_bytes = frame(2, call(0, 0, arg=u32(7)))
    before = snapshot()
    assert before[1] == [], before
    with peer:
        peer.sendall(request_bytes[:-1])
        peer.shutdown(socket.SHUT_WR)
        # Reader stops without obtaining a complete application message.
        assert receive(peer)[:2] == ('close', 1001)
    assert snapshot() == before

    peer, _ = welcomed(('put',), principal)
    with peer:
        peer.sendall(request_bytes)
        assert peer.recv(1) == b'', 'the write cut did not occur before the outcome reached the client'
    committed = snapshot()
    assert committed == (before[0] + 2, [u32(7)]), (before, committed)

    peer, _ = welcomed(('put',), principal)
    with peer:
        peer.sendall(request_bytes)
        done(peer, (0, 0, ('returned', u32(0) + u64(committed[0]))), committed[0])
        peer.sendall(frame(2, call(0, 0, arg=u32(8))))
        done(peer, (0, 0, ('refused', 6)), committed[0])
    assert snapshot() == committed


def history_below_latest():
    s, _ = welcomed(('put',))
    with s:
        for p, version in ((0, 4), (1, 6)):
            s.sendall(frame(2, call(0, p, arg=u32(7))))
            done(s, (0, p, ('returned', u32(0) + u64(version))), version)
        s.sendall(frame(2, call(0, 0, arg=u32(9))))
        done(s, (0, 0, ('refused', 7)), 6)


def reply_over_done_max():
    s, _ = welcomed(('large', 'big', 'read'))
    with s:
        s.sendall(frame(2, call(1, 0, 0, u32(8165))))
        done(s, (1, 0, ('out_of_bytes', 8173, 8172)), 4)
        s.sendall(frame(2, call(2, 0, 1, u32(8169))))
        done(s, (2, 0, ('out_of_bytes', 8173, 8172)), 4)
        s.sendall(frame(2, u32(2) + u32(1) + u32(2) + blob(u32(0)) + u32(0)))
        assert receive(s) == ('batch', 4, [1], [(1, b'', [])], [])


def welcome_with_small_budget():
    with upgraded(frame(2, hello(('put',)))) as s:
        welcome = receive(s)
        assert welcome[:4] == ('welcome', (2, hashlib.sha256(b'hirsel.example.credential.v1' + CREDENTIAL).digest()[:16]), 4, 65536), welcome
        assert receive(s) == ('placed', 0, 0)
        ping = bytes(range(125))
        s.sendall(frame(9, ping))
        assert read_frame(s) == (10, ping)
        s.sendall(frame(8, b'\x03\xe8'))
        assert read_frame(s) == (8, b'\x03\xe8')


def faulted_owner_releases_slot():
    def next_owner():
        for _ in range(100):
            s = connect(18094)
            try:
                s.sendall(REQUEST)
                response = head(s)
                if response.startswith(b'HTTP/1.1 101 '):
                    return s
                assert response.startswith(b'HTTP/1.1 503 '), response
            except BaseException:
                s.close()
                raise
            s.close()
            time.sleep(.02)
        raise AssertionError('the listener never released its single owner slot')

    with next_owner() as s:
        s.sendall(frame(2, u32(3) + u32(2)))
        assert receive(s) == ('reset', 0, [])
        s.sendall(frame(2, u32(3) + u32(1)))
        assert s.recv(1) == b''
    with next_owner() as s:
        s.sendall(frame(2, u32(3) + u32(2)))
        assert receive(s) == ('reset', 0, [])



def hirsel_call(url, *call):
    # Describe the admission fixture at the same n_place it serves.
    command = [CLI['compiler'], 'call', CLI['server'], '--url', url, '--runtime', CLI['runtime'],
               '--n-place', '1048576']
    command += ['--cc', CLI['cc']] if CLI.get('cc') else []
    return subprocess.Popen(command + ['--', *call], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)


def cli_call_exact_value():
    # Bug: a result decoder that goes through f64, or loses a tag or option level.
    # Oracle by hand from admission.hirsel: shape(n) returns Shape(sum: Large(n,
    # Some(n + 2)), nested: Some(None), total: n + 2). For n = 2^53 + 1 an f64
    # decoder prints 9007199254740992 and 9007199254740996 instead.
    n = 2**53 + 1
    process = hirsel_call('ws://127.0.0.1:18094/hirsel', 'shape', '--arg', str(n))
    try:
        out, err = process.communicate(timeout=120)
    finally:
        stop_process(process)
    assert process.returncode == 0, err
    assert json.loads(out) == {'tag': 'Returned', 'value': {
        'sum': {'tag': 'Large', 'values': [n, {'tag': 'Some', 'value': n + 2}]},
        'nested': {'tag': 'Some', 'value': {'tag': 'None'}}, 'total': n + 2}}, out


def cli_call_fingerprint_mismatch():
    # Bug: hirsel call sending its Call after the server declined the entry. The
    # runner answers Placed(Gone) to a want whose identity and fingerprints it does
    # not serve (slot_out_of_range's 'unknown'); this loopback stand-in answers
    # exactly that to the CLI's Hello and records every frame the CLI sends after.
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', 0))
        listener.listen(1)
        listener.settimeout(120)
        process = hirsel_call(f'ws://127.0.0.1:{listener.getsockname()[1]}/hirsel', 'shape', '--arg', '1')
        try:
            s = accept_from(listener, process.pid, descendants=True)
            with s:
                s.settimeout(30)
                key = next(line.split(b':', 1)[1].strip() for line in head(s).split(b'\r\n')
                           if line.lower().startswith(b'sec-websocket-key:'))
                accept = base64.b64encode(hashlib.sha1(key + GUID).digest())
                s.sendall(b'HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n'
                          b'Sec-WebSocket-Accept: ' + accept + b'\r\nSec-WebSocket-Protocol: hirsel.3\r\n\r\n')
                op, hello_bytes = read_frame(s, masked=True)
                # Hello with no credential, exactly one want.
                assert op == 2 and Decode(hello_bytes).word() == 0, hello_bytes
                assert hello_bytes[4:12] == u32(0) + u32(1), hello_bytes
                welcome = u32(4) + bytes([1]) + blob(bytes(16)) + u32(4) + u64(1048576) + blob(b'') + blob(b'') + u64(0)
                s.sendall(frame(2, welcome, masked=False) + frame(2, u32(5) + u32(2), masked=False))
                after = []
                while not after or after[-1][0] != 8:
                    try:
                        after.append(read_frame(s, masked=True))
                    except AssertionError:
                        break
            out, err = process.communicate(timeout=60)
        finally:
            if process.poll() is None:
                stop_process(process)
    assert process.returncode == 2 and 'Placed Gone; the call was not sent' in err, (process.returncode, err)
    assert all(op != 2 for op, _ in after), after


CASES = {f.__name__: f for f in (hello_order, slot_out_of_range, gone_slot_v5, second_hello_closes,
         request_mismatch_wire, resend_same_p, held_request_mismatch, hello_deadline, resume_unknown,
         history_below_latest, reply_over_done_max, faulted_owner_releases_slot, welcome_with_small_budget,
         cli_call_exact_value, cli_call_fingerprint_mismatch)}


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--server', required=True, type=Path)
    p.add_argument('--case', action='append', choices=CASES)
    p.add_argument('--report', type=Path)
    p.add_argument('--compiler', type=Path, help='hirsel, for the hirsel call laws')
    p.add_argument('--runtime', type=Path)
    p.add_argument('--cc', type=Path)
    args = p.parse_args()
    CLI.update({k: str(v.resolve()) for k, v in (('compiler', args.compiler), ('runtime', args.runtime), ('cc', args.cc), ('server', args.server)) if v})
    rows = []
    selected = args.case or CASES
    for name in selected:
        temporary = TemporaryDirectory(prefix='wire-cut-') if name == 'resend_same_p' else None
        environment = None
        if temporary:
            shim = Path(temporary.name) / 'socket.so'
            fixture = Path(__file__).resolve().parents[2] / 'core/tools/std_socket.c'
            subprocess.run(['cc', '-shared', '-fPIC', '-DWIRE_CUT_OUTCOME', str(fixture), '-ldl', '-o', str(shim)], check=True)
            environment = os.environ | {'LD_PRELOAD': str(shim)}
        process = subprocess.Popen([str(args.server.resolve()), name], stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, env=environment)
        try:
            wait_ready(process, 18094)
            TRACE.clear()
            CASES[name]()
            if name == 'welcome_with_small_budget':
                stop_listener(process, 18094)
                refused = subprocess.Popen([str(args.server.resolve()), 'wire_control_budget_too_small'])
                try:
                    assert refused.wait(timeout=8) == 0, 'mandatory control budget was not refused at serve'
                finally:
                    stop_listener(refused, 18094)
            rows.append({'law': name, 'passed': True})
            print(f'{name}: PASS', flush=True)
        except Exception as error:
            rows.append({'law': name, 'passed': False, 'error': str(error)})
            print(f'{name}: FAIL {error}; received {repr(TRACE)[-3000:]}', flush=True)
            traceback.print_exc()
        finally:
            stop_listener(process, 18094)
            if temporary:
                temporary.cleanup()
            stderr = process.stderr.read().decode(errors='replace')
            if stderr:
                print(stderr[-3000:], flush=True)
    if args.report:
        args.report.write_text(json.dumps(rows, indent=2) + '\n')
    return int(any(not row['passed'] for row in rows))


if __name__ == '__main__':
    raise SystemExit(main())
