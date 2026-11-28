# Linux Store soak

Build a native Store workload against the same checkout's glibc runtime. Use an installed Hirsel distribution for the operator commands, which need its archive and Store helpers. The suite uses a signed loopback archive with separate writer, reader and expiry roles. Its credentials are fixture values.

```sh
python3 tools/buck2/prepare.py
.buck2/bin/buck2 build -j 6 --show-output //:hirsel-core-driver-bin-hirsel //:core-driver-runtime-pic //:hirsel-toolchain
python3 tools/store-soak/build.py "$DRIVER" "$RUNTIME" "$BIN/soak-place" --cc tools/buck2/toolchains/llvm-files/bin/hirsel-clang
python3 tools/store-soak/gates.py positive "$RUN_PARENT/positive"
```

The last command must exit 1 with the private-memory slope diagnostic. The supervisor independently checks its receipt chain and recomputes the rejection before it starts any run.

Mount a private 128 MiB tmpfs for Place B and leave Place A, the evidence and the archive on a different device. Use a fresh run path. Run under a user systemd service with a delegated memory controller so each Place has its own cgroup with a 2 GiB limit and no swap.

```sh
mkdir -p "$PRESSURE"
sudo mount -t tmpfs -o size=128m,mode=0700,uid="$(id -u)",gid="$(id -g)",nosuid,nodev,noexec soak-pressure "$PRESSURE"
systemd-run --user --unit=hirsel-store-soak-dry --wait --pipe --collect \
  -p Delegate=yes --working-directory="$PWD" \
  python3 "$PWD/tools/store-soak/supervise.py" dry --revision "$(git rev-parse HEAD)" \
  --run "$RUN_PARENT/dry" --pressure-volume "$PRESSURE" \
  --place "$BIN/soak-place" --hirsel "$BUNDLE/hirsel" --positive "$RUN_PARENT/positive"
python3 tools/store-soak/gates.py check "$RUN_PARENT/dry/receipts"
```

The dry run takes about two minutes after warm-up. It runs a crash with one unacknowledged operation in flight, natural ENOSPC during a checkpoint, two deep archive verifications and a reviewed expiry. Change the crash offset with `--dry-crash`. Preserve each fault copy and both external acknowledgement ledgers under the run's `evidence/` directory. Use a fresh mount or empty Place B directory for the next run.

Use `control` with a new run directory and remove `--wait --pipe` to run 24 hours independently of SSH. The detached unit emits its logs through `journalctl --user -u UNIT`. Run `sync_receipts.py USER@HOST:/absolute/run/receipts LOCAL_DIRECTORY` on the receiving host to copy atomic receipt files every 30 seconds. `gates.py check --live RECEIPTS` verifies the current prefix.

After 24 hours, freeze a complete control with `gates.py freeze RECEIPTS ENVELOPES.json`. Use `measured --envelopes ENVELOPES.json` to start a 72-hour run with the same executable, host and workload. Compare complete measured receipts with `gates.py evaluate RECEIPTS ENVELOPES.json`. Each full day includes two-hourly Place B crashes, isolated pressure at hour 12, archive expiry at hour 13 and deep verification every hour.

The corresponding Store contents are checked against a separately maintained model, including the commit chain, bounded logical rows, SHA-256 of observed blob bytes, and trimmed events. At each quiescent point the sampler uses Linux anonymous PSS and private clean, dirty and huge-page RSS from `smaps_rollup`. It records the full PSS and RSS alongside them. Allocated Store bytes exclude the loopback archive and preserved copies. The limit on Store resident charge is 768 MiB. The declared absolute runtime ceilings are 2 GiB private PSS and RSS, 4,096 VMAs, 128× allocated-to-logical Store bytes and 1,000 ms rolling p99. Frozen noisy limits come from hours 3 through 24. The memory slope ceiling is 1 MiB/h. Exact checks run at each post-drain sample.

Both Stores use the Directory backend. Place A uses a Tiered log. Place B uses a Memory-resident log so its crash and pressure checks measure Directory recovery without an intentionally retained Tiered generation. The fixture exercises the Directory archive lane and the Store branch follower on a temporary fork. The Bucket host-stream janitor needs a separate workload with an attached Bucket authority and stream slot.
