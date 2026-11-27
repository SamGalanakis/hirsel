# MCP over Streamable HTTP

The server accepts both Streamable HTTP session and per-request clients on the
same endpoint. At 2026-07-28, requests carry their own version, identity and
capabilities, and clients can use `server/discover` and `subscriptions/listen`.
Clients using a 2025 revision send `initialize`, then
`notifications/initialized`, and attach the assigned `Mcp-Session-Id` to later
requests. Sessions end at their configured lifetime or on HTTP DELETE.
The server returns HTTP 404 for requests on unknown or ended sessions.
[MCP specification](https://modelcontextprotocol.io/specification/2026-07-28)

The client speaks 2026-07-28 and the Streamable HTTP session revisions
2025-11-25, 2025-06-18 and 2025-03-26. The table in `rpc.supported` drives the
server's per-request advertisements, the client's offer and its session
validation. `client.connect` follows the 2026-07-28 backward-compatibility rules:

1. It probes with a `server/discover` request at 2026-07-28.
   - A reply means the server serves requests statelessly at that revision.
   - A -32020, -32021 or -32022 error comes from a 2026-07-28 server.
     A -32022 error's `supported` list chooses the version.
   - Any other JSON-RPC error, or a 400, 404 or 405 without one, falls back
     to `initialize` at the newest session revision in the table.
2. The server answers with its chosen session revision. The client checks it
   against the session entries, sends `notifications/initialized` and expects 202.

The `Session` keeps the version and the `Mcp-Session-Id` from the initialize
response. Every later request sends them as `MCP-Protocol-Version` and
`Mcp-Session-Id`. A 404 on a request that carried a session ID is
`SessionExpired`, and the caller calls `connect` again. Per-request `_meta`,
`Mcp-Method`, `Mcp-Name` and `Mcp-Param-*` belong to 2026-07-28 only.
Every call returns a `Reply` with the HTTP status, all response headers (`reply_header`
looks one up) and the result. The client does not open the GET stream,
resume with `Last-Event-ID` or use the HTTP+SSE transport.
[2025-11-25 transports](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports)

Start the bounded session registry with `sessions.start`, giving it Clock and
Random capabilities, a session limit and a lifetime in milliseconds. Keep its
task alive while serving. Combine the registry and the server configuration
with `server.serve`. Add the MCP routes with `server.mount` on a
`std.router.Builder[server.Endpoint[Ctx]]` and pass the built router to `serve`.
The server checks each supplied Origin against its configured allowlist.
Bind loopback for local use. A server exposed to a network needs authentication
at its deployment boundary. The client can pass a `Secret` bearer credential
over verified HTTPS. The HTTP library refuses protected headers on plain TCP.

Declare tools as `server.Tool[Ctx]` with a plain handler of type
`fn(mut server.Call, Clock, mut Ctx) -> server.Step waits`. `server.serve`
constructs a fresh `Ctx: send` through the supplied scoped factory for each
admitted tool stream. The producer passes that same mutable context to each
step until the call completes or the client closes the stream. The context can
hold explicit authority such as `Dir` or `Foreign[Sqlite]`. Use `()` and
`fn() {}` for tools that need no application state. The connection and context
factory allocations have separate `connection_bytes` and `context_bytes` bounds.

Create tool input schemas from external-API `std.json.Format` values using
`server.schema`. Handlers may yield progress steps before a final text result
or tool error. Resource handlers return text. A dynamic catalog reloads through
a caller-provided function and directory grant.
A `subscriptions/listen` client can opt into changes to that catalog.

Use `client.call` for discovery, listing and resource reads. Read tool
definitions with `metadata.tools`, then pass a selected definition to
`client.call_tool` so annotated arguments appear in `Mcp-Param-*` headers.
`metadata.tools` returns the valid definitions in `tools`. Each malformed one
goes to `rejected` as a `Malformed` naming the tool and the dotted field path,
such as `inputSchema.properties.q.x-mcp-header`. A bad definition never hides
the valid ones, and it is never dropped silently.
A notice callback can stop the current SSE stream by returning false.

The package covers discovery, tool listing and calls, resource listing
and reads, progress, cancellation and tool-list subscriptions. Prompts,
sampling, elicitation, OAuth, and resource subscriptions are out of scope.
The client does not support multi-round-trip input requests.

## TypeScript SDK oracle

Install the official packages outside the checkout.

```sh
npm install --prefix /tmp/ref-mcp-sdk @modelcontextprotocol/client@2.4.0 \
  @modelcontextprotocol/server@2.4.0 @modelcontextprotocol/node@2.1.2
```

Run the Hirsel fixture entry `mcp@1/interop/main` with mode `serve` to test
the server. Grant Clock, Random, Tasks, Stdout, a scratch directory and
`Net=Listen 127.0.0.1/32 0-65535`. It prints its loopback port. Run
`node tests/sdk-client.mjs http://127.0.0.1:PORT/mcp SCRATCH_DIRECTORY`
against it. Use an empty scratch directory for each run.

To test the client, run `node tests/sdk-server.mjs` followed by the full
Hirsel run command for the same fixture with mode `client`. Grant
`Net=Connect 127.0.0.1/32 1-65535` instead of Listen. The script appends
the loopback port and checks that disconnect aborts the SDK handler.
