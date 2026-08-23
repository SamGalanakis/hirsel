# Attachment Agent Behavior Runbook

## Purpose

Go beyond upload plumbing and prove the real Codex Agent receives attachment context and persists an attachment-aware reply.

## Execute

```bash
export CARGO_TARGET_DIR=/workspace/.cargo-target-scaffold
e2e/protocol-compatibility/attachment-agent-behavior/run.sh
```

The runner generates a PNG with the word `LIME`, uploads it with a text attachment, then sends both in one Owner message to `HIRSEL_AGENT=lash HIRSEL_PROVIDER=codex HIRSEL_DRIVER=fake`. The prompt asks the Agent to inspect the inline image and acknowledge the text attachment without claiming that non-image file bytes are inline.

## Gates

- Chat replay shows both attachments on the Owner message.
- The Owner message persists both attachment records with their exact names and MIME types, including the text attachment's stored metadata.
- The Agent reply includes `LIME`, proving the inline image content reached the Agent rather than merely replaying attachment metadata.

Vision failure is a prompt/provider behavior miss when the image blob is present in the Owner input. Missing Owner attachment rows or a missing persisted Agent reply are mechanical failures.
