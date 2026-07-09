# Attachments Runbook

## Purpose

Prove protocol v1.1 attachments through the debug surface: upload a text blob and a tiny PNG, send an Owner message referencing both, verify Chat replay includes both attachments, fetch both blob assets with auth, and confirm the scripted Agent saw the stored-path attachment notes.

## Start Host

```bash
export CARGO_TARGET_DIR=/workspace/.cargo-target-chat-native
export HIRSEL_AGENT=scripted
export HIRSEL_TOKEN=dev-token
export HIRSEL_DEBUG=1
export HIRSEL_DRIVER=fake
export HIRSEL_DATA_DIR=/tmp/hirsel-e2e-attachments
export HIRSEL_LISTEN=127.0.0.1:3089
cargo run -p hirsel-host
```

## Scenario

Reset:

```bash
curl -sS -X POST http://127.0.0.1:3089/debug/reset
```

Upload a text file:

```bash
TEXT_BLOB_JSON="$(curl -sS -X POST http://127.0.0.1:3089/debug/upload \
  -H 'content-type: application/json' \
  -d '{"name":"../note.txt","mime":"text/plain","data_b64":"aGVsbG8gYXR0YWNobWVudAo="}')"
TEXT_BLOB_ID="$(printf '%s' "$TEXT_BLOB_JSON" | jq -r '.id')"
```

Upload a tiny PNG:

```bash
PNG_BLOB_JSON="$(curl -sS -X POST http://127.0.0.1:3089/debug/upload \
  -H 'content-type: application/json' \
  -d '{"name":"tiny.png","mime":"image/png","data_b64":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/p9sAAAAASUVORK5CYII="}')"
PNG_BLOB_ID="$(printf '%s' "$PNG_BLOB_JSON" | jq -r '.id')"
```

Inject an Owner message referencing both blobs:

```bash
curl -sS -X POST http://127.0.0.1:3089/debug/owner-message \
  -H 'content-type: application/json' \
  -d "{\"body\":\"Please inspect these attachments.\",\"ref\":null,\"attachments\":[\"$TEXT_BLOB_ID\",\"$PNG_BLOB_ID\"]}"
```

Fetch the text blob as an authenticated asset:

```bash
curl -sS -D /tmp/hirsel-text-headers.txt \
  "http://127.0.0.1:3089/blob/$TEXT_BLOB_ID?token=dev-token" \
  -o /tmp/hirsel-text-blob
```

Fetch the PNG blob using bearer auth:

```bash
curl -sS -D /tmp/hirsel-png-headers.txt \
  -H 'authorization: Bearer dev-token' \
  "http://127.0.0.1:3089/blob/$PNG_BLOB_ID" \
  -o /tmp/hirsel-png-blob
```

## Gates

Poll `/debug/chat` until there is an Owner-authored message whose `attachments` array has length `2`, with one `text/plain` blob named `note.txt` and one `image/png` blob named `tiny.png`.

Check `/tmp/hirsel-text-headers.txt` contains `content-type: text/plain` and `content-disposition: attachment; filename="note.txt"`, and `/tmp/hirsel-text-blob` contains `hello attachment`.

Check `/tmp/hirsel-png-headers.txt` contains `content-type: image/png` and `content-disposition: inline; filename="tiny.png"`, and `/tmp/hirsel-png-blob` is non-empty.

Poll `/debug/chat` until an Agent-authored scripted-mode message contains `Scripted turn input:` and two lines beginning `[attachment stored at `. Confirm those lines include `note.txt (text/plain, 17 bytes)` and `tiny.png (image/png,`.

Stop on any HTTP error, malformed JSON, missing blob id, wrong content header, missing attachment in Chat replay, or missing scripted turn-note exposure.
