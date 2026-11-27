"""Deploy the known release of a private test host through the installed operator CLI."""
import json
import os
from pathlib import Path
import socket
import struct
import subprocess


def listening(operator):
    with socket.socket(socket.AF_UNIX) as peer:
        try:
            peer.connect(str(operator))
            return True
        except OSError:
            return False


def pin_host(state, path):
    """The fixture started this host and owns its private state directory."""
    with socket.socket(socket.AF_UNIX) as peer:
        peer.connect(str(state / "operator.sock"))
        peer.sendall(struct.pack("<II", 4, 10))  # Identify
        with peer.makefile("rb") as reply:
            if struct.unpack("<I", reply.read(4))[0] != 20:
                raise ValueError("invalid host identification")
            tag, hi, lo = struct.unpack("<IQQ", reply.read(20))
            if tag != 9:
                raise ValueError("expected catalog identity")
    authority = f"{hi:016x}{lo:016x}"
    path.write_text(f'kind = "host"\nstate = {json.dumps(str(state))}\nauthority = "{authority}"\nuid = {os.geteuid()}\n')
    return authority


def group_spec(path, name, address, store, workdir, recording=None):
    fields = dict(name=name, tenant="operator", listen=address, store=str(store), workdir=str(workdir))
    items = [f"{k}: {json.dumps(v)}" for k, v in fields.items()]
    items += ["trusted: true", "grants: []", "package_roots: []", "args: []"]
    if recording:
        items.append(f"recording: Some(Recording(dir: {json.dumps(str(recording))}, retention_days: 30))")
    path.write_text("host_v1.Group(" + ", ".join(items) + ")\n")


def submit_fixture(command, *, run=subprocess.run, **kwargs):
    """Approve only this fixture's creation and first-release admission."""
    pin = Path(command[command.index("--context") + 1])
    kwargs.setdefault("env", dict(os.environ, XDG_STATE_HOME=str(pin.parent / "operator-state")))
    result = run(command, capture_output=True, text=True, **kwargs)
    output = json.loads(result.stdout)
    seen = set()
    while result.returncode == 75 and output["outcome"]["kind"] == "AwaitDecision":
        plan = output["result"]
        facts = plan["facts"]
        stage = facts["stage"]
        if "--create" not in command or stage in seen or stage not in ("creation", "admission"):
            raise AssertionError(f"unexpected hold: {plan}")
        if stage == "admission" and (facts["root"] != command[command.index("--recipe") + 1]
                                     or facts["approvals"] is not None):
            raise AssertionError(f"unexpected genesis admission: {plan}")
        seen.add(stage)
        doc = f'host_v1.Decision(plan_digest: "{plan["plan_digest"]}", verdict: Approve, entries: [])'
        result = run([command[0], "op", "decide", output["operation"]["id"], "--context",
                      command[command.index("--context") + 1], "--decision", "-", "--json", "--wait"],
                     input=doc, capture_output=True, text=True, **kwargs)
        output = json.loads(result.stdout)
    if result.returncode:
        raise AssertionError(f"operator returned {result.returncode}: {output}\n{result.stderr}")
    return output

def check_creation_plan(command):
    pin = Path(command[command.index("--context") + 1])
    state = pin.parent / "operator-state"
    env = dict(os.environ, XDG_STATE_HOME=str(state))
    def call(args, code, **kwargs):
        result = subprocess.run(args, env=env, capture_output=True, text=True, **kwargs)
        assert result.returncode == code, (result.stdout, result.stderr)
        return json.loads(result.stdout), result.stderr
    plan, log = call([*command, "--plan"], 0)
    assert plan["contract"] == "plan" and plan["outcome"]["effect"] == "none", plan
    operation = plan["operation"]["id"]
    assert operation in log and plan["result"]["facts"]["stage"] == "creation"
    receipt = state / "hirsel/operations" / (operation.replace("/", "-") + ".json")
    assert json.loads(receipt.read_text())["intent_digest"] == plan["result"]["intent_digest"]
    args = [command[0], "op", "get", operation, "--context", str(pin), "--json"]
    held, _ = call([*args, "--wait"], 75)
    assert held["outcome"]["kind"] == "AwaitDecision", held
    decide = [command[0], "op", "decide", operation, "--context", str(pin), "--json"]
    required, _ = call(decide, 75, start_new_session=True)
    assert required["error"]["code"] == "DecisionInputRequired" and required["outcome"]["effect"] == "none"
    doc = f'host_v1.Decision(plan_digest: "{plan["result"]["plan_digest"]}", verdict: Refuse, entries: [])'
    refused, _ = call([*decide, "--decision", "-", "--wait"], 1, input=doc)
    assert refused["result"]["phase"] == "failed", refused
    observed, _ = call(args, 0)
    assert observed["result"]["phase"] == "failed", observed
