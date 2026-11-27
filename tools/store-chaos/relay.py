"""Dual-mode HTTP/TLS and TCP-passthrough fault relay for loopback TLS MinIO."""
import contextlib
import http.client
from http.server import BaseHTTPRequestHandler
import select
import socket
import socketserver
import ssl
import struct
import threading


def matches_http(handler, rule):
    if not rule:
        return True
    if rule == 'put':
        return handler.command == 'PUT'
    if rule == 'cas':
        return handler.command == 'PUT' and bool(handler.headers.get('If-Match'))
    if rule == 'get':
        return handler.command == 'GET'
    if rule == 'complete':
        return handler.command == 'POST' and 'uploadId=' in handler.path
    return True


class HTTP(BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'

    def log_message(self, *args):
        pass  # Request targets and headers can carry SigV4 credentials.

    def reply(self, status, headers, data):
        self.close_connection = True
        self.send_response_only(status)
        for name, value in headers:
            self.send_header(name, value)
        self.send_header('Connection', 'close')
        self.end_headers()
        self.wfile.write(data)

    def handle_one(self):
        relay = self.server
        fault = relay.fault if matches_http(self, relay.match) else ''
        if fault and relay.oneshot and relay.observed.is_set():
            fault = ''
        if fault in ('slowdown', 'multipart-error', 'conflict', 'no-upload'):
            if relay.claim_slot():
                status, code = {'slowdown': (503, 'SlowDown'), 'multipart-error': (200, 'InternalError'),
                                'conflict': (409, 'ConditionalRequestConflict'),
                                'no-upload': (404, 'NoSuchUpload')}[fault]
                body = f'<Error><Code>{code}</Code></Error>'.encode()
                self.reply(status, [('Content-Length', len(body)), ('Content-Type', 'application/xml')], body)
                relay.hit(status=status, code=code, forwarded=False)
                return
            fault = ''
        origin = http.client.HTTPSConnection('127.0.0.1', relay.origin, context=relay.ca, timeout=6)
        try:
            origin.putrequest(self.command, self.path, skip_host=True, skip_accept_encoding=True)
            for name, value in self.headers.raw_items():
                origin.putheader(name, value)
            if self.headers.get('Expect', '').lower() == '100-continue':
                self.wfile.write(b'HTTP/1.1 100 Continue\r\n\r\n')
            body = self.rfile.read(int(self.headers.get('Content-Length', 0)))
            origin.endheaders(body)
            response = origin.getresponse()
            payload = response.read()
            headers = [(k, v) for k, v in response.getheaders() if k.lower() not in
                       ('transfer-encoding', 'content-length', 'connection')]
            headers.append(('Content-Length', len(payload)))
            if fault == 'drop-cas' and relay.claim_slot():
                relay.hit(status=response.status, cas=bool(self.headers.get('If-Match')), dropped=True)
                return
            if fault in ('truncate', 'slow-get') and len(payload) >= 2 and relay.claim_slot():
                size = max(1, len(payload) // 2)
                self.reply(response.status, headers, payload[:size])
                self.wfile.flush()
                if fault == 'slow-get':
                    relay.stop.wait(2)
                relay.hit(status=response.status, sent=size, origin_bytes=len(payload))
                return
            self.reply(response.status, headers, payload)
            if (not relay.match or matches_http(self, relay.match)) and (
                not relay.oneshot or (fault == 'claim' and response.status == 412 and self.headers.get('If-Match'))
            ):
                relay.hit(status=response.status, range=self.headers.get('Range'),
                          if_match=bool(self.headers.get('If-Match')))
        finally:
            origin.close()

    do_GET = do_PUT = do_POST = do_DELETE = do_HEAD = handle_one


def read_exact(sock, size):
    data = bytearray()
    while len(data) < size:
        part = sock.recv(size - len(data))
        if not part:
            raise EOFError
        data.extend(part)
    return data


class TCP(socketserver.BaseRequestHandler):
    def handle(self):
        relay = self.server
        downstream = self.request
        downstream.settimeout(4)
        with socket.create_connection(('127.0.0.1', relay.origin), timeout=4) as upstream:
            saw_request = False
            while not relay.stop.is_set():
                readable, _, _ = select.select([downstream, upstream], [], [], 0.2)
                for source in readable:
                    header = read_exact(source, 5)
                    record = read_exact(source, int.from_bytes(header[3:], 'big'))
                    client = source is downstream
                    peer = upstream if client else downstream
                    active = '' if (relay.oneshot and relay.observed.is_set()) else relay.fault
                    if client and header[0] == 22 and active == 'handshake-stall' and relay.claim_slot():
                        relay.hit(client_hello=record[0] == 1)
                        relay.stop.wait(2)
                        return
                    # Trigger on the first server application record after client application
                    # traffic (works for both TLS 1.2 GET responses and TLS 1.3 post-Finished traffic).
                    if not client and saw_request and header[0] == 23 and active and relay.claim_slot():
                        if active == 'corrupt':
                            record[-1] ^= 1
                            peer.sendall(header + record)
                        else:
                            peer.sendall(header + record[:len(record) // 2])
                            if active == 'rst':
                                peer.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, struct.pack('ii', 1, 0))
                            elif active == 'half-close':
                                peer.shutdown(socket.SHUT_WR)
                                relay.hit(record_bytes=len(record), shutdown='SHUT_WR')
                                relay.stop.wait(2)
                                return
                        relay.hit(record_bytes=len(record), action=active)
                        return
                    peer.sendall(header + record)
                    saw_request |= client and header[0] == 23


class Relay(socketserver.ThreadingTCPServer):
    daemon_threads = True
    allow_reuse_address = True

    def __init__(self, origin, cert, key, mode, fault, oneshot=False, match=None, ca=None):
        self.origin, self.fault = origin, fault
        self.oneshot, self.match = oneshot, match
        self.stop = threading.Event()
        self.observed = threading.Event()
        self.outcome = None
        self._lock = threading.Lock()
        self._tripped = False
        self.ca = ssl.create_default_context(cafile=ca or cert)
        super().__init__(('127.0.0.1', 0), HTTP if mode == 'http' else TCP)
        if mode == 'http':
            tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
            tls.load_cert_chain(cert, key)
            self.socket = tls.wrap_socket(self.socket, server_side=True)

    def claim_slot(self):
        if not self.oneshot:
            return True
        with self._lock:
            if self._tripped:
                return False
            self._tripped = True
            return True

    def handle_error(self, request, address):
        # Never print request frames. A missing witness fails the caller's check.
        pass

    def hit(self, **fields):
        self.outcome = fields
        self.observed.set()


@contextlib.contextmanager
def running(origin, cert, key, mode, fault, oneshot=False, match=None, ca=None):
    with Relay(origin, cert, key, mode, fault, oneshot=oneshot, match=match, ca=ca) as relay:
        thread = threading.Thread(target=relay.serve_forever, kwargs={'poll_interval': 0.05})
        thread.start()
        try:
            yield relay
        finally:
            relay.stop.set()
            relay.shutdown()
            thread.join()
