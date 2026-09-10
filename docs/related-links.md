# Related resources

A Thread's Related list combines its existing artifact references with URLs and
Threads a person or agent explicitly saves. Saving a reference does not create
an artifact, fetch a page, send a message, start work or change the conversation
recipient. Artifact identity and references remain independent; global Artifacts
still lists results across Threads.

`threads.add_related` saves a typed target in self (`.`) or an authorized
descendant Thread:

```typescript
await threads.add_related({
  target: {kind: "url", url: "https://example.com/architecture"},
  title: "Architecture notes"
});
await threads.add_related({target: {kind: "thread", thread: "./12"}});
```

`threads.remove_related` removes a saved reference by `item_id`. Read current
`related_items` through `threads.context` (self) or `threads.read` (self or
descendants), independently of message pagination. Agent-created Thread targets
must also be within self/subtree scope. Human-created references to inaccessible
Threads are omitted from agent output; no title or conversation access leaks.
Ancestors still expose identity only. Thread references do not alter hierarchy.

Thread links use ordinary Markdown and the existing web route:
`[Architecture](/t/12?history=00000000-0000-0000-0000-000000000000)`.
Use the real authoritative `reference_url` supplied by Thread context/read or
create/update results. Listing Threads also supplies the current `history_id`.
An absolute URL may include the current app origin. The browser validates history
before selecting an ID, so an old link cannot open an unrelated reused Thread.

The host deduplicates targets within each Thread. URL scheme, host and default
port normalize through the URL parser; query strings and fragments remain
significant. Repeated adds retain the original URL title. Thread titles come
from the current Thread inventory rather than copied reference metadata.
Credentials, non-HTTP(S) schemes, malformed URLs, backslashes and control
characters are rejected. Limits: 100 Related items per Thread, 4096 URL bytes
and 200 title characters. Empty URL titles use a URL-derived label in the client.
No metadata or favicon request is made by the backend.

Human mutations include originating history, Thread and a durable client ID.
Replays perform no new mutation; acknowledgments contain the current complete
list. Replaying an old add after a remove cannot resurrect the reference. Reusing
a client ID with different input fails. Agent mutations use existing execution
bindings and operation receipts. Reset invalidates both kinds of stale requests.

Realtime snapshots include history, Thread and revision. Clients compare against
the last applied Related snapshot revision, independently of newer general
Thread metadata. Related edits do not change factual activity time, attention,
read state or execution state. Human broadcasts contain the complete current
list even when an agent's tool result hides inaccessible Thread references.

Current store schema 5 includes Related receipts, Thread icons and the optional
showcased artifact in its canonical layout. Startup accepts that exact layout or
an empty store. Older and altered layouts require an offline replacement with a
fresh current store after an idle stop and complete backup; the runtime does not
upgrade or import them.
