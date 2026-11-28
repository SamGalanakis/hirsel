"""L-A9 signed archive operations on three frozen Directory checkpoints.

The source created a bytes table named rows, committed keys 1, 2 and 3 in
separate transactions, and drained the archive after each checkpoint. Only
the object bytes are supplied to the drill. There is no source Store.
"""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
from store_index_recovery import check_index_repair
from store_gap_recovery import check_archive_gap
from store_archive_server import ArchiveServer, KEYS


FIXTURE = Path("tools/tests/fixtures/store-archive-format14.json")
PARENT = "5ecfd92b020e2a1e0a6d81c54c6fdbda"


def drill(hirsel, server, work, release):
    env = dict(os.environ, TMPDIR=str(work), XDG_STATE_HOME=str(work / "state"))
    for key in list(env):
        if key.startswith(("HIRSEL_ARCHIVE_", "HIRSEL_RESTORE_BUCKET_")):
            del env[key]
    env["HIRSEL_ARCHIVE_READ_ACCESS_KEY_ID"] = "reader"
    env["HIRSEL_ARCHIVE_READ_SECRET_ACCESS_KEY"] = KEYS["reader"].decode()
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
    released = run("store", "verify", child, "--release", release)
    assert "0 problems" in released and "not walked" not in released, released
    subprocess.run([release, "check-cut"], cwd=child, check=True)
    print(f"release-backed cold drill: child={receipt['child']} attempt={receipt['attempt']} version={receipt['version']} boundary={receipt['boundary']} {released.strip()}", flush=True)
    assert run("store", "backup", child, "second").split() == ["backup", receipt["child"], "head", "3"]
    assert server.objects == original
    stale = expire("--at", interval["first_ms"])
    advance = expire("--keep-from", "6")
    assert advance["result"]["plan"]["proposed_base"] == "6"
    assert server.objects == original
    unexpected = [req for req in server.requests if req[:2] != ("reader", "GET")]
    assert not unexpected, unexpected
    assert all(req == (None, "GET", "/", None) for req in server.denied), server.denied
    env["HIRSEL_ARCHIVE_EXPIRY_ACCESS_KEY_ID"] = "expiry"
    env["HIRSEL_ARCHIVE_EXPIRY_SECRET_ACCESS_KEY"] = KEYS["expiry"].decode()
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
    env["HIRSEL_RESTORE_BUCKET_ACCESS_KEY_ID"] = "child"
    env["HIRSEL_RESTORE_BUCKET_SECRET_ACCESS_KEY"] = KEYS["child"].decode()
    location = {k: v for k, v in descriptor.items() if k != "parent"}
    profile_path = work / "bucket.json"
    profile = {"location": dict(location, prefix="children/archive/"), "level": "bucket",
               "source_place": None, "host_stream": None}

    def bucket_plan(origin, target="latest", code=0):
        profile_path.write_text(json.dumps(profile))
        profile_path.chmod(0o600)
        return run("store", "restore", "--from", origin, "--target", target,
                   "--to", "@" + str(profile_path), "--plan", "--json", code=code)

    # Trusted coordinates fence every reserved Place and stream prefix before writes.
    profile["host_stream"] = dict(location, prefix="children/")
    bucket_plan(source, code=1)
    profile["host_stream"] = None
    untouched = server.objects.copy()
    review = bucket_plan(source)
    other = bucket_plan(source)
    assert review["outcome"]["effect"] == "none" and server.objects == untouched
    server.lose_head_reply = "children/archive/head"
    answer = decide(review)
    assert server.lose_head_reply is None and answer["result"]["parent"] == PARENT
    assert answer["result"]["child"] != PARENT and answer["result"]["version"] == "4"
    assert answer["result"]["destination"] == profile["location"]
    durable = server.objects.copy()
    decide(other, code=1)
    assert server.objects == durable
    # Recover the exact genesis after losing the external acknowledgement, with
    # the source profile and reader credential removed.
    attempt = review["result"]["attempt"]
    published, = (work / "state").rglob(f"store-{attempt}.published.json")
    published.unlink()
    path.unlink()
    del env["HIRSEL_ARCHIVE_READ_SECRET_ACCESS_KEY"]
    observed = run("op", "get", review["operation"]["id"], "--json")
    assert observed["result"] == answer["result"]
    profile["location"]["prefix"] = "children/backup/"
    backup = bucket_plan(work / "second")
    assert decide(backup)["result"]["parent"] == receipt["child"]
    assert {k:v for k,v in server.objects.items() if not k.startswith("children/")} == surviving
    print("L-A9 archive: signed read-only selection, missing/corrupt references, offline fresh child, expiry CAS and BelowFloor")


if __name__ == "__main__":
    compiler = (Path(sys.argv[1]) if len(sys.argv) > 1 else
                Path(os.environ["HIRSEL_TOOLCHAIN_BUNDLE"]) / "hirsel").resolve()
    with tempfile.TemporaryDirectory(prefix="archive-drill-", dir=os.environ.get("HIRSEL_OPERATOR_TEST_ROOT")) as temp:
        root = Path(temp)
        if len(sys.argv) == 4:
            release, indexed = [Path(p).resolve() for p in sys.argv[2:]]
        else:
            package = root / "verifier"
            shutil.copytree(FIXTURE.parent / "store-recovery-release", package)
            release, indexed = root / "recovery-release", root / "indexed-release"
            for artifact, entry in [(release, "main"), (indexed, "indexed")]:
                subprocess.run([compiler, "build", package, "--backend", "llvm", "--entry", f"hirsel.ai/recovery_drill@1/main/{entry}",
                                "--grant", "Net=Listen 127.0.0.1/32 0-65535", "--dir", "/proc/self/cwd",
                                "--grant", "Clock", "--grant", "Random", "--grant", "Tasks",
                                "--grant", "Process", "--grant", "Stderr", "--limit", "2147483648",
                                "-o", artifact], cwd=root, check=True)
        check_index_repair(compiler, indexed, root, FIXTURE.parent / "store-index-damage-format14.json")
        check_archive_gap(compiler, release, root, FIXTURE.parent / "store-archive-gap-format14.json")
        with ArchiveServer(FIXTURE) as archive:
            drill(compiler, archive, root, release)
