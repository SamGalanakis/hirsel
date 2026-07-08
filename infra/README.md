# Deploying hirsel on the VM

One-time setup (after the first merge to main):

1. `cargo build --release -p hirsel-host && sudo cp target/release/hirsel-host /usr/local/bin/`
   (build with the PWA first: `cd app && npm ci && npm run build` — the host serves `app/dist`)
2. `sudo mkdir -p /etc/hirsel` and create `/etc/hirsel/env`:
   ```
   HIRSEL_TOKEN=<long random>
   HIRSEL_PROVIDER=anthropic          # or codex
   ANTHROPIC_API_KEY=<key>            # if provider=anthropic
   HIRSEL_MODEL=claude-opus-4-8
   HIRSEL_DATA_DIR=/var/lib/hirsel
   HIRSEL_LISTEN=127.0.0.1:8420
   ```
3. `sudo cp infra/hirsel.service /etc/systemd/system/ && sudo systemctl enable --now hirsel`
4. Install caddy, put `infra/Caddyfile` (with the real subdomain) at `/etc/caddy/Caddyfile`, `sudo systemctl reload caddy`.

Inputs that are Sam's call: the subdomain (DNS record to this VM) and the provider credentials. Until caddy is up, the host works locally (`HIRSEL_LISTEN=0.0.0.0:8420` + plain `ws://` for LAN testing; service worker/push need the HTTPS origin).

iroh transport is milestone 2 (ADR-0006) and replaces the caddy/DNS requirement when it lands.
