"""Run with python3 core/tools/test_vopr_failure_groups.py."""
import importlib.machinery
import importlib.util
import json
import subprocess
import sys
from pathlib import Path
import tempfile
import unittest

import sim_campaign as prepare
from vopr_failure_groups import group


SIM_CAMPAIGN_PATH = Path(__file__).resolve().parents[2] / "tools" / "sim-campaign"
loader = importlib.machinery.SourceFileLoader("sim_campaign_tool", str(SIM_CAMPAIGN_PATH))
spec = importlib.util.spec_from_loader(loader.name, loader)
sim_campaign = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sim_campaign)


def red(stderr, law="store@1/store/law", status=101):
    return {"law": law, "seed": 7, "status": status, "stdout": "", "stderr": stderr}


class FailureGroups(unittest.TestCase):
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


class Preparation(unittest.TestCase):
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
