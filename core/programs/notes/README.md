# notes

The reference agent for [the harness](../harness/README.md): a notes keeper
that wires every part an app composes into one agent and runs it offline.

| part | here | what it shows |
| --- | --- | --- |
| Inside tool | `note_add`, `note_delete` | runs in advance's transaction and writes the app's own `notes.notes` and `notes.stats` tables with its result |
| Outside tool | `clock` (Repeatable) | runs in a worker, off the write lock, with what the place granted |
| gate with approval | `confirm_delete` | holds `note_delete`; the program finds the open wait and approves it under the key of those exact arguments |
| subagent | `summarize` (Once) | an Outside tool that asks a second place, with its own session and agent, and returns its final answer |
| reaction | `tally` | counts tool results in the transaction that appends each one |
| section | `notes` | the current notes, rendered from rows before each request |

The app's migration runs `harness.migrate` and then creates its own two
tables. Each place reserves its store's resident cache, pack buffer and
journal group when it starts, so `options()` sizes them for one session
(4 MiB resident) instead of the 1 GiB default.

## Build and run

```
mkdir -p /tmp/notes-data
hirsel build core/programs/notes --limit 268435456 --dir /tmp/notes-data \
  --grant Stdout --grant Clock --grant Random --grant Tasks \
  --grant 'Net=Listen 127.0.0.0/8 0-65535;Connect 127.0.0.0/8 0-65535' -o notes
./notes
```

The places listen on loopback only. The program makes a place directory
and a helper directory under the granted one, and removes both at the end.

## What a run does

1. It seeds the place: a session, a stale note 0 ("Team meeting on
   Tuesday.") and the user's message.
2. The place runs until it is idle. The scripted model asks for `note_add`
   and `clock`, then `summarize` and `note_delete`. The gate holds the delete,
   and with nothing else to do the place goes idle and `operate` returns.
3. The program prints the held call and approves it as the run's caller.
4. The place runs again until it is idle. The delete runs, and the model
   answers with the helper's summary.
5. It prints the transcript, the notes, the reaction's count and the spend.

```
you: The team meeting moved to Thursday. Please note it and tidy up.
held for approval: note_delete {"id":0}
you approved it
transcript:
  user: The team meeting moved to Thursday. Please note it and tidy up.
  system: 1 section change(s), 4 tools
  assistant calls note_add {"text":"The team meeting moved to Thursday."}
  assistant calls clock {}
  result c1: added note 1
  result c2: wall clock ms 1795428015864
  system: 1 section change(s)
  assistant calls summarize {"text":"The team meeting moved to Thursday. Note 0 said Tuesday and is out of date."}
  assistant calls note_delete {"id":0}
  result c3: Summary: The team meeting moved to Thursday.
  result c4: deleted note 0
  system: 1 section change(s)
  assistant: Saved the move and removed the stale note. Summary: The team meeting moved to Thursday.
notes:
  1. The team meeting moved to Thursday.
tool results counted by the reaction: 4
spend: 45 tokens, 0 reserved
```

The spend is three model rounds of 10 input and 5 output tokens each.

## The model route

`scripted_model` answers the default run. The manual OpenRouter smoke uses the
same notes tools through the HTTP gateway, with a `Secret` grant for OpenRouter.
It creates a session over HTTP, approves one `note_add` call, checks the stored
note and `SAVED` reply, and posts another message after the place unloads. It
checks that the original SSE connection sees a partial answer before the
committed reply and prints the first-preview latency and journal commit counts.

Run `../harness/live-smoke.sh` with `hirsel` on `PATH`, or set `HIRSEL` to the
installed executable. The script sources `~/.config/hirsel/openrouter-dev.env`
when present and skips without a key.
