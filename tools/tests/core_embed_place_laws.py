#!/usr/bin/env python3
"""Slice 2 real compiled Hirsel runner place laws:
- M2 durability (immediate stop->start, SIGKILL crash harness, corrupt store refused)
- M11 real host n_place refusal (PlaceCapacity(Room) -> subsequent valid start)
- M4 real dir_bases persistence & isolation, bogus XDG_DATA_HOME/HOME ignored, Secret env vs file
- M3 SubEnd.ClientCapacity on real subscription image
- m2 Welcome.target equals compiled target triple
- Differential: exact same call sequence through Embed ABI vs Standalone WebSocket yields identical Outcome bytes
"""
import argparse
import base64
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[2]
PLACE_DIR = ROOT / "crates/hirsel-core-rt/testdata/embed_place"
HOST_C = ROOT / "crates/hirsel-core-rt/testdata/embed_place_host.c"


def extract_header_v1() -> str:
    src = (ROOT / "crates/hirsel-core-ids/src/embed.rs").read_text()
    m = re.search(r'pub const HEADER_V1: &str = r#"([\s\S]*?)"#;', src)
    if not m:
        raise AssertionError("HEADER_V1 not found in crates/hirsel-core-ids/src/embed.rs")
    return m.group(1)


def parse_kv(stdout: str) -> dict:
    res = {}
    for line in stdout.splitlines():
        if "=" in line:
            k, v = line.split("=", 1)
            res[k.strip()] = v.strip()
    return res


def alloc_free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def ws_connect(port: int, timeout: float = 10.0) -> socket.socket:
    deadline = time.monotonic() + timeout
    while True:
        try:
            sock = socket.create_connection(("127.0.0.1", port), timeout=5.0)
            break
        except OSError:
            if time.monotonic() > deadline:
                raise
            time.sleep(0.05)
    key = base64.b64encode(os.urandom(16)).decode("ascii")
    req = (
        f"GET /hirsel HTTP/1.1\r\n"
        f"Host: 127.0.0.1:{port}\r\n"
        f"Upgrade: websocket\r\n"
        f"Connection: Upgrade\r\n"
        f"Sec-WebSocket-Key: {key}\r\n"
        f"Sec-WebSocket-Version: 13\r\n"
        f"Sec-WebSocket-Protocol: hirsel.4\r\n\r\n"
    )
    sock.sendall(req.encode("ascii"))
    buf = b""
    while b"\r\n\r\n" not in buf:
        chunk = sock.recv(4096)
        if not chunk:
            raise RuntimeError("WS handshake EOF")
        buf += chunk
    assert b"101" in buf.split(b"\r\n", 1)[0], f"bad WS upgrade: {buf!r}"
    return sock


def ws_send_binary(sock: socket.socket, payload: bytes):
    mask = os.urandom(4)
    n = len(payload)
    hdr = bytearray([0x82])
    if n <= 125:
        hdr.append(0x80 | n)
    elif n <= 65535:
        hdr.append(0x80 | 126)
        hdr.extend(struct.pack(">H", n))
    else:
        hdr.append(0x80 | 127)
        hdr.extend(struct.pack(">Q", n))
    hdr.extend(mask)
    masked = bytes(b ^ mask[i & 3] for i, b in enumerate(payload))
    sock.sendall(bytes(hdr) + masked)


def recv_exact(sock: socket.socket, n: int) -> bytes:
    out = bytearray()
    while len(out) < n:
        chunk = sock.recv(n - len(out))
        if not chunk:
            raise RuntimeError("WS EOF")
        out.extend(chunk)
    return bytes(out)


def ws_recv_binary(sock: socket.socket) -> bytes:
    while True:
        h = recv_exact(sock, 2)
        op = h[0] & 0x0F
        ln = h[1] & 0x7F
        if ln == 126:
            ln = struct.unpack(">H", recv_exact(sock, 2))[0]
        elif ln == 127:
            ln = struct.unpack(">Q", recv_exact(sock, 8))[0]
        body = recv_exact(sock, ln) if ln else b""
        if op == 2:
            return body
        if op == 9:
            ws_send_binary(sock, body)
        elif op == 8:
            raise RuntimeError(f"WS closed: {body!r}")


def enc_hello(manifest: dict) -> bytes:
    # Frame::Hello (tag 0): cred=Some(16B matching CRED16), wants=list of (identity, args_fp, result_fp)
    cred = bytes(range(1, 17))
    out = bytearray(struct.pack("<III", 0, 1, len(cred)))
    out.extend(cred)
    out.extend(struct.pack("<I", len(manifest["entries"])))
    for e in manifest["entries"]:
        ident = bytes(e["identity"])
        out.extend(struct.pack("<I", len(ident)))
        out.extend(ident)
        out.extend(struct.pack("<QQ", int(e["args_fp"]), int(e["result_fp"])))
    return bytes(out)


def enc_call(stream_t: int, stream_n: int, p: int, want: int, deadline_ms: int, args: bytes) -> bytes:
    return (
        struct.pack("<IQQQIII", 1, stream_t, stream_n, p, want, deadline_ms, len(args))
        + args
    )


def parse_welcome_now_ms(payload: bytes) -> int:
    # Frame::Welcome (tag 4): caller(issuer u8, subject Bytes), q u32, budget u64,
    # digest Bytes, target String, n_place u64, now_ms u64, retention_ms u64
    off = 0
    (tag,) = struct.unpack_from("<I", payload, off)
    assert tag == 4, f"expected Welcome tag 4, got {tag}"
    off += 4
    off += 1  # caller.issuer
    (subj_len,) = struct.unpack_from("<I", payload, off)
    off += 4 + subj_len
    off += 4 + 8  # q + budget
    (dig_len,) = struct.unpack_from("<I", payload, off)
    off += 4 + dig_len
    (tgt_len,) = struct.unpack_from("<I", payload, off)
    off += 4 + tgt_len
    off += 8  # n_place
    (now_ms,) = struct.unpack_from("<Q", payload, off)
    return now_ms


def extract_first_outcome_from_batch(payload: bytes) -> bytes:
    # Frame::Batch (tag 6): version u64, complete list<u32>, groups list<Group>, outcomes list<KeyedDone>
    off = 0
    tag, version, n_comp = struct.unpack_from("<IQI", payload, off)
    assert tag == 6, f"expected Batch tag 6, got {tag}"
    off += 16 + 4 * n_comp
    (n_groups,) = struct.unpack_from("<I", payload, off)
    off += 4
    for _ in range(n_groups):
        _, vlen = struct.unpack_from("<II", payload, off)
        off += 8 + vlen
        (n_rows,) = struct.unpack_from("<I", payload, off)
        off += 4
        for _ in range(n_rows):
            _, klen = struct.unpack_from("<HI", payload, off)
            off += 6 + klen
            (opt,) = struct.unpack_from("<I", payload, off)
            off += 4
            if opt == 1:
                (rlen,) = struct.unpack_from("<I", payload, off)
                off += 4 + rlen
    (n_out,) = struct.unpack_from("<I", payload, off)
    assert n_out >= 1, "expected at least 1 outcome in Batch"
    off += 4
    # KeyedDone: stream(t:u64, n:u64), p:u64, outcome:Outcome (remaining bytes for single outcome!)
    off += 24
    return payload[off:]


COMPILER = None


class EmbedRealPlaceLaws(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.compiler = Path(COMPILER).resolve()
        cls.standalone_rt = Path(os.environ["HIRSEL_CORE_RT_ARCHIVE"]).resolve()
        cls.embed_rt = Path(os.environ["HIRSEL_EMBED_RT_ARCHIVE"]).resolve()
        cls.cc = Path(os.environ["LLVM_SYS_231_PREFIX"]) / "bin/clang"
        cls.scratch = tempfile.TemporaryDirectory(prefix="hirsel-embed-place-laws-")
        cls.dir = Path(cls.scratch.name)
        (cls.dir / "hirsel_embed.h").write_text(extract_header_v1())

        # Stage a workspace whose `--libraries` root points `programs/{runner,store}` at the
        # workspace's `runner` and `store` members (same layout as `dev_fixture.sh`) so
        # `is_toolchain_runner` is true, binding `launch_word`/`launch_emit` and the place bit!
        ws = cls.dir / "ws"
        ws.mkdir()
        (ws / "hirsel.toml").write_text('[workspace]\nmembers = ["store", "runner", "embed_place"]\n')
        shutil.copytree(ROOT / "core/programs/store", ws / "store", dirs_exist_ok=True)
        shutil.copytree(ROOT / "core/programs/runner", ws / "runner", dirs_exist_ok=True)
        shutil.copytree(PLACE_DIR, ws / "embed_place", dirs_exist_ok=True)
        cls.place_dir = ws / "embed_place"
        cls.libraries = ws / "libraries"
        (cls.libraries / "programs").mkdir(parents=True)
        os.symlink(ROOT / "core/core", cls.libraries / "core")
        os.symlink(ROOT / "core/std", cls.libraries / "std")
        os.symlink(ws / "runner", cls.libraries / "programs/runner")
        os.symlink(ws / "store", cls.libraries / "programs/store")

        cls.secret_file = cls.dir / "api.secret"
        cls.secret_file.write_text("top-secret-token")

        # 1. Build standalone executable of the real Hirsel place (with grants so Describe & WS can run).
        cls.standalone_store = cls.dir / "standalone_store"
        cls.standalone_data = cls.dir / "standalone_data"
        cls.standalone_cache = cls.dir / "standalone_cache"
        cls.standalone_store.mkdir()
        cls.standalone_data.mkdir()
        cls.standalone_cache.mkdir()
        cls.standalone_exe = cls.dir / "embed_place_standalone"
        cls.cas_dir = cls.dir / "cas"
        subprocess.run(
            [
                str(cls.compiler),
                "build",
                str(cls.place_dir),
                "--libraries",
                str(cls.libraries),
                "--runtime",
                str(cls.standalone_rt),
                "--store",
                str(cls.cas_dir),
                "--dir",
                str(cls.standalone_store),
                "--dir",
                str(cls.standalone_data),
                "--dir",
                str(cls.standalone_cache),
                "--grant",
                f"Secret:api_key=file:{cls.secret_file} verify",
                "--grant",
                "Net=Listen 127.0.0.1/32 0-65535",
                "--grant",
                "Clock",
                "--grant",
                "Random",
                "--grant",
                "Tasks",
                "--grant",
                "Process",
                "--grant",
                "Stderr",
                "--limit",
                str(1 << 30),
                "-o",
                str(cls.standalone_exe),
            ],
            check=True,
        )

        # 2. Obtain REAL native-Describe manifest JSON via `hirsel sdk json`.
        manifest_run = subprocess.run(
            [str(cls.compiler), "sdk", "json", str(cls.standalone_exe)],
            capture_output=True,
            text=True,
        )
        if manifest_run.returncode != 0:
            raise AssertionError(
                f"hirsel sdk manifest failed ({manifest_run.returncode}):\n"
                f"stdout:\n{manifest_run.stdout}\nstderr:\n{manifest_run.stderr}"
            )
        cls.manifest_path = cls.dir / "manifest.json"
        cls.manifest_path.write_text(manifest_run.stdout)
        cls.manifest = json.loads(manifest_run.stdout)

        # 3. Compile `embed_place_host.c` and link real Hirsel place with embed-slot runtime!
        host_obj = cls.dir / "embed_place_host.o"
        subprocess.run(
            [
                str(cls.cc),
                "-O2",
                "-fPIC",
                "-std=c11",
                "-Wall",
                "-Wextra",
                "-Werror",
                f"-I{cls.dir}",
                f"-I{ROOT / 'crates/hirsel-core-rt/include'}",
                "-c",
                str(HOST_C),
                "-o",
                str(host_obj),
            ],
            check=True,
        )
        cc_wrap = cls.dir / "cc-embed"
        cc_wrap.write_text(
            f"#!/bin/sh\nexec {shlex.quote(str(cls.cc))} \"$@\" {shlex.quote(str(host_obj))} "
            "-Wl,--wrap=hirsel_core_os_run_with_sources,--wrap=pwrite,--wrap=pwritev\n"
        )
        cc_wrap.chmod(0o755)

        cls.embed_exe = cls.dir / "embed_place_host"
        subprocess.run(
            [
                str(cls.compiler),
                "build",
                str(cls.place_dir),
                "--libraries",
                str(cls.libraries),
                "--runtime",
                str(cls.embed_rt),
                "--cc",
                str(cc_wrap),
                "--store",
                str(cls.cas_dir),
                "--limit",
                str(1 << 30),
                "-o",
                str(cls.embed_exe),
            ],
            check=True,
        )

    @classmethod
    def tearDownClass(cls):
        cls.scratch.cleanup()

    def _run_host(self, mode: str, store_dir: Path, grants: str, dir_bases: str, extra_env=None) -> str:
        env = dict(os.environ)
        env["HIRSEL_EMBED_MANIFEST"] = str(self.manifest_path)
        env["HIRSEL_EMBED_MODE"] = mode
        env["HIRSEL_EMBED_STORE"] = str(store_dir)
        env["HIRSEL_EMBED_GRANTS"] = grants
        env["HIRSEL_EMBED_DIR_BASES"] = dir_bases
        if extra_env:
            env.update(extra_env)
        try:
            run = subprocess.run([str(self.embed_exe)], capture_output=True, text=True, env=env, timeout=30)
        except subprocess.TimeoutExpired as exc:
            self.fail(
                f"mode {mode} timed out after 30s\nstdout:\n{exc.stdout}\nstderr:\n{exc.stderr}"
            )
        self.assertEqual(
            run.returncode,
            0,
            f"mode {mode} exited {run.returncode}\nstdout:\n{run.stdout}\nstderr:\n{run.stderr}",
        )
        return run.stdout

    def _default_grants(self, data_spec: str, cache_spec: str, secret_spec: str = None) -> str:
        sec = secret_spec or f"Secret:api_key=file:{self.secret_file} verify"
        return (
            f"Dir={data_spec}\n"
            f"Dir={cache_spec}\n"
            f"{sec}\n"
            "Net\nClock\nRandom\nTasks\nProcess\nStderr\n"
        )

    def test_m2_durability_crash_corrupt_and_m2_target(self):
        base = self.dir / "m2"
        store = base / "store"
        data_b = base / "data"
        cache_b = base / "cache"
        store.mkdir(parents=True)
        data_b.mkdir()
        cache_b.mkdir()
        grants = self._default_grants("xdg-data:app", "xdg-cache:app")
        bases = f"data={data_b}\ncache={cache_b}\n"

        # 1. Commit + immediate stop -> immediate start + Welcome.target (m2)
        out = self._run_host("m2-and-m2-target", store, grants, bases)
        kv = parse_kv(out)
        self.assertEqual(kv["welcome_target"], "x86_64-unknown-linux-gnu")
        # Returned(Some(b"alpha")) contains ascii "alpha" hex (616c706861)
        self.assertIn("616c706861", kv["get1"])
        self.assertIn("62657461", kv["get2"])
        # get(999) returns Outcome::Returned(encode(Ok(None))) = tag 0 (4B) + len 8 (4B) + Ok 0 (4B) + None 0 (4B)
        self.assertEqual(kv["get999"], "00000000080000000000000000000000")

        # 2. SIGKILL crash harness on real store
        crash_store = base / "crash_store"
        crash_store.mkdir()
        out_c = self._run_host("m2-crash-and-reopen", crash_store, grants, bases)
        kvc = parse_kv(out_c)
        # Key 10 ("before-kill" = 6265666f72652d6b696c6c) MUST survive SIGKILL
        self.assertIn("6265666f72652d6b696c6c", kvc["crash_get10"])
        # Key 11 must be either Ok(None) or wholly "maybe-after" (6d617962652d6166746572)
        self.assertTrue(
            kvc["crash_get11"] == "00000000080000000000000000000000"
            or "6d617962652d6166746572" in kvc["crash_get11"],
            f"unexpected crash_get11: {kvc['crash_get11']}",
        )

        # 3. Transient ENOSPC poison -> HIRSEL_END_PLACE_FAILED -> stop -> start succeeds with key 20 intact;
        #    then corrupt the store on disk -> start returns HIRSEL_REFUSED.
        corrupt_store = base / "corrupt_store"
        corrupt_store.mkdir()
        out_p = self._run_host("m2-transient-poison", corrupt_store, grants, bases)
        kvp = parse_kv(out_p)
        self.assertIn("64757261626c65", kvp["poison_get20"])  # "durable"
        corrupted = False
        for p in corrupt_store.rglob("*"):
            if p.is_file() and p.stat().st_size > 0:
                p.write_bytes(b"\xff" * p.stat().st_size)
                corrupted = True
        self.assertTrue(corrupted, "expected store files to corrupt")
        self._run_host("m2-corrupt-refused", corrupt_store, grants, bases)

    def test_m11_real_host_n_place_capacity_refusal(self):
        base = self.dir / "m11"
        store = base / "store"
        data_b = base / "data"
        cache_b = base / "cache"
        store.mkdir(parents=True)
        data_b.mkdir()
        cache_b.mkdir()
        grants = self._default_grants("xdg-data:app", "xdg-cache:app")
        bases = f"data={data_b}\ncache={cache_b}\n"
        self._run_host("m11-real-capacity", store, grants, bases)

    def test_m4_real_dir_bases_isolation_bogus_env_and_secret(self):
        base = self.dir / "m4"
        store = base / "store"
        data_a = base / "data_a"
        cache_a = base / "cache_a"
        cache_b = base / "cache_b"
        store.mkdir(parents=True)
        data_a.mkdir()
        cache_a.mkdir()
        cache_b.mkdir()

        bogus_env = {
            "XDG_DATA_HOME": "/nonexistent/bogus_xdg_data",
            "XDG_CACHE_HOME": "/nonexistent/bogus_xdg_cache",
            "HOME": "/nonexistent/bogus_home",
            "TEST_ENV_SECRET": "must-never-be-read",
        }
        grants = self._default_grants("xdg-data:embedapp", "xdg-cache:embedapp")
        bases_a = f"data={data_a}\ncache={cache_a}\n"

        # 1. Write via real place into data_a/embedapp/state.bin and cache_a/embedapp/tmp.bin
        self._run_host("m4-dirs-write", store, grants, bases_a, extra_env=bogus_env)
        self.assertEqual((data_a / "embedapp/state.bin").read_bytes(), b"persisted-v1")
        self.assertEqual((cache_a / "embedapp/tmp.bin").read_bytes(), b"persisted-v1")

        # 2. Restart with same data_a, fresh cache_b -> finds state.bin, cache_b has no tmp.bin
        bases_b = f"data={data_a}\ncache={cache_b}\n"
        out = self._run_host("m4-dirs-read", store, grants, bases_b, extra_env=bogus_env)
        kv = parse_kv(out)
        self.assertIn("7065727369737465642d7631", kv["read_data"])  # "persisted-v1"
        self.assertFalse((cache_b / "embedapp/tmp.bin").exists())

        # 3. Secret:api_key=env:TEST_ENV_SECRET refused (SecretSource), relative file: refused
        bad_env_grants = self._default_grants(
            "xdg-data:embedapp", "xdg-cache:embedapp", "Secret:api_key=env:TEST_ENV_SECRET verify"
        )
        out_ref = self._run_host("m4-refused", store, bad_env_grants, bases_a, extra_env=bogus_env)
        self.assertIn("SecretSource", out_ref)

        bad_rel_grants = self._default_grants(
            "xdg-data:embedapp", "xdg-cache:embedapp", "Secret:api_key=file:relative.secret verify"
        )
        out_rel = self._run_host("m4-refused", store, bad_rel_grants, bases_a, extra_env=bogus_env)
        self.assertIn("SecretSource", out_rel)

    def test_m3_real_sub_end_client_capacity(self):
        base = self.dir / "m3"
        store = base / "store"
        data_b = base / "data"
        cache_b = base / "cache"
        store.mkdir(parents=True)
        data_b.mkdir()
        cache_b.mkdir()
        grants = self._default_grants("xdg-data:app", "xdg-cache:app")
        bases = f"data={data_b}\ncache={cache_b}\n"
        self._run_host("m3-sub-client-capacity", store, grants, bases)

    def test_differential_embed_abi_vs_standalone_websocket_outcome_bytes(self):
        base = self.dir / "diff"
        embed_store = base / "embed_store"
        ws_store = base / "ws_store"
        data_b = base / "data"
        cache_b = base / "cache"
        embed_store.mkdir(parents=True)
        ws_store.mkdir()
        data_b.mkdir()
        cache_b.mkdir()
        grants = self._default_grants("xdg-data:app", "xdg-cache:app")
        bases = f"data={data_b}\ncache={cache_b}\n"

        # 1. Run sequence via Embed ABI
        out_embed = self._run_host("diff-embed", embed_store, grants, bases)
        kv_embed = parse_kv(out_embed)

        # 2. Launch EXACT same compiled standalone place as WebSocket server
        port = alloc_free_port()
        proc = subprocess.Popen(
            [str(self.standalone_exe), str(port)],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        try:
            sock = ws_connect(port, timeout=15.0)
            try:
                ws_send_binary(sock, enc_hello(self.manifest))
                welcome = ws_recv_binary(sock)
                now_ms = parse_welcome_now_ms(welcome)
                for _ in range(len(self.manifest["entries"])):
                    placed = ws_recv_binary(sock)
                    self.assertEqual(struct.unpack_from("<I", placed, 0)[0], 5)  # Placed

                # Call 0: put(100, b"diff-hello")
                args_put = struct.pack("<QI", 100, 10) + b"diff-hello"
                ws_send_binary(sock, enc_call(now_ms, 1, 0, 0, 5000, args_put))
                ws_put = extract_first_outcome_from_batch(ws_recv_binary(sock)).hex()

                # Call 1: get(100)
                ws_send_binary(sock, enc_call(now_ms, 1, 1, 1, 5000, struct.pack("<Q", 100)))
                ws_get100 = extract_first_outcome_from_batch(ws_recv_binary(sock)).hex()

                # Call 2: get(999)
                ws_send_binary(sock, enc_call(now_ms, 1, 2, 1, 5000, struct.pack("<Q", 999)))
                ws_get999 = extract_first_outcome_from_batch(ws_recv_binary(sock)).hex()

                self.assertTrue(ws_put.startswith("00000000"), f"put not Returned: {ws_put}")
                self.assertIn("646966662d68656c6c6f", ws_get100)  # "diff-hello"
                self.assertEqual(ws_get999, "00000000080000000000000000000000")
                self.assertEqual(kv_embed["diff_put100"], ws_put)
                self.assertEqual(kv_embed["diff_get100"], ws_get100)
                self.assertEqual(kv_embed["diff_get999"], ws_get999)
            finally:
                sock.close()
        finally:
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait()
            if proc.stdout:
                proc.stdout.close()
            if proc.stderr:
                proc.stderr.close()


if __name__ == "__main__":
    p = argparse.ArgumentParser()
    p.add_argument("--compiler", required=True)
    args, rest = p.parse_known_args()
    COMPILER = args.compiler
    unittest.main(argv=[sys.argv[0], *rest])
