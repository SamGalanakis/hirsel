# TypeScript client for the gateway front

`client.ts` is a browser client for `harness.gateway_http` with no framework and no
build step. Its syntax is erasable: Node runs it as is, and a page loads it once its
types are stripped (`node:module`'s `stripTypeScriptTypes`, as `test.ts` does).
`example.html` is a page using it.

What the client does:

- **Caller.** Every request sends `Authorization: Bearer <base64 credential>`. Each
  POST sends `Hirsel-Position`. Positions are sequential per Caller, so the client
  sends one POST at a time. It resends a POST that got no answer, or a 503, with the
  same bytes at the same position, and advances only once answered. Keep `position`
  beside the credential to continue after a reload.
- **Image.** Rows are buffered until the `batch` event and then applied together. On
  each connection the first batch is the complete image and replaces everything held.
- **Reset.** A dropped stream, a silent one (`staleMs`) or a preview delta that does
  not meet the draft's end reconnects. The new image replaces the old one, earlier
  history pages included.
- **History.** `loadOlder()` asks for the rows before the oldest one held. Each page
  has its own pinned cutoff, and paging stops at `from` 1. A page that arrives after a
  Reset is dropped.
- **Draft.** A `preview` snapshot replaces the draft. A `preview-delta` appends only at
  the draft's byte end. `preview-clear` drops it. Once the transcript holds the row
  at `key.entry + 1`, that row supersedes the draft. Keys are compared by `run`,
  `entry`, `tool` and `attempt`, because `dispatch` holds u64s past 2^53.

While a session is busy, its stream's follow holds one wire session. The gateway's
host gives every session's place four, so a page or a POST meanwhile never waits for
the run.

The harness has no reader ACL. Every history page and stream is a fresh query under
the browser's Caller, so an application can refuse readers by registering the window
with `store.runner.query_as`.

## Test

`test.ts` runs the scripted notes gateway (`gateway/`) behind a same-origin server
that also serves `example.html`, and drives the client through:

- a send and a decision whose answers are cut off;
- an unload under the open stream;
- a drafted answer;
- a stream cut mid-answer;
- a Reset after paging;
- paging back to the first row.

The oracle is a fresh reader: a raw stream's first image plus raw history pages,
parsed without `client.ts`.

```sh
out=$(mktemp -d)
mkdir "$out/data"
hirsel build gateway --limit 536870912 --dir "$out/data" --grant Stdout --grant Clock \
    --grant Random --grant Tasks --grant 'Net=Listen 127.0.0.0/8 0-65535;Connect 127.0.0.0/8 0-65535' \
    -o "$out/gateway"
node test.ts "$out/gateway"          # the checks
node test.ts "$out/gateway" serve    # the example page, until interrupted
```
