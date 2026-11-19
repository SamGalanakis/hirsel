#!/usr/bin/env python3
"""W3c laws over real sockets, with independent codec and store-version oracles."""
import argparse
import json
import os
from pathlib import Path
import socket
import struct
import subprocess
from tempfile import TemporaryDirectory
import time
import traceback

from wire_process import stop_process
from wire_wsock import REQUEST, connect, frame, head, read_frame, wait_ready


def u32(n):
    return struct.pack('<I', n)


def u64(n):
    return struct.pack('<Q', n)


def blob(b):
    return u32(len(b)) + b


def fingerprint(*opcodes):
    # Schema v3, no named definitions; CRC-64-AVRO, SEM-10.
    value = 0xc15d213aa4d7a795
    for byte in b'\x03\0\0\0\0' + bytes(opcodes):
        value ^= byte
        for _ in range(8):
            value = (value >> 1) ^ (0xc15d213aa4d7a795 if value & 1 else 0)
    return value


# PROG-9 encodes the entire function signature, including the transaction's
# Caller parameter. RUNNER-SPEC rule 3 puts those identity bytes in the key.
ENTRIES = {
    'put': (b'\x01wire@1/admission/put(mut store@1/store/Tx,wire@1/admission/H,store@1/store/Caller,take u32)->core@1/core/Result[u64,u32]', fingerprint(5), fingerprint(17, 6, 5)),
    'large': (b'\x01wire@1/admission/large(mut store@1/store/Tx,wire@1/admission/H,store@1/store/Caller,take u32)->core@1/core/Result[core@1/core/List[u8],u32]', fingerprint(5), fingerprint(17, 14, 3, 5)),
    'big': (b'\x01wire@1/admission/big(take u32)->core@1/core/Result[core@1/core/List[u8],core@1/core/Capacity]', fingerprint(5), fingerprint(14, 3)),
    'read': (b'\x01wire@1/admission/read(store@1/store/Tx,wire@1/admission/H,take u32)->core@1/core/Result[store@1/runner/Cover,core@1/core/Capacity]', fingerprint(5), fingerprint(0)),
    'gone': (b'retired', 0, 0),
    'unknown': (b'unknown', 0, 0),
}
TOKEN = b'anon:' + bytes(16)
TRACE = []


def hello(names=(), token=TOKEN):
    option = u32(0) if token is None else u32(1) + blob(token)
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
            out = ('welcome', self.blob(), self.word(), self.word(8), self.blob(), self.blob(), self.word(8))
        elif tag == 5:
            entry = self.word()
            out = ('placed', entry) if entry == 2 else ('placed', entry, self.word())
            if entry == 1:
                out += (self.blob(),)
        elif tag == 6:
            version = self.word(8)
            complete = [self.word() for _ in range(self.word())]
            rows = []
            for _ in range(self.word()):
                sub, key = self.word(), self.blob()
                present = self.word()
                assert present in (0, 1)
                rows.append((sub, key, self.blob() if present else None))
            done = [self.done() for _ in range(self.word())]
            out = ('batch', version, complete, rows, done)
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


def welcomed(names=(), token=TOKEN):
    s = upgraded(frame(2, hello(names, token)))
    welcome = receive(s)
    assert welcome[:1] == ('welcome',) and welcome[2:4] == (4, 1048576), welcome
    assert len(welcome[1]) == 21 and welcome[1].startswith(b'anon:')
    for slot, name in enumerate(names):
        assert receive(s) == (('placed', 2) if name in ('gone', 'unknown') else ('placed', 0, slot))
    return s, welcome[1]


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
        assert receive(s) == ('batch', 2, [3], [], [])


def session_limit_503():
    sockets = []
    try:
        for _ in range(32):
            sockets.append(upgraded())
        with connect(18094) as s:
            s.sendall(REQUEST)
            assert head(s).startswith(b'HTTP/1.1 503 ')
    finally:
        for s in sockets:
            s.close()


def principal_limit_1013():
    first = None
    for _ in range(10):
        s, principal = welcomed((), None)
        first = first or principal
        s.close()
    with upgraded(frame(2, hello((), None))) as s:
        assert receive(s)[:2] == ('close', 1013)
    s, _ = welcomed((), first)
    s.close()
    with upgraded(frame(2, hello((), b'bad'))) as s:
        assert receive(s)[:2] == ('close', 1013)


def call_rate_overloaded():
    # Serialized round trips keep work units available. The principal is shared
    # across both sockets; neither connection gets its own burst.
    a, token = welcomed(('big',))
    b, _ = welcomed(('big',), token)
    refused = False
    with a, b:
        for stream in range(80):
            s = a if stream % 2 == 0 else b
            s.sendall(frame(2, call(stream, 0, arg=u32(0))))
            answer = receive(s)
            assert answer[0] == 'batch' and len(answer[4]) == 1, answer
            outcome = answer[4][0][2]
            if outcome == ('refused', 2):
                refused = True
                break
            assert outcome == ('returned', u32(0)), answer
    assert refused, '80 serialized calls on one principal exceeded the shared burst/rate'


def request_mismatch_wire():
    s, _ = welcomed(('put', 'read'))
    with s:
        s.sendall(frame(2, call(0, 0, arg=u32(7))))
        done(s, (0, 0, ('returned', u32(0) + u64(4))), 4)
        s.sendall(frame(2, call(0, 0, arg=u32(8))))
        done(s, (0, 0, ('refused', 6)), 4)
        s.sendall(frame(2, u32(2) + u32(1) + u32(1) + blob(u32(0)) + u32(0)))
        answer = receive(s)
        assert answer[:3] == ('batch', 4, [1]) and len(answer[3]) == 1 and answer[3][0][2] == u32(7) and answer[4] == [], answer


def resend_same_p():
    def snapshot():
        reader, _ = welcomed(('read',))
        with reader:
            reader.sendall(frame(2, u32(2) + u32(1) + u32(0) + blob(u32(0)) + u32(0)))
            value = receive(reader)
            assert value[0] == 'batch' and value[2] == [1] and value[4] == [], value
            return value[1], [row[2] for row in value[3]]

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
        assert receive(s) == ('batch', 4, [1], [], [])


def welcome_with_small_budget():
    with upgraded(frame(2, hello(('put',)))) as s:
        welcome = receive(s)
        assert welcome[:4] == ('welcome', TOKEN, 4, 65536), welcome
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



CASES = {f.__name__: f for f in (hello_order, slot_out_of_range, gone_slot_v5, second_hello_closes,
         session_limit_503, principal_limit_1013, call_rate_overloaded, request_mismatch_wire, resend_same_p, held_request_mismatch, hello_deadline, resume_unknown,
         history_below_latest, reply_over_done_max, faulted_owner_releases_slot, welcome_with_small_budget)}


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--server', required=True, type=Path)
    p.add_argument('--case', action='append', choices=CASES)
    p.add_argument('--report', type=Path)
    args = p.parse_args()
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
                stop_process(process)
                refused = subprocess.Popen([str(args.server.resolve()), 'wire_control_budget_too_small'])
                try:
                    assert refused.wait(timeout=8) == 0, 'mandatory control budget was not refused at serve'
                finally:
                    stop_process(refused)
            rows.append({'law': name, 'passed': True})
            print(f'{name}: PASS', flush=True)
        except Exception as error:
            rows.append({'law': name, 'passed': False, 'error': str(error)})
            print(f'{name}: FAIL {error}; received {repr(TRACE)[-3000:]}', flush=True)
            traceback.print_exc()
        finally:
            stop_process(process)
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
