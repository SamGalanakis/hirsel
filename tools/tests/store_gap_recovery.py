"""Read a healed archive after a Directory exceeded its 1024-byte hold during an outage."""
import json
import os
from pathlib import Path
import subprocess

from store_archive_server import ArchiveServer, KEYS


def check_archive_gap(hirsel, release, root, fixture):
    parent = "9f9d1c1f02fbbcf8f461780520e0f2d0"
    child = root / "healed-child"
    descriptor = root / "healed.json"
    env = dict(os.environ, XDG_STATE_HOME=str(root / "operations"), TMPDIR=str(root),
               HIRSEL_ARCHIVE_READ_ACCESS_KEY_ID="reader",
               HIRSEL_ARCHIVE_READ_SECRET_ACCESS_KEY=KEYS["reader"].decode())

    def run(*args, code=0, stdin=None):
        p = subprocess.run([hirsel, *args, "--json"], env=env, input=stdin, capture_output=True, text=True)
        assert p.returncode == code, (args, p.stdout, p.stderr)
        reply = json.loads(p.stdout)
        assert reply["exit"] == code and reply["v"] == 1, reply
        return reply

    with ArchiveServer(fixture) as server:
        descriptor.write_text(json.dumps(dict(endpoint=f"http://127.0.0.1:{server.server_port}",
                                              region="us-east-1", bucket="drill", prefix="trace-sink/",
                                              parent=parent)))
        source = "@" + str(descriptor)
        original = server.objects.copy()
        state = run("store", "archive", "describe", source)["result"]
        assert [(i["first_version"], i["through_version"], i["closed_by_gap"])
                for i in state["intervals"]] == [("4", "4", False), ("2", "2", True)], state
        assert not state["open_gap"]
        hold, = state["holds"]
        assert (hold["child"], hold["base"], hold["until_ms"]) == (
            "00000000000000070000000000000002", "4", str(2**64 - 1)), hold
        assert int(hold["stamp"]) > 0
        expiry = run("store", "archive", "expire", source, "--keep-from", "6")["result"]["plan"]
        assert expiry["proposed_base"] == "4" and expiry["limiting_holds"] == [hold], expiry
        run("store", "archive", "verify", source, "--deep")
        def plan(target, code):
            return run("store", "restore", "--from", source, "--target", target, "--to", child, "--plan", code=code)
        gap = plan("version:main,3", 1)
        assert "InGap" in gap["error"]["detail"] and not child.exists(), gap
        # A missing promised manifest is loss even with a separate historical gap.
        key = next(k for k in original if "/m-00000000000000000006-" in k)
        del server.objects[key]
        failed = run("store", "archive", "verify", source, "--deep", code=1)
        assert "Lost" in failed["error"]["detail"], failed
        assert server.objects == {k: v for k, v in original.items() if k != key}
        server.objects[key] = original[key]
        planned = plan("latest", 0)
        vote = 'host_v1.Decision(plan_digest: "{}", verdict: Approve, entries: [])'.format(
            planned["result"]["plan_digest"])
        receipt = run("op", "decide", planned["operation"]["id"], "--decision", "-", stdin=vote)["result"]
        assert receipt["version"] == "4" and receipt["child"] != parent
        checked = subprocess.run([hirsel, "store", "verify", child, "--release", release], capture_output=True, text=True)
        assert checked.returncode == 0 and "0 problems" in checked.stdout, (checked.stdout, checked.stderr)
        assert server.objects == original and all(req[:2] == ("reader", "GET") for req in server.requests)
    print("R7: version 3 remained in the gap, loss above the gap refused, and the independent version 4 base restored", flush=True)
