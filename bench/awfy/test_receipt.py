from copy import deepcopy
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from receipt import make_receipt


class InstructionGuard(unittest.TestCase):
    def test_mandelbrot_injection_and_incomplete_pairs(self):
        # profilesmash-1288 used a checked LangArena row with a 710,000-iteration
        # loop before exec. The counter values below are the measured replay.
        baseline = make_receipt(["arena/CLBG::Mandelbrot"])
        stdout = "CLBG::Mandelbrot checksum=683987290 expected=683987290 status=OK\n"
        baseline["rows"] = [{
            "key": "arena/CLBG::Mandelbrot",
            "suite": "arena",
            "mode": "certified",
            "certified": True,
            "flags": ["--backend", "llvm", "--release"],
            "argv": ["./clbg-64", "one-iteration.json", "CLBG::Mandelbrot"],
            "affinity": [10],
            "compiler_sha256": "a" * 64,
            "runtime_archive_sha256": "b" * 64,
            "fixture_sha256": "c" * 64,
            "marker": stdout,
            "stdout": stdout,
            "sample": {"status": "ok", "verified": True, "instructions": 69_882_571,
                       "pmu_active_percent": {"instructions:u": 100}},
        }]
        with tempfile.TemporaryDirectory() as temp:
            base, candidate = (Path(temp) / name for name in ("base.json", "candidate.json"))
            base.write_text(json.dumps(baseline))

            def compare(value):
                candidate.write_text(json.dumps(value))
                return subprocess.run([sys.executable, Path(__file__).with_name("receipt.py"),
                                       "compare", base, candidate], capture_output=True, text=True)

            self.assertEqual(compare(baseline).returncode, 0)
            injected = deepcopy(baseline)
            injected["rows"][0]["sample"]["instructions"] = 71_302_841
            result = compare(injected)
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertIn("CLBG::Mandelbrot 69,882,571 -> 71,302,841 (+2.032%) FAIL", result.stdout)

            for field, value in (("affinity", [11]), ("compiler_sha256", "d" * 64),
                                 ("argv", ["./clbg-64", "one-iteration.json", "other"])):
                with self.subTest(field=field):
                    changed = deepcopy(baseline)
                    changed["rows"][0][field] = value
                    result = compare(changed)
                    self.assertEqual(result.returncode, 2)
                    self.assertIn(f"identity differs: arena/CLBG::Mandelbrot: {field}", result.stderr)

            missing = deepcopy(baseline)
            missing["rows"].clear()
            result = compare(missing)
            self.assertEqual(result.returncode, 2)
            self.assertIn("missing: ['arena/CLBG::Mandelbrot']", result.stderr)
