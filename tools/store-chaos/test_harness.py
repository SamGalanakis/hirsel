#!/usr/bin/env python3
"""Offline unit/self-checks for the Phase 6 chaos harness (zero external infra)."""
import base64
import gzip
from http.server import BaseHTTPRequestHandler, HTTPServer
import http.client
import json
from pathlib import Path
import ssl
import struct
import subprocess
import sys
import tempfile
import threading
import unittest

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

from history import History, digest
from log_writes import DISCARD, FLUSH, FUA, MARK, cases, entries, replay
from recover import verify_copy
from relay import running


class HistoryOracleTests(unittest.TestCase):
    def test_exact_history_and_uncertain_recovery(self):
        with tempfile.TemporaryDirectory() as tmp:
            h = History(Path(tmp) / 'h.ndjson', base=1)
            h.invoke(1)
            h.returned(1, 'ACK 2 1 0 0\n')
            h.invoke(2)
            h.returned(2, 'ACK 3 2 0 0\n')
            h.invoke(3)
            h.returned(3, '')  # timeout / uncertain
            # Model after ops 1, 2, 3:
            # op 1: a=1, b=4 -> read [0, 0], write data[1]=1, data[4]=2
            # op 2: a=2, b=5 -> read [0, 0], write data[2]=2, data[5]=3
            # op 3: a=3, b=6 -> read [0, 0], write data[3]=3, data[6]=4
            dump_with_3 = '\n'.join([
                'TX 2 1 0 0',
                'TX 3 2 0 0',
                'TX 4 3 0 0',
                'KEY 1 1', 'KEY 2 2', 'KEY 3 3', 'KEY 4 2', 'KEY 5 3', 'KEY 6 4',
                'HEAD 4',
            ])
            res = h.verify(dump_with_3)
            self.assertEqual(res['acked'], 2)
            self.assertEqual(res['recovered'], 3)
            self.assertEqual(res['acked_loss'], 0)
            self.assertEqual(res['digest'], digest({1: 1, 2: 2, 3: 3, 4: 2, 5: 3, 6: 4}))

            # Also valid if uncertain op 3 did not commit.
            dump_without_3 = '\n'.join([
                'TX 2 1 0 0',
                'TX 3 2 0 0',
                'KEY 1 1', 'KEY 2 2', 'KEY 4 2', 'KEY 5 3',
                'HEAD 3',
            ])
            res2 = h.verify(dump_without_3)
            self.assertEqual(res2['acked'], 2)
            self.assertEqual(res2['recovered'], 2)

    def test_rejects_lost_ack_wrong_read_and_digest_mismatch(self):
        with tempfile.TemporaryDirectory() as tmp:
            h = History(Path(tmp) / 'h.ndjson', base=1)
            h.invoke(1)
            h.returned(1, 'ACK 2 1 0 0\n')
            h.invoke(2)
            h.returned(2, 'ACK 3 2 0 0\n')

            with self.assertRaisesRegex(ValueError, 'lost durable acknowledgement'):
                h.verify('TX 2 1 0 0\nKEY 1 1\nKEY 4 2\nHEAD 2\n')
            with self.assertRaisesRegex(ValueError, 'read differs'):
                h.verify('TX 2 1 9 0\nTX 3 2 0 0\nKEY 1 1\nKEY 2 2\nKEY 4 2\nKEY 5 3\nHEAD 3\n')
            with self.assertRaisesRegex(ValueError, 'final state differs'):
                h.verify('TX 2 1 0 0\nTX 3 2 0 0\nKEY 1 99\nKEY 2 2\nKEY 4 2\nKEY 5 3\nHEAD 3\n')

            h2 = History(Path(tmp) / 'h2.ndjson', base=1)
            h2.invoke(1)
            h2.returned(1, 'ACK 2 1 0 0\n')
            h2.invoke(2)
            h2.returned(2, '')  # timed out after op 1 acked
            with self.assertRaisesRegex(ValueError, 'real-time order inverted'):
                h2.verify('TX 2 2 0 0\nTX 3 1 0 0\nKEY 1 1\nKEY 2 2\nKEY 4 2\nKEY 5 3\nHEAD 3\n')


class LogWritesTests(unittest.TestCase):
    def _build_log(self, path, sector_size, records):
        with path.open('wb') as f:
            sb = struct.pack('<QQQI', 0x6a736677736872, 1, len(records), sector_size)
            f.write(sb.ljust(sector_size, b'\0'))
            for block, flags, payload, label in records:
                if flags & MARK:
                    raw_label = label.encode()
                    hdr = struct.pack('<4Q', 0, 0, MARK, len(raw_label)) + raw_label
                    f.write(hdr.ljust(sector_size, b'\0'))
                else:
                    sectors = len(payload) // sector_size if payload else 0
                    hdr = struct.pack('<4Q', block, sectors, flags, 0)
                    f.write(hdr.ljust(sector_size, b'\0'))
                    if payload and not (flags & DISCARD):
                        f.write(payload)

    def test_parse_cases_and_replay(self):
        sz = 512
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            log_path = root / 'writes'
            base = root / 'base'
            base.write_bytes(bytes(sz * 8))
            recs = [
                (0, 0, b'A' * sz, ''),
                (0, MARK, b'', 'ack-floor'),
                (1, 0, b'B' * sz + b'C' * sz, ''),
                (3, 0, b'D' * sz, ''),
                (0, FLUSH | FUA, b'', ''),
                (0, MARK, b'', 'end-window'),
            ]
            self._build_log(log_path, sz, recs)
            parsed_sz, parsed = entries(log_path)
            self.assertEqual(parsed_sz, sz)
            self.assertEqual(len(parsed), len(recs))
            found = list(cases(parsed))
            variants = [c['variant'] for c in found]
            self.assertEqual(variants, ['barrier', 'omit-sector', 'later-sector-first'])

            # Replay barrier: blocks 0, 1, 2, 3 all written.
            img = root / 'img-barrier'
            img.write_bytes(base.read_bytes())
            replay(img, log_path, parsed_sz, parsed, found[0])
            data = img.read_bytes()
            self.assertEqual(data[0:sz], b'A' * sz)
            self.assertEqual(data[sz:2 * sz], b'B' * sz)
            self.assertEqual(data[2 * sz:3 * sz], b'C' * sz)
            self.assertEqual(data[3 * sz:4 * sz], b'D' * sz)

            # Replay later-sector-first: block 3 applied, blocks 1..2 still zeroed!
            img2 = root / 'img-later'
            img2.write_bytes(base.read_bytes())
            replay(img2, log_path, parsed_sz, parsed, found[2])
            data2 = img2.read_bytes()
            self.assertEqual(data2[0:sz], b'A' * sz)
            self.assertEqual(data2[sz:3 * sz], bytes(2 * sz))
            self.assertEqual(data2[3 * sz:4 * sz], b'D' * sz)


class RecoverTests(unittest.TestCase):
    def test_rejects_traversal_filenames(self):
        with tempfile.TemporaryDirectory() as tmp:
            packed = base64.b64encode(gzip.compress(b'x')).decode()
            for bad in ('../escape', 'sub/file', '.', '..'):
                with self.assertRaisesRegex(ValueError, 'invalid Store filename'):
                    verify_copy({bad: packed}, Path(tmp) / f'target-{abs(hash(bad))}', Path('/bin/true'))


class RelayTests(unittest.TestCase):
    def test_http_oneshot_and_tcp_faults(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            cert, key = root / 'pub.crt', root / 'priv.key'
            subprocess.run([
                'openssl', 'req', '-new', '-newkey', 'rsa:2048', '-x509', '-nodes', '-days', '1',
                '-subj', '/CN=localhost', '-addext', 'subjectAltName=DNS:localhost,IP:127.0.0.1',
                '-keyout', key, '-out', cert,
            ], check=True, capture_output=True)

            body = b'H' * 4096

            class Origin(BaseHTTPRequestHandler):
                def log_message(self, *args):
                    pass

                def do_GET(self):
                    self.send_response(200)
                    self.send_header('Content-Length', str(len(body)))
                    self.end_headers()
                    self.wfile.write(body)

                def do_PUT(self):
                    _ = self.rfile.read(int(self.headers.get('Content-Length', 0)))
                    self.send_response(200)
                    self.send_header('Content-Length', '0')
                    self.end_headers()

            class QuietHTTPServer(HTTPServer):
                def handle_error(self, *args):
                    pass

            origin = QuietHTTPServer(('127.0.0.1', 0), Origin)
            ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
            ctx.load_cert_chain(cert, key)
            origin.socket = ctx.wrap_socket(origin.socket, server_side=True)
            t = threading.Thread(target=origin.serve_forever, daemon=True)
            t.start()
            client_ctx = ssl.create_default_context(cafile=cert)
            try:
                # HTTP oneshot slowdown: 1st PUT -> 503 SlowDown, 2nd PUT -> 200 OK.
                with running(origin.server_address[1], str(cert), str(key), 'http', 'slowdown',
                             oneshot=True, match='put') as r:
                    c = http.client.HTTPSConnection('127.0.0.1', r.server_address[1], context=client_ctx)
                    c.request('PUT', '/obj', body=b'abc')
                    resp1 = c.getresponse()
                    raw1 = resp1.read()
                    c.close()
                    self.assertEqual(resp1.status, 503)
                    self.assertIn(b'SlowDown', raw1)
                    self.assertTrue(r.observed.wait(2))
                    self.assertEqual(r.outcome['code'], 'SlowDown')

                    c2 = http.client.HTTPSConnection('127.0.0.1', r.server_address[1], context=client_ctx)
                    c2.request('PUT', '/obj', body=b'abc')
                    resp2 = c2.getresponse()
                    resp2.read()
                    c2.close()
                    self.assertEqual(resp2.status, 200)

                # TCP passthrough corrupt: ciphertext flip causes SSLError and records hit.
                with running(origin.server_address[1], str(cert), str(key), 'tcp', 'corrupt', oneshot=True) as r:
                    c = http.client.HTTPSConnection('127.0.0.1', r.server_address[1], context=client_ctx, timeout=2)
                    with self.assertRaises((ssl.SSLError, OSError, http.client.HTTPException)):
                        c.request('GET', '/obj')
                        c.getresponse().read()
                    c.close()
                    self.assertTrue(r.observed.wait(2))
                    self.assertEqual(r.outcome['action'], 'corrupt')
            finally:
                origin.shutdown()
                origin.server_close()
                t.join()


if __name__ == '__main__':
    unittest.main()
