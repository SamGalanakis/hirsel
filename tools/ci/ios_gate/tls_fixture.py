"""Serve short-lived TLS fixtures on the Mac's loopback interface."""

from contextlib import ExitStack, contextmanager
from datetime import datetime, timedelta, timezone
import json
import socketserver
import ssl
import subprocess
from threading import Thread


def openssl(*args, cwd):
    subprocess.run(["openssl", *args], cwd=cwd, check=True, capture_output=True)


def authority(work, name):
    openssl("req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:prime256v1",
            "-nodes", "-days", "14", "-subj", f"/CN=Hirsel CI {name}",
            "-addext", "basicConstraints=critical,CA:TRUE",
            "-addext", "keyUsage=critical,keyCertSign,cRLSign",
            "-keyout", f"{name}.key", "-out", f"{name}.pem", cwd=work)


def certificates(work):
    now = datetime.now(timezone.utc)
    for name in ("primary", "unknown"):
        authority(work, name)
    (work / "index").write_text("")
    (work / "serial").write_text("1000\n")
    (work / "ca.cnf").write_text("""
[ ca ]
default_ca = local
[ local ]
database = index
serial = serial
new_certs_dir = .
default_md = sha256
policy = names
unique_subject = no
x509_extensions = server
[ names ]
commonName = supplied
[ server ]
basicConstraints = critical,CA:FALSE
keyUsage = critical,digitalSignature
extendedKeyUsage = serverAuth
subjectAltName = DNS:hirsel-ci.test
""")
    for name, issuer, start, end in (
        ("valid", "primary", -1, 7),
        ("expired", "primary", -7, -1),
        ("future", "primary", 1, 7),
        ("unanchored", "unknown", -1, 7),
    ):
        openssl("req", "-new", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:prime256v1",
                "-nodes", "-subj", "/CN=hirsel-ci.test", "-keyout", f"{name}.key",
                "-out", f"{name}.csr", cwd=work)
        begin = (now + timedelta(days=start)).strftime("%Y%m%d%H%M%SZ")
        finish = (now + timedelta(days=end)).strftime("%Y%m%d%H%M%SZ")
        openssl("ca", "-batch", "-notext", "-config", "ca.cnf", "-startdate", begin,
                "-enddate", finish, "-cert", f"{issuer}.pem", "-keyfile", f"{issuer}.key",
                "-in", f"{name}.csr", "-out", f"{name}.pem", cwd=work)


class TLSHandler(socketserver.BaseRequestHandler):
    def handle(self):
        self.request.settimeout(5)
        try:
            with self.server.context.wrap_socket(self.request, server_side=True):
                pass
        except (ssl.SSLError, OSError):
            # The negative rows intentionally abort the handshake.
            pass


@contextmanager
def server(work, name):
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(work / f"{name}.pem", work / f"{name}.key")
    with socketserver.ThreadingTCPServer(("127.0.0.1", 0), TLSHandler) as tcp:
        tcp.context = context
        thread = Thread(target=tcp.serve_forever)
        thread.start()
        try:
            yield tcp.server_address[1]
        finally:
            tcp.shutdown()
            thread.join()


@contextmanager
def serve(work, resources):
    certificates(work)
    primary = (work / "primary.pem").read_bytes()
    (resources / "primary.pem").write_bytes(primary)
    (resources / "unknown.pem").write_bytes((work / "unknown.pem").read_bytes())
    with ExitStack() as stack:
        ports = {name: stack.enter_context(server(work, name))
                 for name in ("valid", "expired", "future", "unanchored")}
        (resources / "ports.json").write_text(json.dumps(ports))
        yield work / "primary.pem"
