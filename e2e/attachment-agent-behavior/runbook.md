# Attachment Agent Behavior Runbook

## Purpose

Go beyond protocol plumbing and prove the real Codex Agent can use attachments as a user would expect: inspect an image attachment and read a text-file attachment from its stored path with tools.

## Execute

```bash
export CARGO_TARGET_DIR=/workspace/.cargo-target-scaffold
e2e/attachment-agent-behavior/run.sh
```

The runner generates a PNG with the word `LIME`, uploads it with a text attachment, then sends both in one Owner message to `HIRSEL_AGENT=lash HIRSEL_PROVIDER=codex HIRSEL_DRIVER=fake`.

## Gates

- Chat replay shows both attachments on the Owner message.
- The Agent reply includes `LIME`.
- The Agent reply includes the exact second line from the text attachment.
- The committed Agent message has a `shell_run` tool-call summary, proving it used tools to inspect the stored text path.

Vision failure is a prompt/provider behavior miss when the image blob is present in the Owner input. Missing stored-path notes or missing tool summaries are mechanical failures.
