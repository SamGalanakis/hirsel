# Attachments Runbook

## Purpose

Prove protocol v1.1 attachments through the debug surface and real WebSocket wire: upload a text
blob and a tiny PNG, send an Owner message referencing both, verify Chat replay includes both
attachments, mint a blob-scoped signed URL with `get_blob_url`, enforce its authentication
boundaries, and confirm the scripted Agent saw the stored-path attachment notes.

## Start Host

Use the shared runner helpers, a verified-free port, and a neutral `/tmp` working directory:

```bash
export ROOT=/workspace/code/hirsel-rbcov
export CARGO_TARGET_DIR=/workspace/.cargo-target-hirsel-rbcov
source "$ROOT/e2e/lib/runbook-lib.sh"
PORT="$(choose_port 3240)"
BASE="http://127.0.0.1:$PORT"
export HIRSEL_AGENT=scripted HIRSEL_PROVIDER=codex HIRSEL_DRIVER=fake
export HIRSEL_MODEL=gpt-5.6-sol HIRSEL_TOKEN=dev-token HIRSEL_DEBUG=1
export HIRSEL_DATA_DIR=/tmp/hirsel-e2e-attachments
rm -rf "$HIRSEL_DATA_DIR"
export HIRSEL_LISTEN="127.0.0.1:$PORT"
mkdir -p /tmp/hirsel-e2e-attachments-work
cd /tmp/hirsel-e2e-attachments-work
exec "$CARGO_TARGET_DIR/debug/hirsel-host"
```

## Scenario

Reset:

```bash
curl -fsS -X POST "$BASE/debug/reset" -H 'authorization: Bearer dev-token'
```

Upload a text file:

```bash
TEXT_BLOB_JSON="$(curl -fsS -X POST "$BASE/debug/upload" \
  -H 'authorization: Bearer dev-token' \
  -H 'content-type: application/json' \
  -d '{"name":"../note.txt","mime":"text/plain","data_b64":"aGVsbG8gYXR0YWNobWVudAo="}')"
TEXT_BLOB_ID="$(printf '%s' "$TEXT_BLOB_JSON" | jq -r '.id')"
```

Upload a tiny PNG:

```bash
PNG_BLOB_JSON="$(curl -fsS -X POST "$BASE/debug/upload" \
  -H 'authorization: Bearer dev-token' \
  -H 'content-type: application/json' \
  -d '{"name":"tiny.png","mime":"image/png","data_b64":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/p9sAAAAASUVORK5CYII="}')"
PNG_BLOB_ID="$(printf '%s' "$PNG_BLOB_JSON" | jq -r '.id')"
```

Inject an Owner message referencing both blobs:

```bash
curl -fsS -X POST "$BASE/debug/owner-message" \
  -H 'authorization: Bearer dev-token' \
  -H 'content-type: application/json' \
  -d "{\"body\":\"Please inspect these attachments.\",\"ref\":null,\"attachments\":[\"$TEXT_BLOB_ID\",\"$PNG_BLOB_ID\"]}"
```

Request the text blob's signed URL over `/ws`, then fetch it without an authorization header:

```bash
SIGNED_JSON="$(node "$ROOT/e2e/attachments/get-blob-url.cjs" \
  "ws://127.0.0.1:$PORT/ws" dev-token "$TEXT_BLOB_ID")"
SIGNED_PATH="$(printf '%s' "$SIGNED_JSON" | jq -r '.url')"
printf '%s' "$SIGNED_JSON" | jq -e \
  '.type == "blob_url" and .blob_id == "'"$TEXT_BLOB_ID"'" and (.expires_at > now)'
curl -sS -D /tmp/hirsel-text-headers.txt -o /tmp/hirsel-text-blob \
  -w '%{http_code}' "$BASE$SIGNED_PATH" | grep -qx 200
```

Fetch the PNG blob using bearer auth:

```bash
curl -sS -D /tmp/hirsel-png-headers.txt \
  -H 'authorization: Bearer dev-token' \
  "$BASE/blob/$PNG_BLOB_ID" \
  -o /tmp/hirsel-png-blob
```

Reject altered, expired, and legacy query credentials. The expired URL uses a correctly computed
HMAC for a past `exp`, isolating expiry from signature validation; the signer key is the disposable
runbook's owner token, matching the host's signed-URL construction.

```bash
TAMPERED_PATH="$(printf '%s' "$SIGNED_PATH" | sed 's/sig=./sig=X/')"
TAMPERED_STATUS="$(curl -sS -o /dev/null -w '%{http_code}' "$BASE$TAMPERED_PATH")"
[[ "$TAMPERED_STATUS" == 401 || "$TAMPERED_STATUS" == 403 ]]

EXPIRED_EXP="$(( $(date +%s) - 1 ))"
EXPIRED_SIG="$(printf 'hirsel-blob-v1\n%s\n%s' "$TEXT_BLOB_ID" "$EXPIRED_EXP" \
  | openssl dgst -sha256 -mac HMAC -macopt key:dev-token -binary \
  | openssl base64 -A | tr '+/' '-_' | tr -d '=')"
EXPIRED_STATUS="$(curl -sS -o /dev/null -w '%{http_code}' \
  "$BASE/blob/$TEXT_BLOB_ID?exp=$EXPIRED_EXP&sig=$EXPIRED_SIG")"
[[ "$EXPIRED_STATUS" == 401 || "$EXPIRED_STATUS" == 403 ]]

LEGACY_STATUS="$(curl -sS -o /dev/null -w '%{http_code}' \
  "$BASE/blob/$TEXT_BLOB_ID?token=dev-token")"
[[ "$LEGACY_STATUS" == 401 || "$LEGACY_STATUS" == 403 ]]
```

## Gates

Poll `/debug/chat` until there is an Owner-authored message whose `attachments` array has length `2`, with one `text/plain` blob named `note.txt` and one `image/png` blob named `tiny.png`.

Check `/tmp/hirsel-text-headers.txt` contains `content-type: text/plain` and `content-disposition: attachment; filename="note.txt"`, and `/tmp/hirsel-text-blob` contains `hello attachment`.

The signed URL fetch must return 200 with no auth header. A tampered `sig`, a validly signed past
`exp`, and the removed `?token=` scheme must each return 401 or 403.

Check `/tmp/hirsel-png-headers.txt` contains `content-type: image/png` and `content-disposition: inline; filename="tiny.png"`, and `/tmp/hirsel-png-blob` is non-empty.

Poll `/debug/chat` until an Agent-authored scripted-mode message contains `Scripted turn input:` and two lines beginning `[attachment stored at `. Confirm those lines include `note.txt (text/plain, 17 bytes)` and `tiny.png (image/png,`.

Stop on any HTTP error, malformed JSON, missing blob id, wrong content header, missing attachment in Chat replay, or missing scripted turn-note exposure.
