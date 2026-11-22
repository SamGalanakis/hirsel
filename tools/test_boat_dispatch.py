"""Dispatcher acceptance test. Run with python3 tools/test_boat_dispatch.py."""
from contextlib import redirect_stdout
import fcntl
import io
import json
import os
from pathlib import Path
import runpy
import sqlite3
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("boat-dispatch")
dispatcher = runpy.run_path(str(SCRIPT))
dispatch = dispatcher["dispatch"]
batches = dispatcher["batches"]
manifest_jobs = dispatcher["manifest_jobs"]
stage = dispatcher["stage"]


class FakeBoat:
    def __init__(self):
        self.started = []
        self.deleted = []

    def __call__(self, *args, **_kwargs):
        match args[:2]:
            case ("--json", "limits"):
                # The provider reports spare capacity. The local counter must
                # refuse the 201st request even if the provider would accept it.
                windows = {window: {"used": 0, "remaining": limit}
                           for window, limit in (("day", 200), ("hour", 60), ("minute", 12))}
                data = {"activeSandboxes": 0, "maxActiveSandboxes": 100, "starts": windows}
            case ("--json", "new"):
                self.started.append(args)
                data = {"event": "created", "id": f"fake-{len(self.started)}"}
            case ("--json", "delete"):
                self.deleted.append(args[2])
                data = {}
            case _:
                raise AssertionError(f"dry run contacted a VM: {args}")
        return subprocess.CompletedProcess(args, 0, json.dumps(data).encode(), b"")


class BoatDispatchTest(unittest.TestCase):
    def test_vopr_yields_to_laws_and_records_disjoint_seed_ranges(self):
        with tempfile.TemporaryDirectory() as scratch, redirect_stdout(io.StringIO()):
            root = Path(scratch)
            job = dispatcher["Job"]("vopr", dispatcher["Input"](root, False), "true", 1, ())
            backend = FakeBoat()
            with (root / "laws.lock").open("a") as law:
                fcntl.flock(law, fcntl.LOCK_SH)
                with self.assertRaisesRegex(ValueError, "law shards are pending"):
                    dispatch([[job]], 60, root, backend, dry_run=True, job_class="vopr")
                self.assertEqual(backend.started, [])
            self.assertEqual(dispatch([[job]] * 179, 60, root, backend, dry_run=True), 0)
            self.assertEqual(dispatch([[job]], 60, root, backend, dry_run=True, job_class="vopr"), 0)
            with self.assertRaisesRegex(ValueError, "preserve start capacity for laws"):
                dispatch([[job]], 60, root, backend, dry_run=True, job_class="vopr")
            self.assertEqual(len(backend.started), 180)
            self.assertEqual(dispatch([[job]], 60, root, backend, dry_run=True), 0)
            self.assertEqual(len(backend.started), 181)

            script = SCRIPT.with_name("sim-campaign")
            campaign = runpy.run_path(str(script))
            fixture = root / "test-law"
            fixture.write_text('#!/bin/sh\ntest "$HIRSEL_TEST_SIM_SEED" != 2\n')
            fixture.chmod(0o700)
            db = sqlite3.connect(":memory:")
            try:
                ranges = campaign["allocate"](db, "first", 2, 2)
                self.assertEqual(ranges, [(1, 3), (3, 5)])
                first = subprocess.check_output([script, "run", fixture, "1", "3"], text=True)
                second = subprocess.check_output([script, "run", fixture, "3", "5"], text=True)
                results = io.StringIO()
                self.assertEqual(campaign["record"](db, "first", ranges,
                                                   (second + first).splitlines(), results, fixture),
                                 (True, 1))
                rows = [json.loads(line) for line in results.getvalue().splitlines()]
                self.assertEqual({row["seed"]: row["status"] for row in rows}, {1: 0, 2: 1, 3: 0, 4: 0})
                self.assertEqual(db.execute("SELECT executed, failed FROM shards ORDER BY shard").fetchall(),
                                 [(2, 1), (2, 0)])
                with self.assertRaises(sqlite3.IntegrityError):
                    campaign["allocate"](db, "first", 2, 2)
                self.assertEqual(campaign["allocate"](db, "second", 1, 3), [(5, 8)])
            finally:
                db.close()

    def test_three_lanes_share_minimum_starts_and_stop_at_200(self):
        with tempfile.TemporaryDirectory() as scratch, redirect_stdout(io.StringIO()) as output:
            root = Path(scratch)
            cache = root / "cache"
            cache.mkdir()
            manifest = root / "shards.json"
            entries = []
            # Five repetitions of (6,5,3,2,2,2) total 100s.
            # (6+2+2) and (5+3+2) per repetition give ten full 10s batches.
            times = iter([6, 5, 3, 2, 2, 2] * 5)
            for lane in range(3):
                bundle = root / f"worktree-{lane}" / "staged"
                bundle.mkdir(parents=True)
                program = bundle / "law"
                program.write_text("test input\n")
                os.utime(program, (lane, lane))
                for _ in range(10):
                    entries.append({"lane": f"lane-{lane}", "bundle": str(bundle),
                                    "command": "true", "seconds": next(times)})
            manifest.write_text(json.dumps(entries))
            jobs = manifest_jobs(manifest)
            groups = batches(jobs, 10)
            self.assertEqual(len(groups), 10)
            self.assertCountEqual([job for group in groups for job in group], jobs)
            self.assertTrue(all(sum(job.seconds for job in group) == 10 for group in groups))

            archives = {stage(cache, job.inputs) for job in jobs[::10]}
            self.assertEqual(len(archives), 1)
            self.assertEqual(len(list(cache.glob("*.tar.gz"))), 1)

            credentials = root / ".ssh"
            credentials.mkdir()
            (credentials / "id_ed25519").write_text("fake credential\n")
            with self.assertRaisesRegex(ValueError, "local-only input in bundle"):
                stage(cache, dispatcher["Input"](credentials, False))

            backend = FakeBoat()
            self.assertEqual(dispatch([[jobs[0]]] * 190, 60, root, backend, dry_run=True), 0)
            output.seek(0)
            output.truncate()
            self.assertEqual(dispatch(groups, 60, root, backend, dry_run=True), 0)
            self.assertEqual(len(output.getvalue().splitlines()), 10)
            with self.assertRaisesRegex(ValueError, r"daily start limit reached \(200\)"):
                dispatch([[jobs[0]]], 60, root, backend, dry_run=True)
            self.assertEqual(len(backend.started), 200)
            self.assertEqual(backend.deleted, [f"fake-{n}" for n in range(1, 201)])
            self.assertTrue(all(args[args.index("--from") + 1] == "hirsel-runner-base"
                                and "--no-env" in args for args in backend.started))


if __name__ == "__main__":
    unittest.main()
