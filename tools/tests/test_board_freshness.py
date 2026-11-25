"""Exercise the nightly wrapper and the missed-run check against a fixture board."""
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time
import unittest

TOOLS = Path(__file__).resolve().parents[1]


class BoardFreshnessTest(unittest.TestCase):
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
            previous_board = base / "previous-board"
            previous_board.mkdir()
            (previous_board / "board.json").write_text(
                json.dumps({"complete": True, "commit": previous, "exit_status": 0}),
            )
            (previous_board / "board.md").write_text("Previous complete board\n")
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
run, base, previous = sys.argv[1:]
promote(Path(run) / "board/board.json", Path(base), Path(previous) if previous else "")' "$4" "$6" "$8" ;;
  'python3 bench/awfy/board.py '*)
    mkdir -p "$3"
    printf '{"complete":true,"commit":"%s","exit_status":0}\n' "$(git rev-parse HEAD)" >"$3/board.json"
    touch "$3/board.md" "${HIRSEL_BOARD_BASE}/board-completed" ;;
  'python3 tools/buck2/prepare.py ') exit 0 ;;
  '.buck2/bin/buck2 build -j4') echo '{"root//:hirsel-toolchain":"prefix"}' ;;
  'python3 core/tools/law_board.py check')
    [[ -f "${HIRSEL_BOARD_BASE}/board-completed" && "$4" == --compiler && $# == 5 ]] || exit 90
    exit 0 ;;
  'python3 core/tools/law_board.py nightly')
    [[ -f "${HIRSEL_BOARD_BASE}/board-completed" && "$6" == --commit && "$7" == "$FIXTURE_PREVIOUS" ]] || exit 90
    echo "fixture: weaker property row"
    exit 1 ;;
  'python3 core/tools/sim_campaign.py --compiler') exit 0 ;;
  */tools/boat-dispatch\ *) exit 7 ;;
  */tools/sim-campaign\ batch\ *)
    [[ -f "${HIRSEL_BOARD_BASE}/board-completed" && "$4" == prepared/manifest.json ]] || exit 90
    exit "$FIXTURE_VOPR_STATUS" ;;
  *) exit 91 ;;
esac
''')
            taskset.chmod(0o755)
            for campaign_status in (0, 1, 75):
                with self.subTest(campaign_status=campaign_status):
                    (base / "board-completed").unlink(missing_ok=True)
                    (base / "last-screen-inputs").unlink(missing_ok=True)
                    if campaign_status != 0:
                        (base / "last-screen-inputs").symlink_to(previous_board)
                    (base / "last-complete.json").unlink(missing_ok=True)
                    (base / "last-complete.json").symlink_to(previous_board / "board.json")
                    os.utime(previous_board / "board.json")
                    result = subprocess.run(
                        [TOOLS / "nightly-board.sh"], capture_output=True, text=True,
                        env=os.environ | {"HIRSEL_BOARD_BASE": str(base),
                                          "HIRSEL_BOARD_CURRENT": str(base / "CURRENT-BOARD.md"),
                                          "HIRSEL_VOPR_LEDGER": str(base / "seeds.sqlite"),
                                          "FIXTURE_VOPR_STATUS": str(campaign_status),
                                          "FIXTURE_PREVIOUS": previous,
                                          "PYTHONPATH": str(TOOLS.parent / "bench/awfy"),
                                          "PATH": f"{base}:{os.environ['PATH']}"},
                    )
                    self.assertEqual(result.returncode, 0, result.stderr)
                    current = (base / "CURRENT-BOARD.md").read_text()
                    self.assertIn("**Passed** · yes", current)
                    boat_status = 0 if campaign_status == 0 else 7
                    self.assertIn(f"Boat exit {boat_status} · VOPR exit {campaign_status}", current)
                    self.assertIn("Laws exit 1", current)
                    run = Path(result.stdout.strip())
                    self.assertEqual((base / "last-complete.json").resolve(), run / "board/board.json")
                    log = (run / "run.log").read_text()
                    self.assertIn("fixture: weaker property row", log)
                    if campaign_status == 1:
                        self.assertIn("VOPR campaign exited 1", log)
                    if campaign_status == 75:
                        self.assertIn("VOPR deferred", log)

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

            def alert():
                return subprocess.check_output(
                    [sys.executable, TOOLS / "board-freshness.py", "--base", base,
                     "--current", current, "--alert"], text=True,
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
            for recovered in (False, True):
                if recovered:
                    (run / "attempt.json").write_text(json.dumps({
                        "commit": "refused-commit", "started": time.time(), "status": 0, "step": "rebaseline",
                    }))
                alert = subprocess.check_output(
                    [sys.executable, TOOLS / "board-freshness.py", "--base", base,
                     "--current", current, "--alert"], text=True,
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
