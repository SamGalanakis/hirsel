"""L-A9 signed archive operations on three frozen Directory checkpoints.

The source created a bytes table named rows, committed keys 1, 2 and 3 in
separate transactions, and drained the archive after each checkpoint. Only
the object bytes are supplied to the drill. There is no source Store.
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from store_archive_server import ArchiveServer, KEYS


FIXTURE = Path("tools/tests/fixtures/store-archive-format14.json")
PARENT = "5ecfd92b020e2a1e0a6d81c54c6fdbda"


def drill(hirsel, server, work):
    env = dict(os.environ, TMPDIR=str(work), XDG_STATE_HOME=str(work / "state"))
    for role, user in [("READ", "reader"), ("EXPIRY", "expiry")]:
        env[f"HIRSEL_ARCHIVE_{role}_ACCESS_KEY_ID"] = user
        env[f"HIRSEL_ARCHIVE_{role}_SECRET_ACCESS_KEY"] = KEYS[user].decode()
    descriptor = {"endpoint": f"http://127.0.0.1:{server.server_port}", "region": "us-east-1",
                  "bucket": "drill", "prefix": "fixture/", "parent": PARENT}
    path = work / "archive.json"
    path.write_text(json.dumps(descriptor))
    source = "@" + str(path)
    original = server.objects.copy()

    def run(*args, code=0, stdin=None):
        p = subprocess.run([hirsel, *args], cwd=work, env=env, input=stdin, text=True,
                           capture_output=True, check=False)
        assert p.returncode == code, (args, p.stdout, p.stderr, server.requests[-2:])
        if "--json" in args:
            reply = json.loads(p.stdout)
            assert (reply["exit"], reply["v"]) == (code, 1), reply
            return reply
        return p.stdout

    def verify(*opts, code=0):
        return run("store", "archive", "verify", source, *opts, "--json", code=code)

    def expire(*opts):
        return run("store", "archive", "expire", source, *opts, "--json")

    def decide(plan, code=0):
        vote = 'host_v1.Decision(plan_digest: "{}", verdict: Approve, entries: [])'.format(
            plan["result"]["plan_digest"])
        return run("op", "decide", plan["operation"]["id"], "--decision", "-", "--json",
                   code=code, stdin=vote)

    info = run("store", "archive", "describe", source, "--json")["result"]
    assert info["parent"] == PARENT and info["floor_base"] == "4" and not info["open_gap"]
    interval, = info["intervals"]
    assert (interval["first_version"], interval["through_version"]) == ("2", "4")
    assert verify()["result"]["verification"] == "references"
    assert verify("--deep")["result"]["verification"] == "store_graph"
    older = next(k for k in original if "/m-00000000000000000004-" in k)
    del server.objects[older]
    damaged = server.objects.copy()
    assert "Lost" in verify(code=1)["error"]["detail"]
    assert "Lost" in verify("--deep", code=1)["error"]["detail"]
    assert server.objects == damaged
    server.objects[older] = original[older]
    pack = next(k for k in original if "/p-" in k)
    bad = bytearray(original[pack])
    bad[len(bad) // 2] ^= 1
    server.objects[pack] = bytes(bad)
    verify(code=1)
    verify("--deep", code=1)
    server.objects[pack] = original[pack]

    child = work / "child"
    plan = run("store", "restore", "--from", source, "--target", "version:main,3",
               "--to", child, "--plan", "--json")
    assert plan["outcome"]["effect"] == "none" and not child.exists()
    receipt = decide(plan)["result"]
    assert receipt["parent"] == PARENT and receipt["child"] != PARENT and receipt["version"] == "3"
    assert receipt["boundary"] == plan["result"]["point"]["boundary"]
    assert f"store {receipt['child']} head 3" in run("store", "inspect", child)
    run("store", "verify", child)
    assert run("store", "backup", child, "second").split() == ["backup", receipt["child"], "head", "3"]
    assert server.objects == original
    stale = expire("--at", interval["first_ms"])
    advance = expire("--keep-from", "6")
    assert advance["result"]["plan"]["proposed_base"] == "6"
    assert server.objects == original
    unexpected = [req for req in server.requests if req[:2] != ("reader", "GET")]
    assert not unexpected, unexpected
    answer = decide(advance)
    assert answer["outcome"]["effect"] == "expired" and int(answer["result"]["deleted"]) > 0
    surviving = server.objects.copy()
    start = len(server.requests)
    decide(stale, code=1)
    assert all(method in ("GET", "HEAD") for _, method, _, _ in server.requests[start:])
    assert server.objects == surviving
    verify("--deep")
    refused = run("store", "restore", "--from", source, "--target", "boundary:" + receipt["boundary"],
                  "--to", "expired-child", "--plan", "--json", code=1)
    assert "BelowFloor" in refused["error"]["detail"]
    print("L-A9 archive: signed read-only selection, missing/corrupt references, offline fresh child, expiry CAS and BelowFloor")


if __name__ == "__main__":
    compiler = (Path(sys.argv[1]) if len(sys.argv) > 1 else
                Path(os.environ["HIRSEL_TOOLCHAIN_BUNDLE"]) / "hirsel").resolve()
    with ArchiveServer(FIXTURE) as archive, tempfile.TemporaryDirectory(prefix="archive-drill-") as temp:
        drill(compiler, archive, Path(temp))
