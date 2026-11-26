"""The board history should ignore flat counters and short wall-time series."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "board-history.py"


class BoardHistoryTest(unittest.TestCase):
    def test_flat_series_and_seven_sample_wall_time_trend(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)

            def record(day, seconds):
                run = base / "runs" / f"202611{day:02d}T031500Z-fixture" / "board"
                run.mkdir(parents=True)
                data = {"complete": True, "exit_status": 0, "rows": [
                    {"goal": "LangArena", "row": name, "metrics": ["instructions", "seconds"],
                     "samples": {"hirsel": {"status": "ok", "instructions": 1000, "seconds": duration}}}
                    for name, duration in (("flat", 2), ("trend", seconds))
                ]}
                (run / "board.json").write_text(json.dumps(data))
                return run.parent

            def history(command, *args):
                return subprocess.check_output([sys.executable, SCRIPT, command, "--base", base, *args],
                                               text=True)

            for day in range(19, 25):
                record(day, day - 18)
            history("append")
            self.assertEqual(history("detect"), "")
            last = record(25, 7)
            history("append", last)
            history("append", last)
            changes = [json.loads(line) for line in history("detect").splitlines()]
            self.assertEqual([(c["row"], c["kind"], c["before"], c["after"]) for c in changes],
                             [("trend", "trend", 1, 7)])


if __name__ == "__main__":
    unittest.main()
