from copy import deepcopy
from datetime import datetime, timedelta, timezone
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

import comparison_units
import receipt
import weekly_board
from bench.units.contract import VERSION


class WeeklyBoard(unittest.TestCase):
    def test_plan_and_window(self):
        plan = subprocess.check_output(
            [sys.executable, Path(weekly_board.__file__), "--plan"], text=True)
        self.assertEqual(plan.count("(3 repeats)"), 353)
        self.assertEqual(plan.count("(7 pairs)"), 28)
        self.assertEqual(plan.count("repeats of 8 requests)"), 20)
        self.assertIn("A1 ext4/256/hardware/matched (1 repeat, 3000 turns/s)", plan)
        self.assertIn("mandelbrot Go W15 (3 repeats)", plan)
        self.assertIn("--weekly-units --units-receipt", plan)
        self.assertEqual(plan.count(" /plaintext\n"), 3)
        start = datetime(2026, 11, 28, 22, tzinfo=timezone.utc)
        end = datetime(2026, 11, 29, 2, 45, tzinfo=timezone.utc)
        self.assertEqual(weekly_board.cutoff(start), end)
        self.assertEqual(weekly_board.cutoff(end - timedelta(seconds=1)), end)
        for now in (start - timedelta(seconds=1), end):
            with self.assertRaisesRegex(ValueError, "weekly measurement window"):
                weekly_board.cutoff(now)

    def test_weekly_export_requires_distinct_admitted_repeats(self):
        fixture = Path(__file__).with_name("fixtures") / "fair"
        archive = receipt.read(fixture / "fair.json")
        source, = archive["rows"]
        key = "Regimes", source["row"]
        coverage = [{"goal": goal, "row": row, "status": "ok" if (goal, row) == key else "rejected",
                     "issues": {"export": "no evidence"}, "commit": archive["commit"]}
                    for goal, row in comparison_units.expected(weekly=True)]
        config = archive["config"] | {
            "mode": "weekly", "families": ["a1", "regimes"], "repeats": 3,
            "regimes": ["fanin", "pipeline", "checkbox"], "measurement_memory_limit_bytes": 2 << 30,
        }
        original = receipt.read(next(fixture.glob("regime-samples/*/pair.json")))
        row = deepcopy(source)
        for sample in row["samples"].values():
            sample["arms"] = []
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            for repeat in range(3):
                pair = deepcopy(original)
                pair["hold"]["load_start"] = 8
                for field in ("started", "finished"):
                    instant = datetime.fromisoformat(pair["hold"][field]) + timedelta(seconds=4 * repeat)
                    pair["hold"][field] = instant.isoformat()
                for arm, sample in pair["samples"].items():
                    raw = fixture / sample["path"] / "stdout"
                    sample["path"] = f"repeat-{repeat}/{arm}"
                    out = root / sample["path"]
                    out.mkdir(parents=True)
                    (out / "stdout").write_bytes(raw.read_bytes())
                    row["samples"][arm]["arms"].append(sample)
                receipt.write(root / f"repeat-{repeat}/pair.json", pair)
            rows = [row if (item["goal"], item["row"]) == key else item for item in coverage]
            export = archive | {"version": VERSION, "config": config, "expected_rows": coverage, "rows": rows}
            path = root / "fair.json"
            receipt.write(path, export)
            rows = comparison_units.import_rows(path, archive["commit"], weekly=True)
            self.assertEqual(len(rows), 34)
            seen = next(row for row in rows if row["row"] == "checkbox/native/w1")
            self.assertEqual(seen["status"], "ok")
            row["samples"]["hirsel"]["arms"][1] = row["samples"]["hirsel"]["arms"][0]
            receipt.write(path, export)
            rows = comparison_units.import_rows(path, archive["commit"], weekly=True)
            seen = next(row for row in rows if row["row"] == "checkbox/native/w1")
            self.assertEqual(seen["issues"]["export"], "hirsel regime repeat reuses a hold")

    def test_compare_paired_aa_is_trend_only_and_requires_all_pairs(self):
        key = "awfy/Bounce"
        row = {
            "key": key, "suite": "awfy", "mode": "release", "flags": ["--release"],
            "argv": ["./hirsel-harness", "Bounce"], "affinity": [16],
            "marker": "verified\n", "stdout": "verified\n",
            **{field: "a" * 64 for field in receipt.CANDIDATE_HASHES},
            **receipt.data_fixture({"inputs": "b" * 64}),
            "sample": {"status": "ok", "verified": True, "instructions": 100, "cycles": 100,
                       "pmu_times_ns": {event: [[1000, 1000]] for event in ("instructions:u", "cycles:u")}},
        }
        pair = {"order": ["base", "candidate"], "base": row, "candidate": row}
        baseline = {"rows": {key: [pair] * 7}, "comparison": "A/A", "failures": {},
                    "cpu": 16, "pairs": 7, "lock": "/run/user/1000/hirsel-board.lock"}
        candidate = deepcopy(baseline)
        candidate["rows"][key][0]["base"]["sample"]["instructions"] = 300
        with tempfile.TemporaryDirectory() as scratch:
            before, after = (Path(scratch) / name for name in ("before.json", "after.json"))
            receipt.write(before, baseline)
            command = [sys.executable, Path(receipt.__file__), "compare", before, after, "--aa"]
            receipt.write(after, candidate)
            result = subprocess.run(command, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("100.0 -> 300.0 (+200.000%)", result.stdout)
            candidate["rows"][key].pop()
            receipt.write(after, candidate)
            result = subprocess.run(command, capture_output=True, text=True)
            self.assertEqual(result.returncode, 2)
            self.assertIn("incomplete A/A pairs", result.stderr)

    def test_change_points_require_seven_observations(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            script = [sys.executable, weekly_board.TOOLS / "board-history.py"]

            def record(index):
                run = root / "weekly-runs" / f"week-{index}"
                run.mkdir(parents=True)
                instant = datetime(2026, 11, 28, 22, tzinfo=timezone.utc) + timedelta(weeks=index)
                report = {"metrics": {"shift/rate": 10 if index < 4 else 20, "flat/rate": 10},
                          "started": instant.isoformat(), "finished": (instant + timedelta(hours=1)).isoformat(),
                          "commit": "a" * 40, "complete": True, "contract": {"version": 1}}
                receipt.write(run / "weekly.json", report)
                return run

            for index in range(6):
                record(index)
            subprocess.run([*script, "append", "--base", root], check=True)
            self.assertEqual(subprocess.check_output([*script, "detect", "--base", root], text=True), "")
            current = record(6)
            subprocess.run([*script, "append", "--base", root, current], check=True)
            detected = subprocess.check_output(
                [*script, "detect", "--base", root, "--run", current.name], text=True)
            changes = [json.loads(line) for line in detected.splitlines()]
            self.assertEqual([(row["row"], row["kind"], row["before"], row["after"]) for row in changes],
                             [("shift", "change-point", 10, 20)])
