"""A loopback S3 fixture with signed requests and separate reader and retention roles."""
from hashlib import sha256
from hmac import compare_digest, digest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from base64 import b64decode
import json
from threading import Thread
from urllib.parse import parse_qsl, quote, unquote, urlsplit
from xml.etree.ElementTree import Element, SubElement, tostring


KEYS = {"reader": b"reader-fixture-secret", "expiry": b"expiry-fixture-secret", "child": b"child-fixture-secret"}


def etag(body):
    return '"' + sha256(body).hexdigest()[:32] + '"'


class ArchiveServer(ThreadingHTTPServer):
    def __init__(self, fixture):
        super().__init__(("127.0.0.1", 0), ArchiveHandler)
        self.objects = {key: b64decode(value, validate=True)
                        for key, value in json.loads(fixture.read_bytes()).items()}
        self.requests = []
        self.lose_head_reply = None
        self.thread = Thread(target=self.serve_forever, daemon=True)

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, *_):
        self.shutdown()
        self.server_close()
        self.thread.join()


class ArchiveHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    def log_message(self, *_):
        pass

    def principal(self, body):
        auth = self.headers.get("Authorization", "")
        algorithm, _, fields = auth.partition(" ")
        if algorithm != "AWS4-HMAC-SHA256":
            return None
        fields = dict(item.strip().split("=", 1) for item in fields.split(","))
        key, date, region, service, last = fields["Credential"].split("/")
        if key not in KEYS or service != "s3" or last != "aws4_request":
            return None
        signing = b"AWS4" + KEYS[key]
        for part in [date, region, service, last]:
            signing = digest(signing, part.encode(), "sha256")
        names = fields["SignedHeaders"]
        signed = "".join(f"{k}:{' '.join(self.headers[k].split())}\n" for k in names.split(";"))
        path = urlsplit(self.path)
        query = "&".join(sorted(quote(k, safe="~") + "=" + quote(v, safe="~")
                                for k, v in parse_qsl(path.query, keep_blank_values=True)))
        payload = self.headers.get("X-Amz-Content-Sha256")
        if payload not in ("UNSIGNED-PAYLOAD", sha256(body).hexdigest()):
            return None
        canonical = "\n".join([self.command, path.path, query, signed, names, payload])
        scope = "/".join([date, region, service, last])
        msg = "\n".join([algorithm, self.headers["X-Amz-Date"], scope, sha256(canonical.encode()).hexdigest()])
        signature = digest(signing, msg.encode(), "sha256").hex()
        return key if compare_digest(signature, fields["Signature"]) else None

    def answer(self, code, data=b"", etag=None, span=None):
        self.send_response(code)
        self.send_header("Content-Length", str(len(data)))
        if getattr(self, "role", None):
            proof_key = digest(b"hirsel.sim.s3.proof" + KEYS[self.role], b"", "sha256")
            message = b"hirsel.sim.s3.v1\n" + self.headers["Authorization"].encode()
            self.send_header("x-hirsel-sim-proof", digest(proof_key, message, "sha256").hex())
        if span:
            self.send_header("Content-Range", span)
        if etag:
            self.send_header("ETag", etag)
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(data)

    def request(self):
        body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
        role = self.role = self.principal(body)
        target = urlsplit(self.path)
        key = unquote(target.path.removeprefix("/drill/"))
        self.server.requests.append((role, self.command, key, self.headers.get("Range")))
        if not role or (role == "reader" and self.command not in ("HEAD", "GET")):
            return self.answer(403)
        objects = self.server.objects
        query = dict(parse_qsl(target.query, keep_blank_values=True))
        if self.command == "GET" and "location" in query:
            return self.answer(200, b"<LocationConstraint>us-east-1</LocationConstraint>")
        if role == "child":
            namespace = query.get("prefix", key)
            if not namespace.startswith("children/"):
                return self.answer(403)
        elif key.startswith("children/"):
            return self.answer(403)
        if self.command == "GET" and "list-type" in query:
            xml = Element("ListBucketResult")
            prefix = query.get("prefix", "")
            for k in sorted(k for k in objects if k.startswith(prefix)):
                row = SubElement(xml, "Contents")
                SubElement(row, "Key").text = k
                SubElement(row, "ETag").text = etag(objects[k])
                SubElement(row, "Size").text = str(len(objects[k]))
            SubElement(xml, "IsTruncated").text = "false"
            return self.answer(200, tostring(xml))
        prev = objects.get(key)
        tag = etag(prev) if prev is not None else None
        if self.command in ("GET", "HEAD"):
            if prev is None:
                return self.answer(404)
            if r := self.headers.get("Range"):
                start, end = r.removeprefix("bytes=").split("-")
                a, b = int(start), min(int(end) if end else len(prev)-1, len(prev)-1)
                if a > b:
                    return self.answer(416)
                return self.answer(206, prev[a:b+1], tag, f"bytes {a}-{b}/{len(prev)}")
            return self.answer(200, prev, tag)
        if self.command == "DELETE":
            objects.pop(key, None)
            return self.answer(204)
        if self.command == "PUT":
            if self.headers.get("If-Match") and tag != self.headers["If-Match"]:
                return self.answer(412)
            if self.headers.get("If-None-Match") == "*" and prev is not None:
                return self.answer(412)
            objects[key] = body
            if key == self.server.lose_head_reply:
                self.server.lose_head_reply = None
                self.close_connection = True
                return
            return self.answer(200, etag=etag(body))
        self.answer(405)

    do_GET = do_HEAD = do_PUT = do_DELETE = request
