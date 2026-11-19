from copy import deepcopy
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

import build
from receipt import build_record, make_receipt, runtime_case


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
            "manifest_sha256": "e" * 64,
            "image_sha256": "f" * 64,
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

            for field, value in (("affinity", [11]), ("runtime_archive_sha256", "d" * 64),
                                 ("argv", ["./clbg-64", "one-iteration.json", "other"])):
                with self.subTest(field=field):
                    changed = deepcopy(baseline)
                    changed["rows"][0][field] = value
                    result = compare(changed)
                    self.assertEqual(result.returncode, 2)
                    self.assertIn(f"identity differs: arena/CLBG::Mandelbrot: {field}", result.stderr)

            def record_build(receipt):
                row, = receipt["rows"]
                row["build_record"] = build_record({
                    "accepted": True, "native": {"linked": True, "step_charging_elided": True},
                    "measurement": {key: row[key] for key in ("compiler_sha256", "runtime_archive_sha256",
                                                              "manifest_sha256", "image_sha256", "flags")},
                })

            paired = deepcopy(injected)
            paired["rows"][0]["compiler_sha256"] = "d" * 64
            record_build(paired)
            result = compare(paired)
            self.assertEqual(result.returncode, 2)
            self.assertIn("base.json: arena/CLBG::Mandelbrot requires a verified build record", result.stderr)

            record_build(baseline)
            base.write_text(json.dumps(baseline))
            result = compare(paired)
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertIn(f"Variable compiler_sha256 {'a' * 64} -> {'d' * 64}", result.stdout)
            self.assertIn("(+2.032%) FAIL", result.stdout)

            paired["rows"][0]["runtime_archive_sha256"] = "e" * 64
            result = compare(paired)
            self.assertEqual(result.returncode, 2)
            self.assertIn("candidate.json: arena/CLBG::Mandelbrot requires a verified build record", result.stderr)

            record_build(paired)
            result = compare(paired)
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertIn(f"Paired runtime_archive_sha256 {'b' * 64} -> {'e' * 64}", result.stdout)

            paired["rows"][0]["flags"] += ["--debug-lines", "on"]
            record_build(paired)
            result = compare(paired)
            self.assertEqual(result.returncode, 2)
            self.assertIn("identity differs: arena/CLBG::Mandelbrot: flags", result.stderr)

            missing = deepcopy(baseline)
            missing["rows"].clear()
            result = compare(missing)
            self.assertEqual(result.returncode, 2)
            self.assertIn("missing: ['arena/CLBG::Mandelbrot']", result.stderr)


class ProfileSelfCosts(unittest.TestCase):
    def test_excludes_call_edges_and_keeps_inlined_line(self):
        from profile_diff import self_counts

        capture = """positions: instr line
events: Ir
ob=/release/awfy
fl=/src/mandelbrot.hirsel
fn=mandelbrot
0x100 31 8
cob=/release/awfy
cfi=/src/helper.hirsel
cfn=helper
calls=8 0x200 9
0x103 31 32
fi=/src/helper.hirsel
0x108 9 8
fe=/src/mandelbrot.hirsel
0x10b 32 8
fl=/src/helper.hirsel
fn=helper
0x200 9 32
totals: 56
"""
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "callgrind.out"
            path.write_text(capture)
            self.assertEqual(self_counts(path), {
                ("/release/awfy", "mandelbrot", 0x100, "/src/mandelbrot.hirsel", 31): 8,
                ("/release/awfy", "mandelbrot", 0x108, "/src/helper.hirsel", 9): 8,
                ("/release/awfy", "mandelbrot", 0x10b, "/src/mandelbrot.hirsel", 32): 8,
                ("/release/awfy", "helper", 0x200, "/src/helper.hirsel", 9): 32,
            })
            path.write_text(capture.replace("totals: 56", "totals: 88"))
            with self.assertRaises(ValueError):
                self_counts(path)


class NativeProvenance(unittest.TestCase):
    def test_runtime_receipt_refuses_a_replaced_binary(self):
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            compiler = directory / "compiler"
            runtime = directory / "runtime"
            binary = directory / "hirsel-harness"
            manifest = directory / "hirsel-inputs.json"
            compiler.write_bytes(b"compiler")
            runtime.write_bytes(b"runtime")
            binary.write_bytes(b"linked image")
            manifest.write_text('{"packages": []}')
            toolchain = {"compiler_sha256": build.sha256(compiler), "runtime_archive_sha256": build.sha256(runtime)}
            command = [compiler, "build", "--release", "--runtime", runtime, "--inputs", manifest, "-o", binary]
            provenance = build.native_provenance(command, manifest)
            build.write_native_report(binary, json.dumps({
                "accepted": True, "native": {"linked": True, "executable": str(binary)},
            }), provenance)

            def row():
                return runtime_case("awfy", "Mandelbrot", directory, [binary, "Mandelbrot", "1", "500"],
                                    b"verified\n", {"manifest_sha256": build.manifest_sha256(manifest)}, toolchain)

            self.assertEqual(row()["flags"], ["--release", "--runtime", "runtime", "--inputs", "manifest", "-o", "image"])
            binary.write_bytes(b"replacement image")
            with self.assertRaisesRegex(ValueError, "image_sha256 differs from the build"):
                row()
