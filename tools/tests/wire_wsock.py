#!/usr/bin/env python3
"""Real-loopback qualification for the wsock protocol echo and client endpoints."""
import argparse
import base64
import hashlib
import os
from pathlib import Path
import socket
import struct
import subprocess
import tempfile
import time

from wire_process import accept_from, stop_listener, stop_process
from concurrent.futures import ThreadPoolExecutor

ROOT = Path(__file__).resolve().parents[2]
KEY = b'dGhlIHNhbXBsZSBub25jZQ=='
GUID = b'258EAFA5-E914-47DA-95CA-C5AB0DC85B11'
REQUEST = (b'GET /hirsel HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\n'
           b'Connection: Upgrade\r\nSec-WebSocket-Version: 13\r\n'
           b'Sec-WebSocket-Key: ' + KEY + b'\r\nSec-WebSocket-Protocol: hirsel.4\r\n\r\n')


def exact(s, count):
    result = b''
    while len(result) != count:
        part = s.recv(count - len(result))
        if not part:
            raise AssertionError(f'EOF with {count-len(result)} bytes missing')
        result += part
    return result


def head(s):
    # Read exactly the HTTP head, leaving read-ahead frame bytes in the socket.
    result = b''
    while not result.endswith(b'\r\n\r\n'):
        result += exact(s, 1)
        assert len(result) <= 8192
    return result


def frame(opcode, payload=b'', masked=True, fin=True):
    count = len(payload)
    size = (bytes([count]) if count < 126 else b'\x7e' + struct.pack('!H', count)
            if count < 65536 else b'\x7f' + struct.pack('!Q', count))
    # An all-zero mask is legal RFC input and keeps the hand oracle direct.
    return bytes([(128 if fin else 0) | opcode, size[0] | (128 if masked else 0)]) + size[1:] + (b'\0'*4 if masked else b'') + payload


def read_frame(s, masked=False):
    opcode, size = exact(s, 2)
    assert opcode & 0x70 == 0 and opcode & 0x80
    assert bool(size & 128) == masked
    count = size & 127
    if count == 126:
        count = struct.unpack('!H', exact(s, 2))[0]
    elif count == 127:
        count = struct.unpack('!Q', exact(s, 8))[0]
    mask = exact(s, 4) if masked else b'\0'*4
    payload = exact(s, count)
    return opcode & 15, bytes(x ^ mask[i % 4] for i, x in enumerate(payload))


def connect(port=18089):
    s = socket.socket()
    s.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 4096)
    s.settimeout(8)
    s.connect(('127.0.0.1', port))
    return s


def upgrade(tail=b''):
    s = connect()
    s.sendall(REQUEST + tail)
    response = head(s)
    assert b'HTTP/1.1 101 ' in response
    assert b'Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=' in response
    return s


def wait_ready(process, port=18089):
    for _ in range(100):
        assert process.poll() is None, process.returncode
        try:
            with connect(port):
                return
        except OSError:
            time.sleep(.02)
    raise AssertionError('echo did not listen')


def session_with_hello():
    s = connect(18094)
    try:
        hello = struct.pack('<III', 0, 1, 8) + b'deadline' + struct.pack('<I', 0)
        s.sendall(REQUEST + frame(2, hello))
        assert b'HTTP/1.1 101 ' in head(s)
        assert read_frame(s) == (2, struct.pack('<IQI', 8, 0, 0))
        return s
    except BaseException:
        s.close()
        raise


def incomplete_deadline():
    with session_with_hello() as s:
        s.sendall(frame(2, b'abc', fin=False))
        start = time.monotonic()
        for at in (12, 24):
            time.sleep(max(0, start + at - time.monotonic()))
            s.sendall(frame(9, b'p'))
            assert read_frame(s) == (10, b'p')
        s.settimeout(11)
        assert read_frame(s) == (8, struct.pack('!H', 1001))
        assert time.monotonic() - start >= 29
        print('incomplete_message_deadline: PASS', flush=True)


def max_batch_call():
    # Call and Done both carry StreamId(t, n) as two little-endian u64s.
    # The Batch has a 24-byte prefix and a 32-byte Returned row header.
    arg = b'a' * (262144 - 56)
    call = struct.pack('<IQQQIII', 1, 0, 7, 2, 0, 1000, len(arg)) + arg
    batch = struct.pack('<IQIII', 6, 0, 0, 0, 1) + struct.pack('<QQQII', 0, 7, 2, 0, len(arg)) + arg
    assert len(batch) == 262144
    return call, batch


def blocked_write_deadline():
    with session_with_hello() as s:
        call, _ = max_batch_call()
        s.sendall(frame(2, call))
        time.sleep(37)
        s.settimeout(3)
        got = bytearray()
        while data := s.recv(65536):
            got.extend(data)
        assert bytes(got[:10]) == b'\x82\x7f' + struct.pack('!Q', 262144)
        assert 10 < len(got) < 262154, len(got)
        print('blocked_write_deadline: PASS', flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--echo', type=Path)
    parser.add_argument('--client', type=Path)
    parser.add_argument('--session', type=Path)
    parser.add_argument('--idle-session', type=Path)
    session_mode = parser.add_mutually_exclusive_group()
    session_mode.add_argument('--session-codes-only', action='store_true')
    session_mode.add_argument('--session-deadlines-only', action='store_true')
    parser.add_argument('--cc', default='cc')
    parser.add_argument('--idle-echo', type=Path)
    parser.add_argument('--client-only', action='store_true')
    args = parser.parse_args()
    if args.session:
        with tempfile.TemporaryDirectory(prefix='wire-session-') as tmp:
            shim = Path(tmp) / 'socket.so'
            subprocess.run([args.cc, '-shared', '-fPIC', str(ROOT/'core/tools/std_socket.c'), '-ldl', '-o', str(shim)], check=True)
            with (Path(tmp)/'stderr').open('wb') as errors:
                process = subprocess.Popen([str(args.session.resolve())], env=os.environ | {'LD_PRELOAD': str(shim)}, stderr=errors)
                try:
                    wait_ready(process, 18094)
                    if args.session_deadlines_only:
                        with ThreadPoolExecutor(max_workers=2) as executor:
                            checks = [executor.submit(law) for law in (incomplete_deadline, blocked_write_deadline)]
                            for check in checks:
                                check.result()
                        return 0
                    if not args.session_codes_only:
                        with connect(18094) as stalled:
                            stalled.sendall(b'GET')
                            with connect(18094) as s:
                                s.sendall(REQUEST + frame(2, struct.pack('<II', 3, 7)))
                                assert b'HTTP/1.1 101 ' in head(s)
                                assert read_frame(s) == (2, struct.pack('<IQI', 8, 0, 0))
                                s.sendall(frame(9, b'p'))
                                assert read_frame(s) == (10, b'p')
                                s.sendall(frame(8, b'\x03\xe8'))
                                assert read_frame(s) == (8, b'\x03\xe8')
                                assert s.recv(1) == b''
                        print('session concurrent upgrades, tail, dispatch, Pong, Close: PASS', flush=True)

                        call, batch = max_batch_call()
                        with connect(18094) as s:
                            s.sendall(REQUEST)
                            assert b'HTTP/1.1 101 ' in head(s)
                            s.sendall(frame(2, call) + frame(8, b'\x03\xe8') + b'x' * 65536)
                            s.shutdown(socket.SHUT_WR)
                            time.sleep(.1)
                            assert read_frame(s) == (2, batch)
                            assert read_frame(s) == (8, b'\x03\xe8')
                            assert s.recv(1) == b''
                        print('session Close behind blocked data, unread input drained: PASS', flush=True)

                        with connect(18094) as s:
                            s.sendall(REQUEST)
                            assert b'HTTP/1.1 101 ' in head(s)
                            s.sendall(frame(2, call) + frame(2, struct.pack('<III', 0, 0, 0)) + frame(9, b'late'))
                            time.sleep(.1)
                            assert read_frame(s) == (2, batch)
                            assert read_frame(s) == (8, struct.pack('!H', 1013))
                            s.sendall(frame(8, struct.pack('!H', 1013)))
                            s.shutdown(socket.SHUT_WR)
                            assert s.recv(1) == b''
                        print('session local Close stays ahead of later Ping: PASS', flush=True)

                        for payload, code in [(struct.pack('<I', 8), 1007), (struct.pack('<III', 0, 0, 0), 1013)]:
                            with connect(18094) as s:
                                s.sendall(REQUEST + frame(2, payload))
                                assert b'HTTP/1.1 101 ' in head(s)
                                assert read_frame(s) == (8, struct.pack('!H', code))
                                s.sendall(frame(8, struct.pack('!H', code)))
                                s.shutdown(socket.SHUT_WR)
                                assert s.recv(1) == b''
                            print(f'session Close {code}: PASS', flush=True)
                    with connect(18094) as s:
                        s.sendall(REQUEST + frame(1, b'hello'))
                        assert b'HTTP/1.1 101 ' in head(s)
                        assert read_frame(s) == (8, struct.pack('!H', 1003))
                        s.sendall(frame(8, struct.pack('!H', 1003)))
                        s.shutdown(socket.SHUT_WR)
                        assert s.recv(1) == b''
                    print('session text message Close 1003: PASS', flush=True)
                finally:
                    stop_listener(process, 18094)
            if args.idle_session:
                with (Path(tmp)/'idle-stderr').open('wb') as errors:
                    process = subprocess.Popen([str(args.idle_session.resolve())], env=os.environ | {'LD_PRELOAD': str(shim)}, stderr=errors)
                    try:
                        wait_ready(process, 18094)
                        with connect(18094) as s:
                            s.sendall(REQUEST)
                            assert b'HTTP/1.1 101 ' in head(s)
                            assert read_frame(s) == (9, b'')
                            assert read_frame(s) == (8, struct.pack('!H', 1001))
                            s.sendall(frame(8, struct.pack('!H', 1001)))
                            s.shutdown(socket.SHUT_WR)
                            assert s.recv(1) == b''
                        print('session idle Ping then Close 1001: PASS', flush=True)
                    finally:
                        stop_listener(process, 18094)
        return 0
    if args.echo is None or args.client is None:
        parser.error('--echo and --client are required unless --session is selected')
    with tempfile.TemporaryDirectory(prefix='wire-wsock-') as tmp:
        shim = Path(tmp) / 'socket.so'
        subprocess.run([args.cc, '-shared', '-fPIC', str(ROOT/'core/tools/std_socket.c'), '-ldl', '-o', str(shim)], check=True)
        if not args.client_only:
            with (Path(tmp)/'stderr').open('wb') as errors:
                process = subprocess.Popen([str(args.echo.resolve())], env=os.environ | {'LD_PRELOAD': str(shim)}, stderr=errors)
                try:
                    wait_ready(process)
                    # RFC 6455 hand vector and read-ahead handoff.
                    with upgrade(frame(2, b'abc')) as s:
                        assert read_frame(s) == (2, b'abc')
                        s.sendall(frame(8, b'\x03\xe8'))
                        assert read_frame(s) == (8, b'\x03\xe8')
                        assert s.recv(1) == b''
                    print('upgrade_tail_kept: PASS', flush=True)

                    # Extra raw bytes after Close must drain before final close,
                    # otherwise Linux can reset the peer and lose our Close.
                    with upgrade() as s:
                        s.sendall(frame(8, b'\x03\xe8') + b'x' * 65536)
                        s.shutdown(socket.SHUT_WR)
                        assert read_frame(s) == (8, b'\x03\xe8')
                        assert s.recv(1) == b''
                    print('close_deadline drain: PASS Close retained with unread input', flush=True)

    
                    # With an 8192-byte kernel send buffer a 256 KiB response must
                    # retain a cursor. The Ping arrives while that response is blocked.
                    payload = b'a' * 262144
                    with upgrade() as s:
                        s.sendall(frame(2, payload) + frame(9, b'p'))
                        time.sleep(.1)
                        assert read_frame(s) == (2, payload)
                        assert read_frame(s) == (10, b'p')
                        s.sendall(frame(8, b'\x03\xe8'))
                        assert read_frame(s) == (8, b'\x03\xe8')
                        assert s.recv(1) == b''
                    print('blocked_pong_drains: PASS full kernel buffer, frame before Pong', flush=True)
    
                    # Locally initiated close, with a peer that never answers.
                    with upgrade() as s:
                        before = time.monotonic()
                        s.sendall(frame(3))
                        assert read_frame(s) == (8, b'\x03\xea')
                        assert s.recv(1) == b''
                        # SHUT_WR gives EOF immediately, while finish keeps the
                        # read owner until its absolute deadline. The sequential
                        # listener admits this probe only after full release.
                        with upgrade() as probe:
                            elapsed = time.monotonic() - before
                            assert 4.8 <= elapsed < 7, elapsed
                            probe.sendall(frame(8, b'\x03\xe8'))
                            assert read_frame(probe) == (8, b'\x03\xe8')
                    print(f'close_deadline local: PASS {elapsed:.3f}s', flush=True)
    
                    # Peer Close behind one data message. It is decoded while the
                    # sole writer is blocked, and the peer never resumes reading.
                    with upgrade() as stalled:
                        before = time.monotonic()
                        stalled.sendall(frame(2, payload) + frame(8, b'\x03\xe8'))
                        time.sleep(.2)
                        with upgrade() as probe:
                            elapsed = time.monotonic() - before
                            assert 4.8 <= elapsed < 7, elapsed
                            probe.sendall(frame(8, b'\x03\xe8'))
                            assert read_frame(probe) == (8, b'\x03\xe8')
                    print(f'close_deadline blocked flush: PASS {elapsed:.3f}s', flush=True)
    
                    # Absolute accept-to-head expiry; a fresh upgrade is admitted.
                    with connect() as slow:
                        before = time.monotonic()
                        slow.sendall(b'GET /hirsel HTTP/1.1\r\n')
                        assert slow.recv(1) == b''
                        elapsed = time.monotonic() - before
                        assert 4.8 <= elapsed < 7, elapsed
                    with upgrade() as s:
                        s.sendall(frame(8, b'\x03\xe8'))
                        assert read_frame(s) == (8, b'\x03\xe8')
                    print(f'pending_released loopback: PASS upgrade expired at {elapsed:.3f}s', flush=True)
                finally:
                    stop_process(process)
                errors.flush()
                assert (Path(tmp)/'stderr').read_bytes() == b'', (Path(tmp)/'stderr').read_text()
    
        # Independent Python peer validates the client's own random challenge,
        # required subprotocol, masks on data/Ping/Pong/Close, and frame tail.
        with socket.socket() as listener:
            listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            listener.bind(('127.0.0.1', 18090))
            listener.listen(1)
            listener.settimeout(8)
            client = subprocess.Popen([str(args.client.resolve())], stderr=subprocess.PIPE)
            try:
                peer = accept_from(listener, client.pid)
                with peer:
                    peer.settimeout(8)
                    request = head(peer)
                    key = next(line.split(b':',1)[1].strip() for line in request.split(b'\r\n') if line.lower().startswith(b'sec-websocket-key:'))
                    assert len(base64.b64decode(key, validate=True)) == 16
                    assert b'Sec-WebSocket-Protocol: hirsel.4' in request
                    accept = base64.b64encode(hashlib.sha1(key + GUID).digest())
                    peer.sendall(b'HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Protocol: hirsel.4\r\nSec-WebSocket-Accept: '+accept+b'\r\n\r\n'+frame(2,b'abc',False))
                    for opcode, payload in [(2,b'abc'), (9,b'abc'), (10,b'abc')]:
                        assert read_frame(peer, True) == (opcode, payload)
                        if opcode == 9:
                            peer.sendall(frame(10, b'abc', False))
                    assert read_frame(peer, True) == (8, b'\x03\xe8')
                    peer.sendall(frame(8, b'\x03\xe8', False))
                _, stderr = client.communicate(timeout=8)
                assert client.returncode == 0, stderr
                print('client role: PASS nonce, selected protocol, read-ahead and four masked opcodes', flush=True)
            finally:
                if client.poll() is None:
                    stop_process(client)
    if args.idle_echo:
        process = subprocess.Popen([str(args.idle_echo.resolve())], stderr=subprocess.PIPE)
        try:
            wait_ready(process)
            with upgrade() as s:
                s.sendall(frame(1, b'a', fin=False))
                for byte in b'bcdef':
                    time.sleep(.1)
                    s.sendall(frame(0, bytes([byte]), fin=False))
                s.sendall(frame(0, b'g'))
                assert read_frame(s) == (1, b'abcdefg')
                s.sendall(frame(8, b'\x03\xe8'))
                assert read_frame(s) == (8, b'\x03\xe8')
                assert s.recv(1) == b''
            print('idle frame activity: PASS complete fragments keep the connection active', flush=True)
        finally:
            stop_process(process)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
