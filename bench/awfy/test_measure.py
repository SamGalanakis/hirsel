from argparse import Namespace
from contextlib import redirect_stdout
from copy import deepcopy
import fcntl
import gzip
import io
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
from pathlib import Path

import awfy_measure
from board import EXPECTED, Board, measurement_slot, rebaseline, receipt
from checkbox_gate import checkbox_value, session_rss_sample
import bench_cpus
import build
import cold_phases


class PassCostAccounting(unittest.TestCase):
    def test_rejects_double_counted_nested_passes(self):
        with tempfile.TemporaryDirectory() as temp:
            trace = Path(temp) / "passes.jsonl.gz"
            rows = [
                {"phase": "release", "pass": "child", "ir_unit": "f", "instructions": 70},
                {"phase": "release", "pass": "parent", "ir_unit": "m", "instructions": 20},
                {"phase": "release", "ir_unit": "m", "pipeline_instructions": 100},
                {"phase": "object-emission", "ir_unit": "part", "pipeline_instructions": 40},
            ]

            def write_trace():
                with gzip.open(trace, "wt") as stream:
                    for row in rows:
                        stream.write(json.dumps(row) + "\n")

            write_trace()
            passes, phases = cold_phases.pass_costs(trace)
            self.assertEqual(sum(row["instructions"] for row in passes.values()), 90)
            self.assertEqual(phases["release"]["unattributed_instructions"], 10)
            self.assertEqual(phases["object-emission"]["unattributed_instructions"], 40)
            rows[1]["instructions"] = 90
            write_trace()
            with self.assertRaisesRegex(ValueError, "exceed their pipeline counters"):
                cold_phases.pass_costs(trace)


class WorkloadMatching(unittest.TestCase):
    def test_build_cli_selects_pool_index_images(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            out = root / "out"
            clang = root / "bin/clang"
            clang.parent.mkdir()
            clang.write_bytes(b"toolchain fixture")
            toolchain = root / "toolchain.json"
            toolchain.write_text(json.dumps({"compiler": str(clang), "runtime": str(clang)}))
            rap = root / "benchmarks/JavaScript/json.js"
            rap.parent.mkdir(parents=True)
            rap.write_text('const RAP_BENCHMARK_MINIFIED = "{}";\n')

            def compile_image(command, **kwargs):
                Path(command[command.index("-o") + 1]).write_bytes(b"image")
                return subprocess.CompletedProcess(command, 0, stdout=b"{}")

            argv = ["build.py", str(out), "--upstream", str(root),
                    "--artifacts", str(toolchain), "--hirsel-entries", "pool_index",
                    "--goose", "goose", "--goose-entries", "pool_index"]
            with (patch.object(sys, "argv", argv), patch.object(build, "llvm_path", return_value=root),
                  patch.object(build.subprocess, "run", side_effect=compile_image)):
                build.main()
            build_report = json.loads((out / "hirsel-pool_index-build.json").read_text())
            self.assertIn("awfy@1/pool_index/main", build_report["measurement"]["flags"])
            sources = json.loads((out / "hirsel-inputs.json").read_text())["packages"][-1]["sources"]
            self.assertIn(["pool_index"], [source["modules"] for source in sources])
            self.assertIn("havlak_pool.benchmark(inner)", (out / "pool_index.hirsel").read_text())
            self.assertIn("import havlak_pool;", (out / "goose-src/pool_index.goose").read_text())
            self.assertEqual((out / "goose-pool_index").read_bytes(), b"image")

    def test_document_pools_do_not_use_original_rust_rows(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / "rebench.conf").write_text(
                "            - Json:\n                extra_args: 100\n"
                "            - Storage:\n                extra_args: 25\n")
            js = root / "benchmarks/JavaScript"
            js.mkdir(parents=True)
            (js / "json.js").write_text('const RAP_BENCHMARK_MINIFIED = "{}";\n')
            originals = dict(awfy_measure.rows(root, False, root))
            pools = dict(awfy_measure.matched_pool_rows(root, root))
        for name, inner, checksum, workload in (("Json", "100", 156, "{}"), ("Storage", "25", 5461, "7,7")):
            pool = name + "Pool"
            self.assertEqual(originals[name]["rust"][0][1:], [name, "1", inner, workload])
            command, marker = pools[pool]["rust"]
            self.assertNotEqual(command[0], originals[name]["rust"][0][0])
            self.assertEqual(command[1:], [pool, "1", inner, workload])
            self.assertIsNone(re.fullmatch(marker, f"{name}: iterations=1 runtime: 1us\n".encode()))
            for side in ("hirsel", "goose"):
                command, marker = pools[pool][side]
                self.assertEqual(command[1:], [pool, "1", inner, workload])
                self.assertIsNotNone(re.fullmatch(marker, f"verified checksum={checksum}\n".encode()))

class BoardCompletion(unittest.TestCase):
    def test_receipt_requires_every_row_and_side(self):
        with tempfile.TemporaryDirectory() as temp, redirect_stdout(io.StringIO()):
            out = Path(temp)
            for omit_edit, sample_status, exit_code in (
                (True, "ok", 1),
                (False, "missing-counter", 1),
                (False, "ok", 0),
            ):
                with self.subTest(omit_edit=omit_edit, sample_status=sample_status):
                    board = self.board_fixture(out, omit_edit, sample_status)
                    self.assertEqual(board.finish(), exit_code)
                    report = json.loads((out / "board.json").read_text())
                    self.assertEqual(report["complete"], exit_code == 0)
                    edit = next(row for row in report["expected_rows"]
                                if (row["goal"], row["row"]) == ("Compile", "LangArena edit"))
                    status = "missing" if omit_edit else "failed" if exit_code else "ok"
                    self.assertEqual(edit["status"], status)

    def test_rebaseline_accepts_identity_only_refusals(self):
        with tempfile.TemporaryDirectory() as temp, redirect_stdout(io.StringIO()) as output:
            run = Path(temp) / "runs/candidate"
            path = run / "board/board.json"
            path.parent.mkdir(parents=True)
            base = Path(temp) / ".local/share/hirsel-board"
            base.mkdir(parents=True)
            previous = base / "last-complete.json"
            previous.symlink_to(base / "previous.json")
            report = self.board_fixture(path.parent).report | {
                "complete": True,
                "exit_status": 2,
                "instruction_comparison": {"status": "Refused", "details": [
                    "awfy/Bounce REFUSED (identity differs: awfy/Bounce: flags, affinity)",
                    "awfy/CD REFUSED (identity differs: awfy/CD: flags, argv, affinity)",
                    "awfy/Json 1,000 -> 1,002 (+0.200%)",
                ]},
                "size_comparison": ["hirsel 2,048 -> 2,048 B (+0 B)"],
                "checkbox_comparison": ["memory-subscribed, 64 clients/hirsel 64.00 -> 64.00 B/session (+0.000%)"],
            }

            with patch.object(Path, "home", return_value=Path(temp)):
                for defect in ("incomplete", "failed measurement", "other instruction refusal",
                               "instruction regression", "size regression", "size refusal", "checkbox refusal"):
                    with self.subTest(defect=defect):
                        bad = deepcopy(report)
                        if defect == "incomplete":
                            bad["complete"] = False
                        elif defect == "failed measurement":
                            bad["rows"][0]["status"] = "failed"
                        elif defect == "other instruction refusal":
                            bad["instruction_comparison"]["details"].append("awfy/Json REFUSED (inactive-counter)")
                        elif defect == "instruction regression":
                            bad["instruction_comparison"]["details"][-1] = "awfy/Json 1,000 -> 1,030 (+3.000%) FAIL"
                        elif defect == "size regression":
                            bad["size_comparison"] = ["hirsel 2,048 -> 4,096 B (+2,048 B) FAIL"]
                        elif defect == "size refusal":
                            bad["size_comparison"] = ["REFUSED (hirsel lacks a verified size sample)"]
                        elif defect == "checkbox refusal":
                            bad["checkbox_comparison"].append("REFUSED (incomplete checkbox sample)")
                        path.write_text(json.dumps(bad))
                        with self.assertRaises(ValueError):
                            rebaseline(run)
                        self.assertEqual(previous.readlink(), base / "previous.json")

                path.write_text(json.dumps(report))
                self.assertEqual(rebaseline(run), 0)
                self.assertEqual(previous.readlink(), path)
                self.assertIn("measurement identity changed: affinity, argv, flags", output.getvalue())

                report["instruction_comparison"] = {"status": "Within +2% on every row", "details": [
                    "awfy/Json 1,000 -> 1,002 (+0.200%)",
                ]}
                report["size_comparison"] = ["REFUSED (size identity differs: strip_flags)"]
                path.write_text(json.dumps(report))
                self.assertEqual(rebaseline(run), 0)
                self.assertIn("measurement identity changed: strip_flags", output.getvalue())

    def board_fixture(self, out, omit_edit=False, sample_status="ok"):
        board = Board(out, "abcd1234")
        board.report["size_measurement"] = {
            "strip_version": "GNU strip 2.42", "strip_flags": ["--strip-all"],
            "verified_workloads": sorted(awfy_measure.EXPECTED["AWFY original"]),
            "pins": board.report["pins"],
        }
        suites = {goal: suite for suite, goal in awfy_measure.SUITES.items()}
        for goal, names in EXPECTED.items():
            for name in names:
                if name == "LangArena edit" and omit_edit:
                    continue
                sides = ("hirsel", "rust") if goal in ("Compile", "Checkbox") else awfy_measure.SIDES
                fields, values = self.sample(goal, name)
                samples = {side: {"status": "ok", **values} for side in sides}
                if name == "LangArena edit":
                    samples["rust"]["status"] = sample_status
                if goal in suites:
                    suite = suites[goal]
                    mode = name.split()[-1] if suite == "compile" else "release"
                    board.report["instruction_receipt"]["rows"].append({
                        "key": f"{suite}/{name}", "suite": suite, "mode": mode,
                        "flags": ["--release"], "argv": ["./sample"],
                        "affinity": [10], "compiler_sha256": "a" * 64,
                        "runtime_archive_sha256": "b" * 64, "fixture_sha256": "c" * 64,
                        "clang_sha256": "2" * 64,
                        "std_sha256": "d" * 64, "workload_sources_sha256": "e" * 64,
                        "image_sha256": "f" * 64,
                        "fixture_files": {"fixture.json": "c" * 64},
                        "marker": "verified\n", "stdout": "verified\n",
                        "sample": samples["hirsel"],
                    })
                board.row(goal, name, samples, fields)
        return board

    @staticmethod
    def sample(goal, name):
        if goal == "Size":
            return ("bytes",), {"bytes": 2048, "image_sha256": "d" * 64, "binary_sha256": "e" * 64}
        if goal == "Checkbox":
            if name.startswith("converge"):
                return ("toggles_per_second",), {"toggles_per_second": 450}
            return ("server_rss_bytes",), {"server_rss_bytes": 4096, "connections": 64}
        return ("instructions", "cycles"), {
            "instructions": 123, "cycles": 99, "verified": True,
            "pmu_times_ns": {event: [[100, 100]] for event in ("instructions:u", "cycles:u")},
        }

    def test_checkbox_rss_subtracts_startup_idle_before_session_division(self):
        idle, loaded = 8 * 1024**2, 12 * 1024**2
        sample = session_rss_sample({"status": "ok", "server_rss_bytes": loaded, "connections": 64}, idle)
        self.assertEqual(sample["server_idle_rss_bytes"], idle)
        self.assertEqual(sample["server_loaded_rss_bytes"], loaded)
        self.assertEqual(int(checkbox_value(sample, "server_rss_bytes")), (loaded - idle) // 64)

    def test_second_descriptor_cannot_take_measurement_lock(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "board.lock"
            with path.open("a") as other:
                fcntl.flock(other, fcntl.LOCK_EX | fcntl.LOCK_NB)
                fcntl.flock(other, fcntl.LOCK_UN)
                with measurement_slot(path):
                    with self.assertRaises(BlockingIOError):
                        fcntl.flock(other, fcntl.LOCK_EX | fcntl.LOCK_NB)
                fcntl.flock(other, fcntl.LOCK_EX | fcntl.LOCK_NB)


class ComparisonReceipts(unittest.TestCase):
    commit = "f" * 40

    def arm(self, mode, rss):
        return {"commit": self.commit, "mode": mode, "status": "ok", "verified": True,
                "max_rss_kib": rss, "instructions": 1, "cycles": 1, "seconds": 1,
                "pmu_times_ns": {"instructions:u": [[10, 10]], "cycles:u": [[10, 10]]}}

    def runtime_evidence(self):
        from comparison import RUNTIME_SUITES, VERSION

        evidence = {"version": VERSION, "commit": self.commit, "families": ["runtime-memory"],
                    "failures": {}, "samples": {}, "runtime": {}}
        for suite in RUNTIME_SUITES:
            for name in awfy_measure.EXPECTED[suite]:
                evidence["runtime"][f"{suite}/{name}"] = {
                    side: self.arm("runtime", 128) for side in awfy_measure.SIDES}
        cases = evidence["runtime"]
        cases["AWFY original/Bounce"]["hirsel"]["max_rss_kib"] = 256
        cases["AWFY original/List"]["hirsel"]["max_rss_kib"] = 64
        return evidence

    def test_opposite_ratios_cancel_and_full_coverage_is_required(self):
        from comparison import summary

        report = summary(self.runtime_evidence(), [])
        self.assertTrue(report["selection_complete"])
        self.assertFalse(report["complete"])
        self.assertEqual(report["rows"][0]["coverage"]["rust"], (14, 14))
        aggregate = report["rows"][-1]
        self.assertEqual(aggregate["coverage"], {"rust": (72, 72), "goose": (72, 72)})
        self.assertEqual(aggregate["ratios"], {"rust": {"geomean_peak_rss": 1},
                                                "goose": {"geomean_peak_rss": 1}})

    def test_invalid_rss_arm_refuses_only_aggregates_depending_on_it(self):
        from comparison import summary

        for change, reason in (
            (None, "missing arm"),
            ({"commit": "e" * 40}, "stale revision"),
            ({"mode": "startup"}, "mismatched mode"),
            ({"max_rss_kib": 0}, "missing or invalid metric"),
            ({"status": "memory-censored"}, "memory-censored"),
        ):
            with self.subTest(reason=reason):
                evidence = self.runtime_evidence()
                samples = evidence["runtime"]["AWFY original/Bounce"]
                if change is None:
                    del samples["rust"]
                else:
                    samples["rust"].update(change)
                report = summary(evidence, [])
                original, pools, arena, aggregate = report["rows"]
                self.assertEqual(original["issues"]["AWFY original/Bounce/rust"], reason)
                self.assertEqual(pools["status"], "ok")
                self.assertEqual(arena["status"], "ok")
                self.assertEqual(aggregate["coverage"], {"rust": (71, 72), "goose": (72, 72)})
                self.assertEqual(aggregate["ratios"], {"rust": {}, "goose": {"geomean_peak_rss": 1}})
                self.assertFalse(report["selection_complete"])

    def test_old_checksum_cannot_qualify_edit(self):
        from comparison import SPECTRALNORM, compare_row, compile_oracle, COMPILE_FIELDS

        mode = "arena-release/edit"
        samples = {side: self.arm(mode, 128) | {"rebuilt": True} for side in awfy_measure.SIDES}
        for side, sample in samples.items():
            marker = ("CLBG::Spectralnorm checksum=1051192944 expected=1051192944 status=OK"
                      if side == "hirsel" else "CLBG::Spectralnorm: OK in 0.01s")
            sample["verification"] = {
                SPECTRALNORM: {"returncode": 0, "stdout": marker + "\n", "expected_checksum": 1051192945}}
        def compare():
            return compare_row("Release compile", "LangArena one-body edit", samples, self.commit, mode,
                               COMPILE_FIELDS, oracle=lambda side, sample:
                               compile_oracle(sample, side, "arena-release", "edit"))
        row = compare()
        self.assertEqual(row["status"], "incomplete")
        self.assertEqual(row["issues"], {"hirsel": "workload or rebuild oracle failed"})
        self.assertEqual(row["ratios"], {"rust": {}, "goose": {}})
        samples["hirsel"]["verification"][SPECTRALNORM]["stdout"] = (
            "CLBG::Spectralnorm checksum=1051192945 expected=1051192945 status=OK\n")
        self.assertEqual(compare()["status"], "ok")

class FairReceipts(unittest.TestCase):
    fixtures = Path(__file__).with_name("fixtures") / "fair"

    def test_producer_receipt_round_trip(self):
        from comparison_units import import_rows
        with patch.object(sys, "path", [str(build.ROOT / "bench/units"), *sys.path]):
            from bench.units import fair

        # The archived quiet-1720-checkbox export contains one measured row.
        # Replay it through the current writer at full coverage without running anything.
        archive = receipt.read(self.fixtures / "fair.json")
        commit = archive["commit"]
        source, = archive["rows"]
        observed = source["samples"]
        cpu_sets = {role: archive["config"][f"{role}_cpus"]
                    for role in ("server", "load", "helper", "controller")}
        config = fair.run_config(full=True, suites=["a1", "regimes"], cases=fair.CASES,
                                 repeats=1, reverse=False, cpu_sets=cpu_sets)
        with tempfile.TemporaryDirectory() as temp:
            out = Path(temp)
            shutil.copytree(self.fixtures / "regime-samples", out / "regime-samples")
            path = out / "fair.json"
            report = fair.Report(out, commit, config, fair.expected_rows(config["families"], fair.CASES, True))
            report.data.update(started=archive["started"], binary_sha256=archive["binary_sha256"])
            report.row("Regimes", source["row"], observed, source["metrics"],
                       case="checkbox", grain_ns=0, width=1, kind="work_rate")
            report.data["failures"]["run"] = "Deadline"
            report.save()
            export = receipt.read(path)
            self.assertEqual(export["config"]["build_memory_limit_bytes"], 4 << 30)
            self.assertEqual(export["config"]["measurement_memory_limit_bytes"], 2 << 30)
            self.assertEqual(export["config"]["helper_cpus"], [26, 27])
            self.assertEqual(export["config"]["controller_cpus"], [26, 27])
            self.assertEqual(len(export["rows"]), 257)

            def read_board():
                rows = import_rows(path, commit)
                self.assertEqual(len(rows), 257)
                tmpfs = [row for row in rows if row["row"].startswith("tmpfs/")
                         and row["row"].endswith(" / matched offer")]
                self.assertEqual([row["row"].split("/")[1] for row in tmpfs], ["256", "2048", "8192"])
                self.assertTrue(all(row["status"] == "rejected" and row["issues"]["export"] == "Deadline"
                                    for row in tmpfs))
                return next(row for row in rows if row["row"] == "checkbox/native/w1")

            refused = read_board()
            self.assertEqual(refused["status"], "rejected")
            self.assertEqual(refused["issues"]["export"], "regime pair receipt or hold is unqualified")

            # The recorded opening load was 8.0347. Set only that input to the
            # accepted bound to exercise the successful qualification path.
            pair_path = out / Path(observed["hirsel"]["arms"][0]["path"]).parent / "pair.json"
            pair = receipt.read(pair_path)
            self.assertGreater(pair["hold"]["load_start"], 8)
            pair["hold"]["load_start"] = 8
            receipt.write(pair_path, pair)
            qualified = read_board()
            self.assertEqual(qualified["status"], "ok")
            self.assertEqual(qualified["samples"]["hirsel"]["work_per_s"],
                             12800 / observed["hirsel"]["arms"][0]["service_seconds"])

            for field, value, refusal in (
                ("measurement_memory_limit_bytes", 3 << 30, "required build, measurement cap"),
                ("build_memory_limit_bytes", 8 << 30, "required build, measurement cap"),
                ("load_cpus", [16], "invalid units CPU topology"),
            ):
                with self.subTest(field=field):
                    receipt.write(path, export | {"config": export["config"] | {field: value}})
                    with self.assertRaisesRegex(ValueError, refusal):
                        import_rows(path, commit)
            receipt.write(path, export | {"rows": export["rows"][:-1]})
            with self.assertRaisesRegex(ValueError, "incomplete units label set"):
                import_rows(path, commit)

    def test_switches_charge_the_outgoing_thread_group(self):
        from bench.units import measurement

        # CPU 16 runs the unrelated worker for 3 ms, the owned worker for
        # 996 ms, and the unidentified tail for 2 ms. CPU 17 runs the
        # owned load process for 900 ms and is otherwise idle.
        start, end = 11_000_000_000, 12_001_000_000
        windows = [{"side": "hirsel", "pids": [700, 800], "start_ns": start, "end_ns": end}]
        with ((self.fixtures / "switches.txt").open() as trace,
              patch.object(measurement.subprocess, "Popen") as popen):
            reader = popen.return_value.__enter__.return_value
            reader.stdout = trace
            reader.wait.return_value = 0
            result = measurement.classify_tasks(
                Path("schedule.perf.data"), {"server": [16], "load": [17]}, windows,
                10_990_000_000, 12_003_000_000, {16: 1_001_000_000, 17: 900_000_000},
            )
        self.assertIn("trace:pid,tid,cpu,time,event,trace", popen.call_args.args[0])
        arm = result["arm_cpu"]["hirsel"]
        self.assertEqual(arm["owned_cpu_ns"], {16: 996_000_000, 17: 900_000_000})
        self.assertEqual(arm["foreign_cpu_ns"], {16: 5_000_000, 17: 0})
        self.assertTrue(arm["admitted"])


class SiblingActivity(unittest.TestCase):
    def test_cold_phases_reuses_the_lane_cpu_set(self):
        original = os.sched_getaffinity(0)
        cpus = sorted(original)[:2]
        self.addCleanup(os.sched_setaffinity, 0, original)

        def collect(args, scratch):
            self.assertEqual(os.sched_getaffinity(0), set(cpus))
            self.assertEqual(args.measure_cpus, (cpus[0],))
            return 0

        with tempfile.TemporaryDirectory() as temp:
            argv = ["cold_phases.py", str(Path(temp) / "out"), "--artifacts", "toolchain.json",
                    "--fixture", "fixture.json", "--cpus", ",".join(map(str, cpus))]
            with patch.object(sys, "argv", argv), patch.object(cold_phases, "collect", side_effect=collect):
                self.assertEqual(cold_phases.main(), 0)

    def test_records_busy_sibling_from_linux_ticks(self):
        # CPU 18 sees four active ticks out of twenty. Linux already
        # includes guest time in user time. CPU 19 only sees I/O wait.
        with tempfile.TemporaryDirectory() as temp:
            stat = Path(temp) / "stat"
            stat.write_text("cpu18 100 0 50 300 80 0 0 0 20 0\ncpu19 100 0 50 300 80 0 0 0 20 0\n")
            with patch.object(bench_cpus, "Path", return_value=stat):
                with bench_cpus.smt_activity([18, 19]) as busy:
                    stat.write_text("cpu18 104 0 50 316 80 0 0 0 24 0\ncpu19 100 0 50 300 100 0 0 0 20 0\n")
        self.assertEqual(busy, {18: 20.0, 19: 0.0})

    def test_refuses_threads_sharing_a_core_in_one_run(self):
        with tempfile.TemporaryDirectory() as temp:
            topology = Path(temp) / "thread_siblings_list"
            topology.write_text("2,18\n")
            with patch.object(bench_cpus, "Path", return_value=topology):
                self.assertEqual(bench_cpus.sibling_cpus([2], [1]), [18])
                with self.assertRaisesRegex(ValueError, "build set"):
                    bench_cpus.sibling_cpus([2], [18])
                with self.assertRaisesRegex(ValueError, "measurement set"):
                    bench_cpus.sibling_cpus([2, 18], [1])


class PairedSampling(unittest.TestCase):
    def test_cycle_noise_does_not_override_instruction_regression(self):
        key = "awfy/List"
        base = {
            "key": key, "suite": "awfy", "mode": "release",
            "flags": ["--release"], "argv": ["./sample", "List"], "marker": "verified\n",
            "compiler_sha256": "a" * 64, "runtime_archive_sha256": "b" * 64,
            "clang_sha256": "2" * 64,
            "fixture_sha256": "c" * 64, "image_sha256": "d" * 64, "std_sha256": "e" * 64,
            "fixture_files": {"rebench.conf": "c" * 64}, "workload_sources_sha256": "f" * 64,
            "manifest_sha256": "1" * 64,
        }
        base["build_record"] = base | {"accepted": True, "linked": True}
        candidate = base | {"image_sha256": "e" * 64,
                            "build_record": base["build_record"] | {"image_sha256": "e" * 64}}
        cpu = min(os.sched_getaffinity(0))
        events = ("instructions:u", "cycles:u")
        counts = iter((100, 96, 101, 100, 100, 104) + (100, 103, 103, 100, 100, 103) * 2)
        activity = iter((5, 5, 10, 10, 25, 25, 10, 30, 49, 29, 25, 25, 10, 30, 50, 29, 25, 25))
        images = []

        def observe(row, directory, env, siblings, *, cycles):
            images.append(row["image_sha256"])
            row["affinity"] = [cpu]
            row["stdout"] = "verified\n"
            row["sample"] = {
                "status": "ok", "verified": True, "cycles": next(counts),
                "smt_busy_percent": {18: next(activity)},
                "instructions": 100 if row["image_sha256"] == base["image_sha256"] else 101,
                "pmu_times_ns": {event: [[100, 100]] for event in events},
            }

        with (tempfile.TemporaryDirectory() as temp, redirect_stdout(io.StringIO()),
              patch.object(receipt, "runtime_rows", return_value=[(key, ())]),
              patch.object(receipt, "prepare_pair_rows", side_effect=[{key: base}] + [{key: base}, {key: candidate}] * 2),
              patch.object(receipt, "sibling_cpus", return_value=[18]),
              patch.object(receipt.os, "sched_setaffinity"), patch.object(receipt, "observe", side_effect=observe)):
            root = Path(temp)
            args = Namespace(upstream=None, candidate_awfy_build=None, candidate_pool_build=None,
                             candidate_arena_build=None, candidate_artifacts=None, aa=None, artifacts=None,
                             rows="List", pairs=3, cpu=cpu, lock=root / "lock", threshold=None, out=root / "aa")
            self.assertEqual(receipt.pair_samples(args), 0)
            args.aa = args.out / "pairs.json"
            args.candidate_artifacts = root / "candidate-toolchain.json"
            for verdict in ("within noise", "sibling busy"):
                args.out = root / verdict.replace(" ", "-")
                self.assertEqual(receipt.pair_samples(args), 1)
                result = receipt.read(args.out / "pairs.json")["summary"][key]
                self.assertEqual(result["cycles"]["median_percent"], "3")
                self.assertEqual(result["cycles"]["verdict"], verdict)
        a, b = base["image_sha256"], candidate["image_sha256"]
        self.assertEqual(images[6:], [a, b, b, a, a, b] * 2)
