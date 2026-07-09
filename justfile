set shell := ["bash", "-cu"]
set positional-arguments

default:
    @just --list

# Dev loop: host auto-restarts on Rust changes, PWA dev server with HMR.
# Usage: just dev [port]   (host port; PWA HMR is on 5173 — open that one)
# Env overrides pass through: HIRSEL_PROVIDER, HIRSEL_DRIVER, HIRSEL_TOKEN, HIRSEL_MODEL...
dev port="3089":
    #!/usr/bin/env bash
    set -euo pipefail
    trap 'kill 0 2>/dev/null' EXIT INT TERM
    export HIRSEL_LISTEN="127.0.0.1:{{ port }}"
    export HIRSEL_TOKEN="${HIRSEL_TOKEN:-dev-token}"
    export HIRSEL_DEBUG="${HIRSEL_DEBUG:-1}"
    export HIRSEL_PROVIDER="${HIRSEL_PROVIDER:-codex}"
    export HIRSEL_DATA_DIR="${HIRSEL_DATA_DIR:-./data}"
    echo ""
    echo "  hirsel host   http://127.0.0.1:{{ port }}      (WS/API + debug; token: $HIRSEL_TOKEN, provider: $HIRSEL_PROVIDER)"
    echo "  PWA w/ HMR    http://127.0.0.1:5173      <- open this one"
    echo ""
    # Rust auto-restart: entr snapshots the file list at start; run `just dev`
    # again after adding new .rs files.
    ( find crates prompts -name '*.rs' -o -name 'Cargo.toml' -o -name 'agent.md' | entr -rn cargo run -p hirsel-host ) &
    ( cd app && VITE_WS_URL="ws://127.0.0.1:{{ port }}/ws" npm run dev -- --port 5173 --strictPort )

# Run the real thing, no watchers: builds the PWA, host serves it. Usage: just run [port]
run port="3089":
    #!/usr/bin/env bash
    set -euo pipefail
    ( cd app && npm run build )
    export HIRSEL_LISTEN="127.0.0.1:{{ port }}"
    export HIRSEL_TOKEN="${HIRSEL_TOKEN:-dev-token}"
    export HIRSEL_PROVIDER="${HIRSEL_PROVIDER:-codex}"
    export HIRSEL_DATA_DIR="${HIRSEL_DATA_DIR:-./data}"
    echo ""
    echo "  hirsel        http://127.0.0.1:{{ port }}      (token: $HIRSEL_TOKEN, provider: $HIRSEL_PROVIDER)"
    echo ""
    cargo run --release -p hirsel-host

build:
    cargo build --release --workspace
    cd app && npm run build

test:
    cargo test --workspace
    cd app && npm test -- --run

check:
    cargo clippy --workspace --all-targets -- -D warnings
    cd app && npx tsc --noEmit

# Headless Android emulator loop. VM paths and tool versions live in
# /workspace/android/env.sh; see docs/android-dev.md.
emu:
    #!/usr/bin/env bash
    set -euo pipefail
    source /workspace/android/env.sh
    serial="emulator-5554"
    runtime_dir="/tmp/hirsel-emu"
    log="$runtime_dir/emulator.log"
    mkdir -p "$runtime_dir"

    if [[ "$(timeout 5 adb -s "$serial" shell getprop sys.boot_completed 2>/dev/null | tr -d '\r' || true)" == "1" ]]; then
        echo "ready"
        exit 0
    fi

    if ! pgrep -f 'emulator.*-avd hirsel-emu.*-port 5554' >/dev/null; then
        if [[ ! -r /dev/kvm || ! -w /dev/kvm ]]; then
            echo "error: /dev/kvm is not accessible; re-login after joining kvm, or run: sg kvm -c 'just emu'" >&2
            exit 1
        fi
        nohup emulator -avd hirsel-emu -port 5554 -no-window -no-audio \
            -gpu swiftshader_indirect -no-boot-anim -no-metrics >"$log" 2>&1 &
        echo $! >"$runtime_dir/emulator.pid"
    fi

    deadline=$((SECONDS + 180))
    while [[ "$(timeout 5 adb -s "$serial" shell getprop sys.boot_completed 2>/dev/null | tr -d '\r' || true)" != "1" ]]; do
        if (( SECONDS >= deadline )); then
            echo "error: emulator did not boot within 180s; log: $log" >&2
            tail -n 80 "$log" >&2 || true
            exit 1
        fi
        if ! pgrep -f 'emulator.*-avd hirsel-emu.*-port 5554' >/dev/null; then
            echo "error: emulator exited before boot; log: $log" >&2
            tail -n 80 "$log" >&2 || true
            exit 1
        fi
        sleep 2
    done
    echo "ready"

emu-stop:
    #!/usr/bin/env bash
    set -euo pipefail
    source /workspace/android/env.sh
    serial="emulator-5554"
    runtime_dir="/tmp/hirsel-emu"

    if timeout 5 adb -s "$serial" get-state >/dev/null 2>&1; then
        timeout 10 adb -s "$serial" emu kill >/dev/null
    elif [[ -f "$runtime_dir/emulator.pid" ]] && kill -0 "$(cat "$runtime_dir/emulator.pid")" 2>/dev/null; then
        kill "$(cat "$runtime_dir/emulator.pid")"
    else
        echo "stopped"
        exit 0
    fi

    deadline=$((SECONDS + 30))
    while pgrep -f 'emulator.*-avd hirsel-emu.*-port 5554' >/dev/null; do
        if (( SECONDS >= deadline )); then
            echo "error: emulator did not stop within 30s" >&2
            exit 1
        fi
        sleep 1
    done
    rm -f "$runtime_dir/emulator.pid"
    echo "stopped"

emu-screenshot out="":
    #!/usr/bin/env bash
    set -euo pipefail
    source /workspace/android/env.sh
    serial="emulator-5554"
    if [[ -z "${1:-}" ]]; then
        out="/tmp/hirsel-emu/screenshot-$(date -u +%Y%m%dT%H%M%SZ).png"
    else
        out="$1"
    fi
    mkdir -p "$(dirname "$out")"
    tmp="$(mktemp "${out}.tmp.XXXXXX")"
    trap 'rm -f "$tmp"' EXIT
    timeout 20 adb -s "$serial" exec-out screencap -p >"$tmp"
    if [[ ! -s "$tmp" ]] || [[ "$(file -b --mime-type "$tmp")" != "image/png" ]]; then
        echo "error: emulator screenshot was not a non-empty PNG" >&2
        exit 1
    fi
    mv "$tmp" "$out"
    trap - EXIT
    readlink -f "$out"

emu-install apk:
    #!/usr/bin/env bash
    set -euo pipefail
    source /workspace/android/env.sh
    apk="$1"
    if [[ ! -f "$apk" ]]; then
        echo "error: APK not found: $apk" >&2
        exit 1
    fi
    timeout 120 adb -s emulator-5554 install -r "$apk"

emu-launch package:
    #!/usr/bin/env bash
    set -euo pipefail
    source /workspace/android/env.sh
    package="$1"
    component="$(timeout 15 adb -s emulator-5554 shell cmd package resolve-activity --brief \
        -a android.intent.action.MAIN -c android.intent.category.LAUNCHER "$package" | tr -d '\r' | tail -n 1)"
    if [[ "$component" != */* ]]; then
        echo "error: no launcher activity found for $package" >&2
        exit 1
    fi
    timeout 30 adb -s emulator-5554 shell am start -W -n "$component"

emu-tap x y:
    source /workspace/android/env.sh
    timeout 10 adb -s emulator-5554 shell input tap "$1" "$2"

emu-text text:
    #!/usr/bin/env bash
    set -euo pipefail
    source /workspace/android/env.sh
    text="$1"
    text="${text// /%s}"
    timeout 10 adb -s emulator-5554 shell input text "$text"

emu-key keyevent:
    source /workspace/android/env.sh
    timeout 10 adb -s emulator-5554 shell input keyevent "$1"

emu-swipe x1 y1 x2 y2:
    source /workspace/android/env.sh
    timeout 10 adb -s emulator-5554 shell input swipe "$1" "$2" "$3" "$4" 300

emu-ui-dump:
    #!/usr/bin/env bash
    set -euo pipefail
    source /workspace/android/env.sh
    remote="/data/local/tmp/hirsel-window.xml"
    timeout 20 adb -s emulator-5554 shell uiautomator dump --compressed "$remote" >/dev/null
    timeout 10 adb -s emulator-5554 exec-out cat "$remote"
    adb -s emulator-5554 shell rm -f "$remote" >/dev/null

emu-logcat pattern="":
    #!/usr/bin/env bash
    set -euo pipefail
    source /workspace/android/env.sh
    log="$(mktemp /tmp/hirsel-logcat.XXXXXX)"
    trap 'rm -f "$log"' EXIT
    timeout 20 adb -s emulator-5554 logcat -d -t 500 -v threadtime >"$log"
    if [[ -n "${1:-}" ]]; then
        rg -i -- "$1" "$log" || true
    else
        cat "$log"
    fi
