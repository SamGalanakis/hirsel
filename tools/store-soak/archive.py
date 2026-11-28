"""Disk-backed loopback archive using the recovery fixture's SigV4 checks."""
from collections import Counter
from http.server import ThreadingHTTPServer
from pathlib import Path
import sqlite3
import sys
from threading import Lock, Thread
from urllib.parse import parse_qsl, unquote, urlsplit
from xml.etree.ElementTree import Element, SubElement, tostring

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tests"))
from store_archive_server import ArchiveHandler, KEYS, etag

# These three keys belong exclusively to the local fixture.
KEYS["writer"] = b"writer-fixture-secret"


class SinkHandler(ArchiveHandler):
    def request(self):
        body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
        self.role = self.principal(body)
        url = urlsplit(self.path)
        query = dict(parse_qsl(url.query, keep_blank_values=True))
        key = unquote(url.path)
        methods = {"reader": ("GET", "HEAD"), "writer": ("GET", "HEAD", "PUT"),
                   "expiry": ("GET", "HEAD", "PUT", "DELETE")}
        with self.server.lock:
            if (self.role not in methods or self.command not in methods[self.role]
                    or not (key == "/drill" or key.startswith("/drill/"))):
                if not (self.role is None and self.command == "GET" and key == "/"):
                    self.server.failures += 1
                return self.answer(403)
            self.server.requests[self.command] += 1
            code, data, tag, span = self.apply(key.removeprefix("/drill/"), query, body)
        return self.answer(code, data, tag, span)

    def apply(self, key, query, body):
        if self.command == "GET" and "location" in query:
            return 200, b"<LocationConstraint>us-east-1</LocationConstraint>", None, None
        if self.command == "GET" and "list-type" in query:
            return 200, self.list_objects(query), None, None
        if any(name in query for name in ("uploads", "uploadId", "partNumber")):
            self.server.failures += 1
            return 501, b"", None, None
        if not key.startswith("soak/"):
            self.server.failures += 1
            return 403, b"", None, None
        db = self.server.db
        previous = db.execute("SELECT etag, body FROM objects WHERE key = ?", (key,)).fetchone()
        tag, data = previous if previous is not None else (None, b"")
        if self.command == "PUT":
            match = self.headers.get("If-Match")
            if (match is not None and tag != match) or (self.headers.get("If-None-Match") == "*" and tag is not None):
                return 412, b"", None, None
            tag = etag(body)
            db.execute("INSERT OR REPLACE INTO objects VALUES (?, ?, ?)", (key, tag, body))
            return 200, b"", tag, None
        if self.command == "DELETE":
            db.execute("DELETE FROM objects WHERE key = ?", (key,))
            return 204, b"", None, None
        if tag is None:
            return 404, b"", None, None
        if span := self.headers.get("Range"):
            a, b = span.removeprefix("bytes=").split("-")
            start, end = int(a), min(int(b) if b else len(data) - 1, len(data) - 1)
            if start > end:
                return 416, b"", None, None
            return 206, data[start:end + 1], tag, f"bytes {start}-{end}/{len(data)}"
        return 200, data, tag, None

    def list_objects(self, query):
        prefix = query.get("prefix", "")
        if not prefix.startswith("soak/"):
            raise ValueError(f"unexpected list prefix {prefix!r}")
        last = query.get("start-after", "")
        limit = int(query.get("max-keys", "1000"))
        rows = self.server.db.execute(
            "SELECT key, etag, length(body) FROM objects "
            "WHERE key >= ? AND key < ? AND key > ? ORDER BY key LIMIT ?",
            (prefix, prefix + "\U0010ffff", last, limit + 1),
        ).fetchall()
        xml = Element("ListBucketResult")
        for key, tag, size in rows[:limit]:
            item = SubElement(xml, "Contents")
            SubElement(item, "Key").text = key
            SubElement(item, "ETag").text = tag
            SubElement(item, "Size").text = str(size)
        SubElement(xml, "IsTruncated").text = str(len(rows) > limit).lower()
        return tostring(xml)

    do_GET = do_HEAD = do_PUT = do_DELETE = do_POST = request


class Sink(ThreadingHTTPServer):
    block_on_close = False

    def __init__(self, path):
        super().__init__(("127.0.0.1", 0), SinkHandler)
        self.db = sqlite3.connect(path, isolation_level=None, check_same_thread=False)
        self.db.execute("PRAGMA journal_mode=WAL")
        self.db.execute("PRAGMA cache_size=-8192")
        self.db.execute("CREATE TABLE objects (key TEXT PRIMARY KEY, etag TEXT, body BLOB)")
        self.lock = Lock()
        self.failures = 0
        self.requests = Counter()
        self.thread = Thread(target=self.serve_forever, daemon=True)

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, *_):
        self.shutdown()
        self.server_close()
        self.thread.join()
        with self.lock:
            self.db.close()

    @property
    def endpoint(self):
        return f"http://127.0.0.1:{self.server_port}"

    def metrics(self):
        with self.lock:
            # The archive uses atomic, single PUTs. A multipart request is an error.
            count, size = self.db.execute("SELECT count(*), coalesce(sum(length(body)), 0) FROM objects").fetchone()
            if self.failures:
                raise ValueError(f"loopback archive rejected {self.failures} unexpected requests")
            return {"objects": count, "object_bytes": size, "requests": dict(self.requests), "pending_multipart": 0}
