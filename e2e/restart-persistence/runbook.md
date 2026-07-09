# Restart Persistence Runbook

## Purpose

Prove the host survives a restart onto an EXISTING data dir — the daily-driver path every other runbook misses by booting fresh. Regression guard for `store_commit_failed: runtime turn host-queue-drain:N ... already committed` (drain replay keys colliding across boots, found live 2026-07-09).

## Setup

One data dir reused across two boots. Real agent (HIRSEL_AGENT=lash HIRSEL_PROVIDER=codex HIRSEL_DRIVER=fake) is the primary variant; scripted is the fallback when no provider credentials exist.

```bash
export DATA=/tmp/hirsel-e2e-restart
rm -rf "$DATA"
BASE=http://127.0.0.1:<verified-free-port>   # check ss -tlnp first; foreign processes squat on this VM
# boot 1
HIRSEL_AGENT=lash HIRSEL_PROVIDER=codex HIRSEL_DRIVER=fake HIRSEL_DEBUG=1 \
HIRSEL_TOKEN=dev-token HIRSEL_DATA_DIR="$DATA" HIRSEL_LISTEN=127.0.0.1:<same-port> <host-binary> &
```

## Scenario

1. Boot 1: send owner message "reply with exactly the word one". Gate: agent chat reply containing `one`; NO agent message containing `Agent turn failed`.
2. Kill the host with SIGTERM. Wait for exit.
3. Boot 2 on the SAME data dir. Gate: `/debug/health` ok.
4. Gate: `/debug/chat` still contains the full boot-1 history (owner message + reply).
5. Send owner message "reply with exactly the word two". Gate: agent chat reply containing `two`, and NO message containing `Agent turn failed` anywhere in the transcript.
6. Repeat steps 2-5 once more (boot 3) — two restarts, since the first restart is the historical failure case and the second catches monotonic-state bugs.

## Success

All gates green across three boots of one data dir. Any `Agent turn failed` chat message or non-2xx debug response is a mechanical product failure.
