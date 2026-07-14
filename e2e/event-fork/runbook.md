# Event Fork Runbook

## Purpose

Prove the event-fork loop without a live server in automated verification, or against a disposable
debug host when manually gating: open a judgment by `event_id`, verify the fork is seeded with and
returns the exact event snapshot, choose while the fork is live, and observe one quiet conclusion in
main Chat plus complete teardown.

## Setup

Follow `e2e/RULES.md` and the disposable-host setup in `e2e/side-chats/runbook.md`. Create a judgment
Event through a normal Agent/tool path and record its `EVENT_ID`, `ANCHOR_ID`, and option key/label.

## Gate 1: open by event

```bash
OPEN="$(post_json debug/open-side-chat '{"event_id":'"$EVENT_ID"'}')"
SC="$(printf '%s' "$OPEN" | jq -r '.sc')"
printf '%s' "$OPEN" | jq -e \
  '.event_id == '"$EVENT_ID"' and .ping_id == '"$EVENT_ID"' and
   .event.id == '"$EVENT_ID"' and .event.fork_sc == "'"$SC"'" and
   (.event.ui | type == "object") and .resumed == false'
```

The side Lash seed must contain one JSON `Event snapshot` with `event_id`, `kind`, `name`,
`description`, and the exact blessed `ui`. In a real-Agent gate, ask what card is being discussed and
verify the response uses those fields. The snapshot is host-built prompt context, not a copied main
transcript.

Also require an `event_upsert` for this Event with `fork_sc == $SC`.

## Gate 2: decide while open

Record the main-chat count, then send the normal card action:

```bash
BEFORE="$(get_json debug/chat | jq '.messages | length')"
post_json debug/event-action \
  '{"event_id":'"$EVENT_ID"',"action":"choose","data":{"choice":"'"$CHOICE_KEY"'"}}' \
  >/dev/null
```

Require all of the following:

- exactly one new main-chat message;
- it is Owner-authored, has `ref == $ANCHOR_ID`, and its body is
  `Discussed @<event name> → <chosen label>`;
- the Event is `done` with `fork_sc == null` and an authoritative `event_upsert` carries that state;
- one `side_chat_closed` names `$SC`, and `$SC` is absent from `/debug/side-chats`.

The same closure path is available to the side Agent as the scoped
`fork.decide({ choice: "<key>" })` tool. Non-judgment forks do not receive that tool; discarding an
info/summary fork clears `fork_sc` and adds no main-chat message.
