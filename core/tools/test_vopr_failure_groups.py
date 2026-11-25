"""Run with python3 core/tools/test_vopr_failure_groups.py."""
import importlib.machinery
import importlib.util
import json
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
