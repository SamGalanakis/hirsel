"""Run with python3 core/tools/test_vopr_failure_groups.py."""
import importlib.machinery
import importlib.util
import gzip
import json
import subprocess
import sys
from pathlib import Path
import tempfile
import unittest

import sim_campaign as prepare
from vopr_failure_groups import group, publish


SIM_CAMPAIGN_PATH = Path(__file__).resolve().parents[2] / "tools" / "sim-campaign"
loader = importlib.machinery.SourceFileLoader("sim_campaign_tool", str(SIM_CAMPAIGN_PATH))
spec = importlib.util.spec_from_loader(loader.name, loader)
sim_campaign = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sim_campaign)


def red(stderr, law="store@1/store/law", status=101):
    return {"law": law, "seed": 7, "status": status, "stdout": "", "stderr": stderr}


class FailureGroups(unittest.TestCase):
    def test_nightly_requires_two_canary_results_in_every_batch(self):
        with tempfile.TemporaryDirectory() as directory:
            run = Path(directory)
            batch = run / "nightly-001.jsonl.gz"
            even = {**red("", prepare.CANARY, 0), "seed": 8, "tape_replay": {"status": 0}}
            odd = {**red("error: ()\n", prepare.CANARY, 1), "tape_replay": {"status": 0}}
            with gzip.open(batch, "wt") as stream:
                for row in (even, odd):
                    print(json.dumps(row), file=stream)
            self.assertEqual(publish(run, require_canary=True), 0)
            self.assertEqual((run / "failure-groups.jsonl").read_text(), "")
            with gzip.open(run / "nightly-002.jsonl.gz", "wt") as stream:
                print(json.dumps(odd), file=stream)
            self.assertEqual(publish(run, require_canary=True), 2)
            self.assertIn("canary_missing", (run / "failure-groups.jsonl").read_text())

    def test_partition(self):
        # Each case: rows that must share one group, or rows that must split.
        same = [
            # One Fail site whose message embeds a seed and an offset, plus a
            # quoted value and " at " inside the message.
            [red('Fail("seed 41 offset 3 key \\"a1\\" at x.hirsel:1:1") at store/journal.hirsel:88:9\n'),
             red('Fail("seed 977 offset 12 key \\"b22\\" at x.hirsel:1:1") at store/journal.hirsel:88:9\n')],
            # A non-cause diagnostic whose numbers vary.
            [red("bounded drain: 12 frames left after 300 ticks\n", status=1),
             red("bounded drain: 4 frames left after 1000 ticks\n", status=1)],
        ]
        split = [
            # Bare causes carry no message: the line is the only thing telling them apart.
            [red("Bounds at std@1/object.hirsel:2283:22\n"), red("Bounds at std@1/object.hirsel:2309:22\n")],
            # Two Fail sites in one file that share their text.
            [red('Fail("short read") at store/journal.hirsel:88:9\n'),
             red('Fail("short read") at store/journal.hirsel:140:9\n')],
            # Same site, different kind or law.
            [red("Overflow at a.hirsel:3:4\n"), red("Bounds at a.hirsel:3:4\n")],
            [red("Unwrap at a.hirsel:3:4\n", law="x"), red("Unwrap at a.hirsel:3:4\n", law="y")],
        ]
        for rows in same:
            self.assertEqual(len({group(row) for row in rows}), 1, rows)
        for rows in split:
            self.assertEqual(len({group(row) for row in rows}), len(rows), rows)

    def test_a_replay_past_the_bound_is_a_timeout_and_the_night_moves_on(self):
        with tempfile.TemporaryDirectory() as held:
            root = Path(held)
            prepared = root / "prepared"
            prepared.mkdir()
            slow = prepared / "slow-law"
            fast = prepared / "fast-law"
            slow.write_text("#!/bin/sh\nexec sleep 5\n")
            fast.write_text("#!/bin/sh\nexit 0\n")
            slow.chmod(0o755)
            fast.chmod(0o755)
            (prepared / "manifest.json").write_text(json.dumps([
                {"law": "sim/slow", "executable": "slow-law", "seeds": 1},
                {"law": "sim/fast", "executable": "fast-law", "seeds": 1},
            ]))
            prior = root / "nights" / "2026-11-24T000000Z-prior"
            run = root / "nights" / "2026-11-25T000000Z-now"
            prior.mkdir(parents=True)
            run.mkdir()
            (prior / "failure-groups.jsonl").write_text(
                json.dumps({"group": ["sim/slow", "seed_red", "exit 1"], "seed": 7}) + "\n"
                + json.dumps({"group": ["sim/fast", "seed_red", "exit 1"], "seed": 8}) + "\n")

            # Returning at all is what lets the night go on to its fresh seeds.
            still_red = sim_campaign.reds_first(prepared / "manifest.json", run, False, seed_timeout=0.2)

            self.assertEqual(still_red, 1)

            rows = [json.loads(line) for line in (run / "reds-first.jsonl").read_text().splitlines()]
            self.assertEqual([row["seed"] for row in rows], [7, 8])
            self.assertEqual(rows[0]["status"], 124)
            self.assertEqual(rows[0]["timed_out"], True)
            self.assertEqual(rows[0]["failure_class"], "timeout")
            self.assertEqual(rows[1]["status"], 0)


LAW = """#!{python}
import os, sys
seed = int(os.environ["HIRSEL_TEST_SIM_SEED"])
raw = os.environ.get("HIRSEL_COVERAGE")
if raw:
    # The runtime opens the raw file create_new, and a second open fails the run.
    with open(raw, "xb") as out:
        end = b"" if seed == {unfinished} else (0x45444F4E).to_bytes(4, "little")
        out.write(b"HIRSELCV" + bytes(40) + end + b"{law}" + b"\\0" + str(seed).encode())
sys.exit(seed == {red})
"""

# Pairs each raw file with the one map whose stem begins its name, as
# `hirsel coverage merge` does, and refuses a raw file with no end mark.
TOOL = """#!{python}
import json, sys
args = sys.argv[3:]
listing = [line for line in open(args[0][1:]).read().splitlines()]
maps = [args[i + 1] for i, arg in enumerate(args) if arg == "--map"]
runs = []
for raw in listing:
    name = raw.rsplit("/", 1)[1]
    paired = [m for m in maps if name.startswith(m.rsplit("/", 1)[1].removesuffix(".covmap"))]
    data = open(raw, "rb").read()
    if len(paired) != 1 or data[48:52] != (0x45444F4E).to_bytes(4, "little"):
        sys.exit(f"{{raw}}: unpaired or unfinished")
    law, seed = data[52:].split(b"\\0")
    runs.append([open(paired[0]).read(), law.decode(), int(seed)])
open(args[args.index("-o") + 1], "w").write(json.dumps(sorted(runs)))
"""

DISPATCHER = """#!{python}
import json, shutil, subprocess, sys
from pathlib import Path
manifest = Path(sys.argv[-1])
job, = json.loads(manifest.read_text())
bundle = manifest.parent / job["bundle"]
status = subprocess.call(job["command"], shell=True, cwd=bundle)
for spec in job.get("fetch", []):
    remote, local = spec.split(":")
    shutil.copytree(bundle / remote, manifest.parent / local / Path(remote).name)
sys.exit(status)
"""


class Coverage(unittest.TestCase):
    def test_every_seed_writes_its_own_raw_file_merged_against_its_own_map_by_group(self):
        # Three laws in two groups, two seeds each: the second seed of each law
        # fails (and is diagnosed by a rerun), and law c's first seed never
        # finishes its raw file. Each profile must hold exactly its group's
        # finished runs, each read against the map of the law that ran it.
        python = sys.executable
        with tempfile.TemporaryDirectory() as held:
            root = Path(held)
            entries = []
            for index, (law, group) in enumerate((("a", "one"), ("b", "one"), ("c", "two"))):
                stem = f"{index}" * 64
                executable = root / stem
                executable.write_text(LAW.format(python=python, law=law, red=2 + 2 * index,
                                                 unfinished=5 if law == "c" else -1))
                executable.chmod(0o755)
                (root / f"{stem}.covmap").write_text(law)
                entries.append({"law": law, "executable": stem, "seeds": 2,
                                "coverage": {"group": group, "map": f"{stem}.covmap", "tool": "hirsel"}})
            for name, body in (("hirsel", TOOL), ("dispatcher", DISPATCHER)):
                (root / name).write_text(body.format(python=python))
                (root / name).chmod(0o755)
            (root / "manifest.json").write_text(json.dumps(entries))
            ran = subprocess.run([str(SIM_CAMPAIGN_PATH), "batch", "night", str(root / "manifest.json"),
                                  "--ledger", str(root / "seeds.sqlite"), "--results", str(root / "night.jsonl"),
                                  "--dispatcher", str(root / "dispatcher")],
                                 cwd=root, capture_output=True, text=True)
            self.assertEqual(ran.returncode, 1, ran.stderr)
            rows = [json.loads(line) for line in (root / "night.jsonl").read_text().splitlines()]
            self.assertEqual([(row["law"], row["seed"], row["status"]) for row in rows],
                             [("a", 1, 0), ("a", 2, 1), ("b", 3, 0), ("b", 4, 1), ("c", 5, 0), ("c", 6, 1)])
            self.assertEqual([row["seed"] for row in rows if row.get("same_build_nondeterminism")], [])
            profiles = root / "night.coverage"
            self.assertEqual(json.loads((profiles / "one.json").read_text()),
                             [["a", "a", 1], ["a", "a", 2], ["b", "b", 3], ["b", "b", 4]])
            self.assertEqual(json.loads((profiles / "two.json").read_text()), [["c", "c", 6]])
            self.assertIn("coverage two: 1 unobserved runs: c seed 5", ran.stderr)

    def test_history_violation_retains_sem8_tape(self):
        python = sys.executable
        law_script = f"""#!{python}
import os, sys
if len(sys.argv) >= 4 and sys.argv[1] == "--hirsel-replay-recording":
    data = open(sys.argv[3], "rb").read()
    sys.exit(0 if data.startswith(b"TAPE:") else 1)
seed = int(os.environ["HIRSEL_TEST_SIM_SEED"])
record = os.environ.get("HIRSEL_RECORD")
if record:
    with open(record, "wb") as out:
        out.write(f"TAPE:{{seed}}".encode())
if seed == 2:
    sys.stderr.write(f'Fail("sim.history violation seed={{seed}} class=output-gate expected=7 observed=5") at std@1/sim/history.hirsel:153:5\\n')
    sys.exit(101)
"""
        with tempfile.TemporaryDirectory() as held:
            root = Path(held)
            for name in ("law-bin", "law-rec"):
                script = root / name
                script.write_text(law_script)
                script.chmod(0o755)
            dispatcher = root / "dispatcher"
            dispatcher.write_text(DISPATCHER.format(python=python))
            dispatcher.chmod(0o755)
            (root / "manifest.json").write_text(json.dumps([
                {"law": "runner@1/runner/test_sim_release_uses_strongest",
                 "executable": "law-bin", "recording": "law-rec", "seeds": 2},
            ]))
            ran = subprocess.run([str(SIM_CAMPAIGN_PATH), "batch", "night", str(root / "manifest.json"),
                                  "--ledger", str(root / "seeds.sqlite"), "--results", str(root / "night.jsonl"),
                                  "--dispatcher", str(dispatcher)],
                                 cwd=root, capture_output=True, text=True)
            self.assertEqual(ran.returncode, 1, ran.stderr)
            rows = [json.loads(line) for line in (root / "night.jsonl").read_text().splitlines()]
            self.assertEqual([(row["seed"], row["status"], row["failure_class"]) for row in rows],
                             [(1, 0, None), (2, 101, "history_violation")])
            self.assertNotIn("sem8_tape", rows[0])
            tape_name = rows[1]["sem8_tape"]
            tapes = root / "night.tapes"
            self.assertEqual(sorted(p.name for p in tapes.iterdir()), [tape_name])
            self.assertEqual((tapes / tape_name).read_bytes(), b"TAPE:2")
            with gzip.open(root / "night.jsonl.gz", "wt") as stream:
                for row in rows:
                    print(json.dumps(row), file=stream)
            self.assertEqual(publish(root), 0)
            groups = [json.loads(line) for line in (root / "failure-groups.jsonl").read_text().splitlines()]
            self.assertEqual(len(groups), 1)
            self.assertEqual(groups[0]["group"], [
                "runner@1/runner/test_sim_release_uses_strongest", "seed_red", "history_violation"
            ])
            self.assertEqual(groups[0]["seed"], 2)
            self.assertEqual(groups[0]["sem8_tape"], tape_name)


class Preparation(unittest.TestCase):
    def test_board_keeps_good_laws_when_a_contract_is_refused_or_has_no_binary(self):
        names = ["std@1/tests/exec/run_until_escalates",
                 "std@1/tests/http_connection/http_request_scratch",
                 "std@1/tests/net/public_connect_preflight"]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            compiler = root / "hirsel"
            compiler.write_text(f"#!{sys.executable}\n" + r'''
import hashlib, json, pathlib, sys
args = sys.argv
assert args[args.index("--test-timeout") + 1] == "300"
definitions = [args[i + 1] for i, arg in enumerate(args) if arg == "--exact"]
if any("/exec/" in name for name in definitions):
    print(json.dumps({"accepted": False, "diagnostics": [{"primary": {"module": "contract-0.hirsel"}}]}))
    sys.exit(1)
out = pathlib.Path(args[args.index("-o") + 1])
out.mkdir()
rows = []
for name in definitions:
    if "http_request_scratch" in name and "--laws" in args:
        rows.append({"definition": name, "source": "executed", "stopped": "link refused"})
        continue
    parts = (b"hirsel.text.v1", name.encode())
    stem = hashlib.sha256(b"".join(len(part).to_bytes(8, "little") + part for part in parts)).hexdigest()
    (out / stem).touch()
    if "--coverage" in args:
        (out / (stem + ".covmap")).touch()
    rows.append({"definition": name, "source": "executed", "passed": True})
print(json.dumps({"tests": rows}))
''')
            compiler.chmod(0o755)
            work = root / "prepared"
            command = [sys.executable, prepare.__file__, "--compiler", str(compiler),
                       "--work", str(work), "--prepare-results", str(root), "--seeds", "2"]
            for name in names:
                command.extend(("--law", name))
            prepared = subprocess.run(command, capture_output=True, text=True)
            self.assertEqual(prepared.returncode, 1, prepared.stderr)
            laws = json.loads((work / "manifest.json").read_text())
            self.assertEqual([law["law"] for law in laws], names[2:])
            self.assertFalse(laws[0]["defer_probe_gate"])
            self.assertEqual(publish(root), 3)
            failures = [json.loads(line) for line in (root / "failure-groups.jsonl").read_text().splitlines()]
            self.assertEqual([row["group"][:2] for row in failures], [[name, "prepare_red"] for name in names[:2]])
            for row in failures:
                report = Path(row["reason"].rsplit("; see ", 1)[1])
                self.assertTrue(report.is_relative_to(root / "prepared/contracts"))
                self.assertTrue(report.is_file())
            self.assertIn("link refused", failures[1]["reason"])

    def test_every_selected_contract_law_builds_a_campaign_entry(self):
        # A law the inventory selects must never stop preparation as an
        # unsupported fixture (NET-18 public_connect_preflight carries only a
        # file-level test-env, which the seeded base run does not need).
        contracts = prepare.selected_laws()["contracts"]
        with tempfile.TemporaryDirectory() as held:
            _, entries = prepare.contract_inputs(contracts, Path(held))
        self.assertEqual(entries.keys(), contracts.keys())


if __name__ == "__main__":
    unittest.main()
