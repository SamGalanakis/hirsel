set shell := ["bash", "-cu"]

default:
    @just --list

# Dev loop: host auto-restarts on Rust changes, PWA dev server with HMR.
# Usage: just dev [port]   (host port; PWA HMR is on 5173 — open that one)
# Env overrides pass through: HIRSEL_PROVIDER, HIRSEL_DRIVER, HIRSEL_TOKEN, HIRSEL_MODEL...
dev port="3089":
    #!/usr/bin/env bash
    set -euo pipefail
    trap 'kill 0 2>/dev/null' EXIT INT TERM
    export HIRSEL_LISTEN="127.0.0.1:{{port}}"
    export HIRSEL_TOKEN="${HIRSEL_TOKEN:-dev-token}"
    export HIRSEL_DEBUG="${HIRSEL_DEBUG:-1}"
    export HIRSEL_PROVIDER="${HIRSEL_PROVIDER:-codex}"
    export HIRSEL_DATA_DIR="${HIRSEL_DATA_DIR:-./data}"
    echo ""
    echo "  hirsel host   http://127.0.0.1:{{port}}      (WS/API + debug; token: $HIRSEL_TOKEN, provider: $HIRSEL_PROVIDER)"
    echo "  PWA w/ HMR    http://127.0.0.1:5173      <- open this one"
    echo ""
    # Rust auto-restart: entr snapshots the file list at start; run `just dev`
    # again after adding new .rs files.
    ( find crates prompts -name '*.rs' -o -name 'Cargo.toml' -o -name 'agent.md' | entr -rn cargo run -p hirsel-host ) &
    ( cd app && VITE_WS_URL="ws://127.0.0.1:{{port}}/ws" npm run dev -- --port 5173 --strictPort )

# Run the real thing, no watchers: builds the PWA, host serves it. Usage: just run [port]
run port="3089":
    #!/usr/bin/env bash
    set -euo pipefail
    ( cd app && npm run build )
    export HIRSEL_LISTEN="127.0.0.1:{{port}}"
    export HIRSEL_TOKEN="${HIRSEL_TOKEN:-dev-token}"
    export HIRSEL_PROVIDER="${HIRSEL_PROVIDER:-codex}"
    export HIRSEL_DATA_DIR="${HIRSEL_DATA_DIR:-./data}"
    echo ""
    echo "  hirsel        http://127.0.0.1:{{port}}      (token: $HIRSEL_TOKEN, provider: $HIRSEL_PROVIDER)"
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
