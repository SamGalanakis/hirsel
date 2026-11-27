"""Exercise the nightly wrapper and the missed-run check against a fixture board."""
import json
import os
import shlex
from pathlib import Path
import socket
import sqlite3
import subprocess
import sys
import tempfile
import time
import unittest

TOOLS = Path(__file__).resolve().parents[1]


class BoardFreshnessTest(unittest.TestCase):
    def test_nightly_service_path_includes_cargo_and_mise_shims(self):
        environment = next(
            line.removeprefix("Environment=PATH=")
            for line in (TOOLS / "hirsel-board.service").read_text().splitlines()
            if line.startswith("Environment=PATH=")
        )
        path = environment.split(":")
        self.assertIn("%h/.cargo/bin", path)
        self.assertIn("%h/.local/share/mise/shims", path)
        self.assertLess(path.index("%h/.local/bin"), path.index("%h/.cargo/bin"))

    def test_board_environment_resolves_wrapped_cargo_and_mise_npm(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            local_bin = home / ".local/bin"
            cargo_bin = home / ".cargo/bin"
            node_bin = home / ".local/share/mise/installs/node/25.2.1/bin"
            for directory_path in (local_bin, cargo_bin, node_bin):
                directory_path.mkdir(parents=True)

            def executable(path, text):
                path.write_text(text)
                path.chmod(0o755)

            executable(local_bin / "cargo", """#!/bin/bash
if [[ -z ${KILN_REAL_CARGO:-} ]]; then
  printf 'cargo wrapper: no real cargo found after fixture-kiln on PATH\\n' >&2
  exit 127
fi
exec "$KILN_REAL_CARGO" "$@"
""")
            executable(cargo_bin / "cargo", """#!/bin/bash
printf 'fixture-cargo:%s\\n' "$*"
""")
            executable(local_bin / "mise", f"""#!/bin/bash
if [[ $1 == which ]]; then
  case $2 in
    node) printf '%s\\n' {shlex.quote(str(node_bin / 'node'))}; exit 0 ;;
    npm) printf '%s\\n' {shlex.quote(str(node_bin / 'npm'))}; exit 0 ;;
  esac
fi
exit 1
""")
            executable(node_bin / "node", """#!/bin/bash
printf 'fixture-node:%s\\n' "$*"
""")
            executable(node_bin / "npm", """#!/bin/bash
printf 'fixture-npm:%s\\n' "$*"
""")

            environment = {"HOME": str(home), "PATH": f"{local_bin}:/usr/local/bin:/usr/bin:/bin"}
            helper = TOOLS / "board-environment.sh"
            for command, expected in (
                (("cargo", "+1.99.0", "--version"), "fixture-cargo:+1.99.0 --version"),
                (("npm", "--version"), "fixture-npm:--version"),
            ):
                with self.subTest(command=command):
                    result = subprocess.run([helper, *command], env=environment,
                                            capture_output=True, text=True)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(result.stdout.strip(), expected)

    def test_sfu_prepare_web_resolves_npm_through_mise_under_systemd_path(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            local_bin = home / ".local/bin"
            node_bin = home / ".local/share/mise/installs/node/25.2.1/bin"
            local_bin.mkdir(parents=True)
            node_bin.mkdir(parents=True)
            npm_log = home / "npm.log"

            def executable(path, text):
                path.write_text(text)
                path.chmod(0o755)

            executable(local_bin / "mise", f"""#!/bin/bash
if [[ $1 == which && $2 == npm ]]; then
  printf '%s\\n' {shlex.quote(str(node_bin / 'npm'))}
  exit 0
fi
exit 1
""")
            executable(node_bin / "npm", f"""#!/bin/bash
printf '%s\\n' "$*" >>{shlex.quote(str(npm_log))}
""")
            compiler = home / "hirsel"
            executable(compiler, """#!/bin/bash
case $1 in
  build) exit 0 ;;
  sdk) [[ $2 == ts ]] && exit 0 ;;
esac
printf 'unexpected compiler command: %s\\n' "$*" >&2
exit 2
""")

            environment = {"HOME": str(home), "PATH": f"{local_bin}:/usr/local/bin:/usr/bin:/bin"}
            run_py = TOOLS.parent / "core/programs/webrtc_sfu/service/run.py"
            result = subprocess.run(
                [sys.executable, run_py, "--hirsel", compiler, "--work", home / "work", "--build-only"],
                env=environment, capture_output=True, text=True,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("exec --yes --package=esbuild@0.25.12", npm_log.read_text())

    def test_pruning_keeps_referenced_and_in_progress_runs(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            tags = ("expired", "current", "promoted", "legacy", "profile", "screen", "weekly",
                    "history", "accepted", "refused", "recent1", "recent2", "recent3")
            runs = {tag: base / "runs" / f"202611{day:02d}T000000Z-Ab1234"
                    for day, tag in enumerate(tags, start=1)}
            for run in runs.values():
                run.mkdir(parents=True)
            toolchain = runs["expired"] / "toolchain"
            toolchain.mkdir()
            (toolchain / "artifact").touch()
            toolchain.chmod(0o555)

            def complete(tag, status, mtime):
                report = runs[tag] / "board" / "board.json"
                report.parent.mkdir()
                report.write_text(json.dumps({"complete": True, "commit": tag, "exit_status": status}))
                report.with_suffix(".md").touch()
                os.utime(report, (mtime, mtime))
                return report

            promoted = complete("promoted", 0, 1)
            legacy = complete("legacy", 0, 2)
            complete("accepted", 3, 3)
            complete("refused", 2, 4)
            (base / "last-complete.json").symlink_to(promoted.relative_to(base))
            (base / "last-complete.v2.json").symlink_to(legacy)
            (base / "last-screen-inputs").symlink_to(runs["screen"])
            (base / "baselines").mkdir()
            (base / "baselines/profile.json").write_text(json.dumps({"cwd": str(runs["profile"] / "images")}))
            (base / "weekly-main").mkdir()
            (base / "weekly-main/nightly-input").symlink_to(runs["weekly"])
            subprocess.run([sys.executable, TOOLS / "board-history.py", "append", "--base", base], check=True)
            with sqlite3.connect(base / "history.sqlite") as history:
                history.execute("INSERT INTO samples VALUES (?, ?, ?, ?, ?, ?, ?, ?)", (
                    runs["history"].name, "goal", "row", "side", "metric",
                    json.dumps([str(runs["history"] / "data")]), 1, None,
                ))

            subprocess.run([sys.executable, TOOLS / "board-freshness.py", "--base", base,
                            "--prune", runs["current"]], capture_output=True, check=True)
            self.assertEqual(set((base / "runs").iterdir()), {runs[tag] for tag in (
                "current", "promoted", "legacy", "profile", "screen", "weekly", "history", "accepted",
                "refused", "recent1", "recent2", "recent3",
            )})

    def test_only_a_complete_nightly_sweep_counts_towards_the_26_hour_limit(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            nightly = base / "nightly"
            older = nightly / "older"
            older.mkdir(parents=True)
            newer = nightly / "newer"
            newer.mkdir()
            (newer / "prepare.jsonl").touch()
            command = [sys.executable, TOOLS / "board-freshness.py", "--base", base,
                       "--nightly-dir", nightly, "--alert"]
            self.assertIn("NIGHTLY-STALE age unavailable", subprocess.check_output(command, text=True))
            complete = older / "complete"
            complete.touch()
            before = time.time() - 26 * 3600 - 60
            os.utime(complete, (before, before))
            self.assertIn("NIGHTLY-STALE 26.0 h old", subprocess.check_output(command, text=True))
            (newer / "complete").touch()
            self.assertNotIn("NIGHTLY-STALE", subprocess.check_output(command, text=True))

    def test_locks_release_with_a_live_build_daemon(self):
        with tempfile.TemporaryDirectory() as directory, socket.socket(socket.AF_UNIX) as listener:
            base = Path(directory)
            main = base / "main"
            subprocess.run(["git", "init", "-q", main], check=True)
            subprocess.run(["git", "-C", main, "-c", "user.name=Fixture", "-c", "user.email=fixture@example.com",
                            "commit", "--allow-empty", "-qm", "Measured commit"], check=True)
            subprocess.run(["git", "-C", main, "update-ref", "refs/remotes/origin/main", "HEAD"], check=True)
            address = base / "daemon.sock"
            listener.bind(str(address))
            listener.settimeout(5)
            listener.listen()
            taskset = base / "taskset"
            taskset.write_text(r'''#!/bin/bash
shift 2
case "$1 $2 $3" in
  git*) exit 0 ;;
  'python3 bench/awfy/screen.py prepare') mkdir -p "$4" ;;
  'python3 bench/awfy/board.py '*) exit 1 ;;
  'python3 tools/buck2/prepare.py ') exit 0 ;;
  '.buck2/bin/buck2 build -j4')
    [[ "$5" == //:hirsel-toolchain ]] || exit 1
    flock -n "$HIRSEL_BOARD_BASE/baseline.lock" true
    echo "$?" >"$HIRSEL_BOARD_BASE/aux-baseline-status"
    python3 -c 'import socket, sys
with socket.socket(socket.AF_UNIX) as daemon:
    daemon.settimeout(15)
    daemon.connect(sys.argv[1])
    daemon.recv(1)' "$FIXTURE_DAEMON" </dev/null >/dev/null 2>&1 &
    exit 1 ;;
  *) exit 91 ;;
esac
''')
            taskset.chmod(0o755)
            result = subprocess.run(
                [TOOLS / "nightly-board.sh"], capture_output=True, text=True, timeout=10,
                env=os.environ | {"HIRSEL_BOARD_BASE": str(base),
                                  "HIRSEL_BOARD_CURRENT": str(base / "CURRENT-BOARD.md"),
                                  "FIXTURE_DAEMON": str(address), "PATH": f"{base}:{os.environ['PATH']}"},
            )
            self.assertEqual(result.returncode, 1, result.stderr)
            daemon, _ = listener.accept()
            with daemon:
                # A live connection keeps the fixture daemon around while both locks are probed.
                lock_statuses = {name: subprocess.run(["flock", "-n", base / name, "true"]).returncode
                                 for name in ("nightly.lock", "baseline.lock")}
                self.assertEqual(lock_statuses, {"nightly.lock": 0, "baseline.lock": 0})
                self.assertEqual((base / "aux-baseline-status").read_text().strip(), "0")

    def test_laws_and_vopr_run_after_board_and_preserve_its_verdict(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            main = base / "main"
            main.mkdir()
            subprocess.run(["git", "init", "-q", main], check=True)
            for message in ("Previous board", "Measured commit"):
                subprocess.run(["git", "-C", main, "-c", "user.name=Fixture", "-c", "user.email=fixture@example.com",
                                "commit", "--allow-empty", "-qm", message], check=True)
            previous = subprocess.check_output(["git", "-C", main, "rev-parse", "HEAD^"], text=True).strip()
            subprocess.run(["git", "-C", main, "update-ref", "refs/remotes/origin/main", "HEAD"], check=True)
            previous_board = base / "baselines"
            previous_board.mkdir()
            previous_snapshot = previous_board / f"{previous}.json"
            previous_snapshot.write_text(
                json.dumps({"complete": True, "commit": previous, "exit_status": 0}),
            )
            previous_snapshot.with_suffix(".md").write_text("Previous complete board\n")
            taskset = base / "taskset"
            taskset.write_text(r'''#!/bin/bash
shift 2
case "$1 $2 $3" in
  git*) exit 0 ;;
  'python3 bench/awfy/screen.py prepare') mkdir -p "$4" ;;
  'python3 bench/awfy/screen.py manifest') echo '{}' ;;
  'python3 bench/awfy/board.py rebaseline')
    python3 -c 'from pathlib import Path
from board_baseline import promote
import sys
run, base, previous, inode = sys.argv[1:]
promote(Path(run) / "board/board.json", Path(base), Path(previous) if previous else "", inode)' "$4" "$6" "$8" "${10}" ;;
  'python3 bench/awfy/board.py '*)
    mkdir -p "$3"
    printf '{"complete":true,"commit":"%s","exit_status":%s}\n' \
      "$(git rev-parse HEAD)" "$FIXTURE_BOARD_STATUS" >"$3/board.json"
    touch "$3/board.md" "${HIRSEL_BOARD_BASE}/board-completed"
    exit "$FIXTURE_BOARD_STATUS" ;;
  'python3 tools/buck2/prepare.py ') exit 0 ;;
  '.buck2/bin/buck2 build -j4') echo '{"root//:hirsel-toolchain":"prefix"}' ;;
  'python3 core/tools/law_board.py check')
    [[ -f "${HIRSEL_BOARD_BASE}/board-completed" && "$4" == --compiler && "$6" == --compiler-scratch ]] || exit 90
    exit 1 ;;
  'python3 core/tools/law_board.py nightly')
    [[ -f "${HIRSEL_BOARD_BASE}/board-completed" && "$8" == --commit && "$9" == "$FIXTURE_PREVIOUS" ]] || exit 90
    echo "fixture: weaker property row"
    exit 0 ;;
  'python3 core/tools/sim_campaign.py --compiler')
    [[ "$7" == --prepare-results && "$8/prepared" == "$6" ]] || exit 90
    mkdir -p "$6"
    echo '[{"law":"ready"}]' >"$6/manifest.json"
    exit "$FIXTURE_PREPARE_STATUS" ;;
  */tools/boat-dispatch\ *) exit 7 ;;
  */tools/sim-campaign\ batch\ *)
    [[ -f "${HIRSEL_BOARD_BASE}/board-completed" && "$4" == prepared/manifest.json && -s "$4" ]] || exit 90
    echo "fixture: dispatched ready law"
    exit "$FIXTURE_VOPR_STATUS" ;;
  *) exit 91 ;;
esac
''')
            taskset.chmod(0o755)
            for board_status, prepare_status, batch_status in ((0, 0, 0), (0, 1, 0), (0, 0, 75), (2, 0, 0)):
                with self.subTest(board_status=board_status, prepare_status=prepare_status, batch_status=batch_status):
                    campaign_status = prepare_status or batch_status
                    (base / "board-completed").unlink(missing_ok=True)
                    (base / "last-screen-inputs").unlink(missing_ok=True)
                    if campaign_status != 0:
                        (base / "last-screen-inputs").symlink_to(previous_board)
                    (base / "last-complete.json").unlink(missing_ok=True)
                    (base / "last-complete.json").symlink_to(previous_snapshot)
                    os.utime(previous_snapshot)
                    baseline_bytes = (base / "last-complete.json").read_bytes()
                    commit = subprocess.check_output(["git", "-C", main, "rev-parse", "HEAD"], text=True).strip()
                    snapshot = base / "baselines" / f"{commit}.json"
                    snapshot.unlink(missing_ok=True)
                    result = subprocess.run(
                        [TOOLS / "nightly-board.sh", "--locked", "--fetched"], capture_output=True, text=True,
                        env=os.environ | {"HIRSEL_BOARD_BASE": str(base),
                                          "HIRSEL_BOARD_CURRENT": str(base / "CURRENT-BOARD.md"),
                                          "HIRSEL_VOPR_LEDGER": str(base / "seeds.sqlite"),
                                          "FIXTURE_BOARD_STATUS": str(board_status),
                                          "FIXTURE_PREPARE_STATUS": str(prepare_status),
                                          "FIXTURE_VOPR_STATUS": str(batch_status),
                                          "FIXTURE_PREVIOUS": previous,
                                          "PYTHONPATH": str(TOOLS.parent / "bench/awfy"),
                                          "PATH": f"{base}:{os.environ['PATH']}"},
                    )
                    self.assertEqual(result.returncode, board_status, result.stderr)
                    current = (base / "CURRENT-BOARD.md").read_text()
                    self.assertIn(f"**Passed** · {'yes' if board_status == 0 else 'no'}", current)
                    boat_status = 0 if campaign_status == 0 else 7
                    self.assertIn(f"Boat exit {boat_status} · VOPR exit {campaign_status}", current)
                    self.assertIn("Laws exit 1", current)
                    run = Path(result.stdout.strip())
                    log = (run / "run.log").read_text()
                    if board_status == 2:
                        self.assertEqual((base / "last-complete.json").resolve(), previous_snapshot)
                        self.assertEqual((base / "last-complete.json").read_bytes(), baseline_bytes)
                        self.assertFalse(snapshot.exists())
                        self.assertIn("Comparison refused. Baseline retained.", log)
                    else:
                        self.assertEqual((base / "last-complete.json").resolve(), snapshot)
                        self.assertEqual(snapshot.read_bytes(), (run / "board/board.json").read_bytes())
                    self.assertIn("fixture: weaker property row", log)
                    self.assertIn("Law lock check exited 1", log)
                    self.assertIn("fixture: dispatched ready law", log)
                    if campaign_status == 1:
                        self.assertIn("VOPR campaign exited 1", log)
                        self.assertIn("VOPR batch exited 0 after partial preparation", log)
                    if campaign_status == 75:
                        self.assertIn("VOPR deferred", log)

    def test_service_failure_writes_dated_alert_and_followup(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            followups = base / "followups.md"
            followups.write_text("Existing entry\n")
            culprit = "Culprit fc72987650b1cd38f85493120edf157d2d8c2343 arena/Json::ParseMapping\n"
            expected = {}
            for status, outcome in ((1, "incomplete"), (2, "refused"), (3, "regressed")):
                if status == 3:
                    with path.open("a") as alert:
                        alert.write(culprit)
                    expected[path] += culprit
                result = subprocess.check_output(
                    [sys.executable, TOOLS / "board-freshness.py", "--base", base, "--service-failure", followups],
                    env=os.environ | {"MONITOR_EXIT_CODE": "exited", "MONITOR_EXIT_STATUS": str(status),
                                      "MONITOR_INVOCATION_ID": "a" * 32},
                    text=True,
                )
                self.assertRegex(result, r"^\d{4}-\d{2}-\d{2} .* UTC Hirsel nightly board ")
                self.assertIn(f"{outcome} (exit {status})", result)
                path = base / "alerts" / f"{result[:10]}.txt"
                expected[path] = expected.get(path, "") + result
                for alert, contents in expected.items():
                    self.assertEqual(alert.read_text(), contents)
                self.assertIn("- " + result, followups.read_text())
            weekly = subprocess.check_output(
                [sys.executable, TOOLS / "board-freshness.py", "--base", base, "--service-failure", followups,
                 "--service-failure-tier", "weekly"],
                env=os.environ | {"MONITOR_EXIT_CODE": "exited", "MONITOR_EXIT_STATUS": "1",
                                  "MONITOR_INVOCATION_ID": ""},
                text=True,
            )
            self.assertIn("Hirsel weekly board incomplete (exit 1)", weekly)
            self.assertIn("-u hirsel-board-weekly.service", weekly)
            self.assertEqual((base / "alerts" / f"{weekly[:10]}-weekly.txt").read_text(), weekly)
            for alert, contents in expected.items():
                self.assertEqual(alert.read_text(), contents)
            self.assertIn("- " + weekly, followups.read_text())
            vopr = subprocess.check_output(
                [sys.executable, TOOLS / "board-freshness.py", "--base", base, "--service-failure", followups],
                env=os.environ | {"MONITOR_UNIT": "hirsel-vopr-nightly.service", "MONITOR_EXIT_CODE": "exited",
                                  "MONITOR_EXIT_STATUS": "2", "MONITOR_INVOCATION_ID": ""},
                text=True,
            )
            self.assertIn("Hirsel nightly VOPR sweep failed (exit 2)", vopr)
            self.assertIn("-u hirsel-vopr-nightly.service", vopr)
            self.assertEqual((base / "alerts" / f"{vopr[:10]}-vopr.txt").read_text(), vopr)
            self.assertIn("- " + vopr, followups.read_text())
            self.assertTrue(followups.read_text().startswith("Existing entry\n"))

    def test_failed_fetch_preserves_last_complete_and_alerts_when_stale(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            main = base / "main"
            main.mkdir()

            def git(*args):
                return subprocess.check_output(["git", "-C", main, *args], text=True).strip()

            git("init", "-q")
            for message in ("Measured commit", "New main commit"):
                git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.com",
                    "commit", "--allow-empty", "-qm", message)
            commit = git("rev-parse", "HEAD^")
            head = git("rev-parse", "HEAD")
            git("update-ref", "refs/remotes/origin/main", head)
            current = base / "CURRENT-BOARD.md"
            nightly = base / "vopr/nightly"
            (nightly / "recent").mkdir(parents=True)
            (nightly / "recent/complete").touch()

            def alert():
                return subprocess.check_output(
                    [sys.executable, TOOLS / "board-freshness.py", "--base", base,
                     "--current", current, "--alert", "--nightly-dir", nightly], text=True,
                )

            self.assertIn("BOARD-STALE age unavailable", alert())
            complete = base / "complete"
            complete.mkdir()
            board = complete / "board.json"
            board.write_text(json.dumps({"commit": commit, "complete": True, "exit_status": 0}))
            (complete / "board.md").write_text("Complete results\n")
            (base / "last-complete.json").symlink_to(board)
            incomplete = base / "runs/20000101T000000Z-old/board"
            incomplete.mkdir(parents=True)
            (incomplete / "board.json").write_text(
                json.dumps({"commit": head, "complete": False, "exit_status": 2}),
            )
            (incomplete / "board.md").write_text("Refused results\n")

            recent = time.time() - 25 * 3600
            os.utime(board, (recent, recent))
            self.assertEqual(alert(), "")
            old = time.time() - 27 * 3600
            os.utime(board, (old, old))
            self.assertIn("BOARD-STALE 27.0 h old", alert())

            peer = base / "peers/langarena/rust"
            peer.mkdir(parents=True)
            (peer / "Cargo.toml").touch()
            taskset = base / "taskset"
            taskset.write_text(
                '#!/bin/bash\nshift 2\n'
                'case "$1" in\n'
                '  git) exit 0 ;;\n'
                '  python3) [[ "$2 $3" == "bench/awfy/screen.py prepare" ]] && mkdir -p "$4" ;;\n'
                '  */tools/boat-dispatch) exit 0 ;;\n'
                '  cargo) echo "fixture: cargo fetch failed" >&2; exit 2 ;;\n'
                '  *) exit 90 ;;\n'
                'esac\n',
            )
            taskset.chmod(0o755)
            result = subprocess.run(
                [TOOLS / "nightly-board.sh"], capture_output=True, text=True,
                env=os.environ | {"HIRSEL_BOARD_BASE": str(base), "HIRSEL_BOARD_CURRENT": str(current),
                                  "PATH": f"{base}:{os.environ['PATH']}"},
            )
            self.assertEqual(result.returncode, 2, result.stderr)
            self.assertIn("fixture: cargo fetch failed", result.stdout)
            self.assertIn("cargo +1.99.0 fetch langarena/rust/Cargo.toml", result.stderr)
            shown = current.read_text()
            self.assertIn(f"**Latest attempt** · `{head}`", shown)
            self.assertIn("Failed · cargo +1.99.0 fetch langarena/rust/Cargo.toml (exit 2)", shown)
            self.assertIn(f"**Last complete board** · `{commit}`", shown)
            self.assertIn("1 commits behind origin/main", shown)
            self.assertIn("Complete results", shown)
            self.assertNotIn("Refused results", shown)
            self.assertEqual((base / "last-complete.json").resolve(), board)

    def test_refused_board_is_fresh_and_complete(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            run = base / "runs/20261124T020000Z-refused"
            board = run / "board"
            board.mkdir(parents=True)
            (board / "board.json").write_text(json.dumps(
                {"commit": "refused-commit", "complete": True, "exit_status": 2},
            ))
            (board / "board.md").write_text("Refused results\n")
            current = base / "CURRENT-BOARD.md"
            nightly = base / "vopr/nightly"
            (nightly / "recent").mkdir(parents=True)
            (nightly / "recent/complete").touch()
            for recovered in (False, True):
                if recovered:
                    (run / "attempt.json").write_text(json.dumps({
                        "commit": "refused-commit", "started": time.time(), "status": 0, "step": "rebaseline",
                    }))
                alert = subprocess.check_output(
                    [sys.executable, TOOLS / "board-freshness.py", "--base", base,
                     "--current", current, "--alert", "--nightly-dir", nightly], text=True,
                )
                self.assertEqual(alert, "")
                shown = current.read_text()
                fields = ("**Fresh** · yes", "**Complete** · yes", "**Passed** · no",
                          "**Refused** · yes", f"**Recovered** · {'yes' if recovered else 'no'}")
                for field in fields:
                    self.assertIn(field, shown)
                print(" | ".join(fields))


if __name__ == "__main__":
    unittest.main()
