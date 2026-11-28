"""L-A9. Back up a serving Place, admit its restored child, and write through it."""
from contextlib import contextmanager
import json
import os
from pathlib import Path
import re
import socket
import subprocess
import sys
import tempfile
import time
from urllib.request import urlopen

sys.path.insert(0, str(Path("core/tools").resolve()))
from operator_fixture import group_spec, listening, pin_host, submit_fixture


def address():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return f"127.0.0.1:{sock.getsockname()[1]}"


@contextmanager
def launcher(hirsel, state, env):
    host = subprocess.Popen([hirsel, "launcher", "--control", state / "launcher.sock", "--state", state,
                             "--lifecycle", "leader", "--operator-listen", state / "operator.sock",
                             "--admission", "tools/tests/fixtures/host-admission.hcfg", "--max-places", "4",
                             "--platform-host", hirsel.parent / "libexec/hirsel/platform_main"], env=env)
    try:
        start = time.monotonic()
        while not listening(state / "operator.sock"):
            assert host.poll() is None and time.monotonic() - start < 60
            time.sleep(.05)
        yield
    finally:
        if host.poll() is None:
            assert subprocess.check_output(["ps", "-o", "comm=", "-p", str(host.pid)], text=True).strip() == "hirsel"
            host.terminate()
        host.wait(timeout=15)


def drill(hirsel, work, recipe, code):
    state = work / "host"
    state.mkdir(mode=0o700)
    pin = work / "context.hcfg"
    env = dict(os.environ, XDG_STATE_HOME=str(work / "operator"), TMPDIR=str(work))

    def run(*args, code=0, stdin=None):
        p = subprocess.run([hirsel, *args], env=env, input=stdin, text=True, capture_output=True)
        assert p.returncode == code, (args, p.returncode, p.stdout, p.stderr)
        if "--json" in args:
            out = json.loads(p.stdout)
            assert out["exit"] == code and out["v"] == 1, out
            return out
        return p.stdout

    def decide(plan, *, host=False, code=0):
        args = ["--context", pin, "--wait"] if host else []
        decision = 'host_v1.Decision(plan_digest: "{}", verdict: Approve, entries: [])'.format(
            plan["result"]["plan_digest"])
        return run("op", "decide", plan["operation"]["id"], *args, "--decision", "-", "--json", stdin=decision, code=code)

    def retire(name, incarnation):
        plan = run("host", "retire", "--context", pin, "--group", name + "@" + incarnation, "--plan", "--json")
        assert plan["outcome"]["effect"] == "none" and plan["result"]["subject"]["generation"] == incarnation
        assert decide(plan, host=True)["result"]["phase"] == "retired"
        assert run("op", "get", plan["operation"]["id"], "--context", pin, "--json")["result"]["phase"] == "retired"

    def spec(name, addr, store):
        path = work / (name + ".hcfg")
        store.parent.mkdir(exist_ok=True)
        group_spec(path, name, addr, store, store.parent)
        return path

    def ready(addr):
        start = time.monotonic()
        while time.monotonic() - start < 60:
            try:
                with urlopen("http://" + addr + "/hirsel/status", timeout=2) as response:
                    status = json.load(response)
                    if status["phase"] == "ready":
                        return state / "objects/images" / status["digest"]
            except OSError:
                pass
            time.sleep(.05)
        raise AssertionError(f"{addr} failed to become ready")

    def write(addr, image, key):
        result = json.loads(run("call", image, "--url", f"ws://{addr}/hirsel", "--n-place", "1048576",
                                "--timeout", "10", "--", "main/write_row", "--arg",
                                json.dumps([key, ("%02x" % key) * 100])))
        assert result["tag"] == "Returned" and result["value"]["tag"] == "Ok", result

    with launcher(hirsel, state, env):
        pin_host(state, pin)
        original = work / "original/live"
        old_addr = address()
        registration = spec("original", old_addr, original)
        original.mkdir()
        installed = submit_fixture([str(hirsel), "deploy", "--recipe", recipe, "--code-store", str(code),
                                    "--context", str(pin), "--create", str(registration), "--wait", "--json"], env=env)
        image = ready(old_addr)
        write(old_addr, image, 1)
        run("store", "backup", original, work / "held", code=75)
        backup = work / "online"
        run("store", "backup", "--state", state, "--group", "original", backup)
        child = work / "restored/live"
        new_addr = address()
        registration = spec("restored", new_addr, child)
        plan = run("store", "restore", "--from", backup, "--target", "latest", "--to", child, "--plan", "--json")
        receipt = decide(plan)
        saved = receipt["result"]
        assert saved["parent"] != saved["child"] and saved["activation"] != "0"
        run("store", "verify", child, "--release", image)
        external = work / "receipt.json"
        saved_receipt = json.dumps(receipt)
        external.write_text(saved_receipt)
        adopt = ["host", "adopt", "@" + str(external), "--context", pin, "--as", registration, "--plan", "--json"]
        external.write_text(json.dumps(dict(receipt, result=dict(saved, child=saved["parent"]))))
        run(*adopt, code=2)
        external.write_text(saved_receipt)
        plan = run(*adopt)
        assert plan["outcome"]["effect"] == "none"
        proof = plan["result"]["recovery"]
        assert all(proof[key] == saved[key] for key in ("child", "parent", "destination", "attempt", "boundary",
                                                        "version", "activation", "release_binding"))
        assert plan["result"]["facts"]["stage"] == "creation"
        with socket.socket() as sock:
            assert sock.connect_ex(("127.0.0.1", int(new_addr.rsplit(":", 1)[1]))) != 0
        admission = decide(plan, host=True, code=75)
        assert admission["result"]["facts"]["stage"] == "admission"
        accepted = decide(admission, host=True)
        assert accepted["result"]["phase"] == "installed"
        inc = accepted["target"]["incarnation"]
        assert inc != installed["target"]["incarnation"]
        status = run("status", "--group", "restored@" + inc, "--context", pin, "--json")
        assert status["result"]["store_id"] == proof["child"]
        assert ready(new_addr) == image
        write(new_addr, image, 2)
        second = work / "second"
        run("store", "backup", "--state", state, "--group", "restored", second)
        retire("original", installed["target"]["incarnation"])
        retire("restored", inc)
        run("store", "verify", child, "--release", image)
        reopened = run("store", "inspect", child)
        assert proof["child"] in reopened
        check = work / "check-place/live"
        check.parent.mkdir()
        final = decide(run("store", "restore", "--from", second, "--target", "latest", "--to", check, "--plan", "--json"))
        assert final["result"]["parent"] == proof["child"] and int(final["result"]["version"]) > int(proof["version"])
        # Reopen the saved image with the same immutable service and compare both rows.
        subprocess.run([image, "check-cut"], cwd=check.parent, check=True)
        # A different retained selection or a receipt naming another child
        # must fail when the launcher opens the stopped Directory.
        for name, wrong_id in [("wrong-assembly", False), ("wrong-id", True)]:
            bad_store = work / name / "live"
            bad_spec = spec(name, address(), bad_store)
            bad = decide(run("store", "restore", "--from", second, "--target", "latest",
                             "--to", bad_store, "--plan", "--json"))
            opts = ["--assembly", "0" * 64]
            if wrong_id:
                bad["result"]["child"] = saved["parent"]
                opts = []
            external.write_text(json.dumps(bad))
            bad_plan = run("host", "adopt", "@" + str(external), "--as", bad_spec, "--context", pin,
                           *opts, "--plan", "--json")
            denied = decide(bad_plan, host=True, code=1)
            assert denied["result"]["phase"] == "failed", denied
        print("L-A9 host: accepted and wrote to child", proof["child"], "incarnation", inc,
              "second backup version", final["result"]["version"])


if __name__ == "__main__":
    hirsel = (Path(sys.argv[1]) if len(sys.argv) > 1 else Path(os.environ["HIRSEL_TOOLCHAIN_BUNDLE"]) / "hirsel").resolve()
    with tempfile.TemporaryDirectory(prefix="host-recovery-") as temp:
        work = Path(temp)
        if len(sys.argv) == 4:
            recipe, code = sys.argv[2], Path(sys.argv[3])
        else:
            import shutil
            package = work / "release"
            shutil.copytree("tools/tests/fixtures/store-recovery-release", package)
            code = work / "code"
            args = [hirsel, "build", package, "--entry", "hirsel.ai/recovery_drill@1/main/main",
                    "--emit", "recipe", "--backend", "cranelift", "--code-store", code,
                    "--dir", "live", "--limit", "2147483648", "--n-place", "1048576"]
            for grant in ["Net=Listen 127.0.0.0/8 0-65535", "Clock", "Random", "Tasks", "Process", "Stderr"]:
                args.extend(["--grant", grant])
            recipe = subprocess.check_output(args, text=True, cwd=work).strip()
        assert re.fullmatch("[0-9a-f]{64}", recipe)
        drill(hirsel, work, recipe, code)
