#!/usr/bin/env bash
set -euo pipefail
export CARGO_TARGET_DIR=/workspace/.cargo-target-hirsel-rbcov

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd -- "$SCRIPT_DIR/../../.." && pwd)"
# shellcheck source=../lib/runbook-lib.sh
source "$ROOT/e2e/protocol-compatibility/lib/runbook-lib.sh"

PORT="$(choose_port 3160)"
DATA_DIR="/tmp/hirsel-e2e-attachment-agent-data"
HOST_LOG="/tmp/hirsel-e2e-attachment-agent-host.log"
HIRSEL_AGENT=lash
HIRSEL_PROVIDER=codex
HIRSEL_DRIVER=fake
HIRSEL_MODEL=gpt-5.6-sol
trap 'stop_hirsel_host TERM' EXIT

ART_DIR="/tmp/hirsel-e2e-attachment-agent-artifacts"
rm -rf "$ART_DIR"
mkdir -p "$ART_DIR"
python3 - <<'PY'
from PIL import Image, ImageDraw, ImageFont
from pathlib import Path
out = Path("/tmp/hirsel-e2e-attachment-agent-artifacts/lime.png")
img = Image.new("RGB", (320, 120), "white")
draw = ImageDraw.Draw(img)
try:
    font = ImageFont.truetype("DejaVuSans-Bold.ttf", 62)
except Exception:
    font = ImageFont.load_default()
draw.text((70, 24), "LIME", fill="black", font=font)
img.save(out)
Path("/tmp/hirsel-e2e-attachment-agent-artifacts/note.txt").write_text(
    "first line\nSECOND-LINE-TOKEN-8842\nthird line\n",
    encoding="utf-8",
)
PY

start_hirsel_host fresh
post_json debug/reset '{}' >/dev/null
pass_gate "debug reset"

PNG_B64="$(base64 -w0 "$ART_DIR/lime.png")"
TXT_B64="$(base64 -w0 "$ART_DIR/note.txt")"
PNG_JSON="$(post_json debug/upload "$(jq -nc --arg data "$PNG_B64" '{name:"lime.png",mime:"image/png",data_b64:$data}')")"
TXT_JSON="$(post_json debug/upload "$(jq -nc --arg data "$TXT_B64" '{name:"note.txt",mime:"text/plain",data_b64:$data}')")"
PNG_ID="$(printf '%s' "$PNG_JSON" | jq -r '.id')"
TXT_ID="$(printf '%s' "$TXT_JSON" | jq -r '.id')"
pass_gate "uploaded png=$PNG_ID text=$TXT_ID"

BODY='Two attachments are included. Inspect the image and identify the word in it. Reply with IMAGE_WORD=<word> and acknowledge that note.txt is attached. Do not quote or infer the text file contents.'
OWNER_JSON="$(post_json debug/owner-message "$(jq -nc --arg body "$BODY" --arg png "$PNG_ID" --arg txt "$TXT_ID" '{client_id:"attachment-agent-behavior",body:$body,ref:null,attachments:[$png,$txt]}')")"
OWNER_ID="$(printf '%s' "$OWNER_JSON" | jq -r '.message.id')"

wait_jq debug/chat '.messages[] | select(.id == '"$OWNER_ID"' and .author == "owner" and (.attachments | length == 2) and .attachments[0].name == "lime.png" and .attachments[0].mime == "image/png" and .attachments[1].name == "note.txt" and .attachments[1].mime == "text/plain")' 10 >/dev/null
pass_gate "owner chat replay includes exact image and text attachment metadata"

wait_agent_message_after "$OWNER_ID" '(.body | ascii_downcase | contains("lime"))' 240
pass_gate "Agent persisted an attachment-aware reply"

if get_json debug/chat | jq -e '.messages[] | select(.author == "agent" and .id > '"$OWNER_ID"' and (.body | ascii_downcase | contains("lime")))' >/dev/null; then
  pass_gate "Agent identified LIME in image"
else
  debug_snapshot /tmp/hirsel-e2e-attachment-agent-vision-miss
  fail_gate "Agent did not identify LIME in image"
  exit 2
fi

debug_snapshot /tmp/hirsel-e2e-attachment-agent-final
