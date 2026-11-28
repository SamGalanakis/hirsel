"""Nightly receipts retain both campaign evidence and failures before dispatch."""

import gzip
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from sim_campaign import CANARY


class NightlyReceipt(unittest.TestCase):
    def test_negative_controls_reject_survival_and_infrastructure_errors(self):
        from vopr_nightly_negative import run_controls

        def outcome(status, stderr=""):
            return subprocess.CompletedProcess([], status, "", stderr)

        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / "negative.jsonl"
            checks = {
                "expected": lambda: (outcome(33, "store phase read: status 33\n"), True),
                "survived": lambda: (outcome(0), False),
                "wrong red": lambda: (outcome(120, "cut not reached\n"), False),
                "unstarted": lambda: (_ for _ in ()).throw(FileNotFoundError("missing executable")),
            }
            self.assertEqual(run_controls(log, checks), 1)
            rows = [json.loads(line) for line in log.read_text().splitlines()]
            self.assertEqual([(row["oracle"], row["rejected"]) for row in rows],
                             [("expected", True), ("survived", False), ("wrong red", False),
                              ("unstarted", False)])
            self.assertIsNone(rows[-1]["exit_status"])

    def test_frequency_gate_includes_a_never_visited_rare_probe(self):
        from sim_campaign import selected_laws
        from vopr_nightly_receipt import frequency_missed, summarize

        declared = {}
        selected_laws(probes=declared)
        law = "store@1/store/test_l69_random_arm"
        self.assertTrue(declared[law]["crash before the rename"])
        self.assertFalse(declared[law]["fork"])
        self.assertTrue(frequency_missed(1, 20_000))
        self.assertFalse(frequency_missed(2, 20_000))
        self.assertFalse(frequency_missed(1, 19_999))
        with tempfile.TemporaryDirectory() as directory:
            run = Path(directory)
            manifest = run / "manifest.json"
            manifest.write_text(json.dumps([{"build": "commit", "law": law}]))
            (run / "probe-declarations.json").write_text(json.dumps({
                law: {"crash before the rename": True, "fork": False},
            }))
            for batch, calls in enumerate((0, 10), 1):
                with gzip.open(run / f"{batch}.jsonl.gz", "wt") as stream:
                    print(json.dumps({"build": "commit", "law": law, "seed": batch,
                                      "status": 0, "witnesses": {"fork": calls}}), file=stream)
            summary = summarize(run, manifest, "commit")
            self.assertEqual(summary["probes_missed"], [(law, "crash before the rename")])
            rare, common = summary["probes"]
            self.assertEqual((rare["runs"], rare["hits"], rare["severity"]), (2, 0, 30))
            self.assertEqual((common["runs"], common["hits"], common["severity"]), (2, 1, None))

    def test_reach_requires_seed_results_somewhere_in_the_night(self):
        from vopr_nightly_receipt import summarize

        law = "store@1/store/test_model"
        for rows, deferred, missed in (
            ([{"kind": "batch_deferred", "campaign": "night-001"}], 1, []),
            ([], 0, []),
            ([{"build": "commit", "law": CANARY, "seed": 1, "status": 1,
               "stdout": "", "stderr": "error: ()\n"}], 0, [(law, "64 or more commands")]),
        ):
            with self.subTest(rows=rows), tempfile.TemporaryDirectory() as directory:
                run = Path(directory)
                manifest = run / "manifest.json"
                manifest.write_text(json.dumps([{"build": "commit", "law": name}
                                                for name in (law, CANARY)]))
                (run / "probe-declarations.json").write_text(json.dumps({law: {"64 or more commands": False}}))
                with gzip.open(run / "night-001.jsonl.gz", "wt") as stream:
                    for row in [{"build": "commit", "law": law, "seed": 0, "status": 0}, *rows]:
                        print(json.dumps(row), file=stream)
                summary = summarize(run, manifest, "commit")
                self.assertEqual(summary["probes_missed"], missed)
                self.assertEqual(summary["batches_deferred"], deferred)
                self.assertFalse([error for error in summary["evidence_errors"] if "unexpected" in error])

    def test_a_night_that_swept_no_seeds_says_so_instead_of_canary_missing_or_reach_red(self):
        # 2026-11-28: Boat deferred the only batch, and the night read as canary_missing plus
        # 27 runs-0 reach reds. A sweep that never dispatched a batch must read the same way.
        tool = Path(__file__).with_name("vopr_nightly_receipt.py")
        law = "store@1/store/test_model"
        deferral = {"kind": "batch_deferred", "campaign": "night-001"}
        deterministic = {"build": "commit", "law": law, "seed": 0, "status": 101,
                         "stdout": "", "stderr": "deterministic failure", "kind": "deterministic_red"}
        for phase, batch, status, groups in (
            ("sweep-not-run", None, 75, [["boat-dispatch", "sweep_not_run", None]]),
            ("sweep-deferred", [deterministic, deferral], 75,
             [[law, "deterministic_red", "exit 101 deterministic failure"], ["boat-dispatch", "batch_deferred", None]]),
            ("sweep-failed", [deterministic], 2,
             [[law, "deterministic_red", "exit 101 deterministic failure"], ["boat-dispatch", "batch_no_seed_results", None]]),
        ):
            with self.subTest(phase=phase), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                run = root / "night"
                run.mkdir()
                (run / "inventory").write_text(f"{law}\n{CANARY}\n")
                source = root / "revision.c"
                source.write_text("const unsigned char hirsel_code_revision[32] = {1};\n"
                                  "int main(void) { return 0; }\n")
                binary = root / "law"
                subprocess.run(["cc", "-o", binary, source], check=True)
                manifest = root / "manifest.json"
                manifest.write_text(json.dumps([
                    {"build": "commit", "law": name, "executable": str(binary), "recording": str(binary)}
                    for name in (law, CANARY)
                ]))
                (run / "probe-declarations.json").write_text(json.dumps({law: {"64 or more commands": False}}))
                if batch is not None:
                    with gzip.open(run / "night-001.jsonl.gz", "wt") as stream:
                        for row in batch:
                            print(json.dumps(row), file=stream)
                ran = subprocess.run([sys.executable, tool, run, manifest, "commit", phase, str(status)])
                self.assertEqual(ran.returncode, status)
                written = [json.loads(line)["group"] for line in (run / "failure-groups.jsonl").read_text().splitlines()]
                self.assertEqual(written, groups)
                self.assertFalse((run / "complete").exists())
                receipt = json.loads((root / "receipts.ndjson").read_text())
                self.assertEqual((receipt["phase"], receipt["status"], receipt["probes_missed"]), (phase, status, []))

    def test_sometimes_counts_hits_per_seed_across_batches(self):
        from vopr_nightly_receipt import summarize

        with tempfile.TemporaryDirectory() as directory:
            run = Path(directory)
            manifest = run / "manifest.json"
            manifest.write_text(json.dumps([
                {"build": "commit", "law": "a", "required_witnesses": ["never", "rare", "repeat"]}
            ]))
            for batch, rows in enumerate((
                [{"repeat": 10, "rare": 0}, None],
                [{"repeat": 2, "extra": 4}, {"rare": 1}],
            )):
                with gzip.open(run / f"{batch}.jsonl.gz", "wt") as stream:
                    for i, witnesses in enumerate(rows):
                        print(json.dumps({"build": "commit", "law": "a", "seed": 2 * batch + i + 1,
                                          "status": 0, "witnesses": witnesses}), file=stream)
            probes = summarize(run, manifest, "commit")["probes"]
            self.assertEqual([(probe["label"], probe["hits"], probe["true_calls"]) for probe in probes],
                             [("extra", 1, 4), ("never", 0, 0), ("rare", 1, 1), ("repeat", 2, 12)])
            self.assertTrue(all(probe["runs"] == 4 and probe["observed_runs"] == 3 for probe in probes))

    def test_receipt_preserves_red_nights_and_executable_revisions(self):
        tool = Path(__file__).with_name("vopr_nightly_receipt.py")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            log = root / "receipts.ndjson"
            run = root / "night"
            run.mkdir()
            (run / "inventory").write_text(f"a\nb\n{CANARY}\n")
            source = root / "revision.c"
            source.write_text(
                'const unsigned char hirsel_code_revision[32] = {1, 2, 3};\n'
                'int main(void) { return 0; }\n'
            )
            binary = root / "law"
            subprocess.run(["cc", "-o", binary, source], check=True)
            manifest = root / "manifest.json"
            manifest.write_text(json.dumps([
                {"build": "commit", "law": law, "executable": str(binary), "recording": str(binary)}
                for law in ("a", CANARY)
            ]))
            (run / "prepare.jsonl").write_text(json.dumps({
                "build": "commit", "law": "b", "kind": "prepare_red", "reason": "refused"
            }) + "\n")
            with gzip.open(run / "batch.jsonl.gz", "wt") as stream:
                for law, seed, status in (("a", 1, 1), (CANARY, 2, 0), (CANARY, 3, 1)):
                    row = {"build": "commit", "law": law, "seed": seed, "status": status,
                           "stdout": "", "stderr": "error: ()\n" if status else "",
                           "tape_replay": {"status": 0}}
                    if law == "a":
                        row.update(tape_divergence="Outcome", witnesses={"fault:lost-write": 1})
                    print(json.dumps(row), file=stream)
            command = [sys.executable, tool, run, manifest, "commit", "sweep", "0"]
            self.assertEqual(subprocess.run(command).returncode, 1)
            self.assertEqual(subprocess.run(command).returncode, 1)
            early = root / "build-failed"
            early.mkdir()
            (early / "inventory").write_text("a\n")
            self.assertEqual(subprocess.run([
                sys.executable, tool, early, early / "absent", "parent", "build", "2"
            ]).returncode, 2)
            receipt, failure = map(json.loads, log.read_text().splitlines())
            revision = (b"\x01\x02\x03" + bytes(29)).hex()
            self.assertEqual(receipt["code_revision"]["a"], {"normal": revision, "recording": revision})
            self.assertEqual(receipt["laws_unprepared"], {"b": "refused"})
            self.assertEqual(receipt["seeds_executed"], 3)
            self.assertEqual(receipt["seeds_failed"], 1)
            self.assertEqual(receipt["replays_diverged"], 1)
            self.assertEqual(receipt["fault_classes_sampled"], ["lost-write"])
            self.assertTrue(receipt["canary_red"])
            self.assertTrue(receipt["not_covered"])
            self.assertEqual((failure["commit"], failure["phase"], failure["status"]),
                             ("parent", "build", 2))
            self.assertEqual(failure["laws_unprepared"], {"a": "no prepared executable"})
            self.assertEqual(failure["code_revision"], {})
            self.assertIsNone(failure["canary_red"])


if __name__ == "__main__":
    unittest.main()
