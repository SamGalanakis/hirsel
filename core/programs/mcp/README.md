# MCP over Streamable HTTP

This package targets [MCP revision 2026-07-28](https://modelcontextprotocol.io/specification/2026-07-28).
The TypeScript SDK v2 supports this revision. Requests carry their own
version, identity and capabilities. This revision uses `server/discover`
and `subscriptions/listen`. It has no initialization handshake, session
headers, GET stream or replay after disconnect.

Mount `server.mount` on a `std.router` and serve it with `std.http`.
The server checks each supplied Origin against its configured allowlist.
Bind loopback for local use. A server exposed to a network needs authentication
at its deployment boundary. The client can pass a `Secret` bearer credential
over verified HTTPS. The HTTP library refuses protected headers on plain TCP.

The server takes plain Hirsel tool handlers. Create tool input schemas from
external-API `std.json.Format` values using `server.schema`. Handlers may
yield progress steps before a final text result or tool error. SSE keeps a
call alive until it completes or the client closes the stream. A dynamic
catalog reloads through a caller-provided function and directory grant.
A `subscriptions/listen` client can opt into changes to that catalog.

Use `client.call` for discovery, listing and resource reads. Read tool
definitions with `metadata.tools`, then pass a selected definition to
`client.call_tool` so annotated arguments appear in `Mcp-Param-*` headers.
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
the server. Grant Clock, Tasks, Stdout, a scratch directory and
`Net=Listen 127.0.0.1/32 0-65535`. It prints its loopback port. Run
`node tests/sdk-client.mjs http://127.0.0.1:PORT/mcp SCRATCH_DIRECTORY`
against it. Use an empty scratch directory for each run.

To test the client, run `node tests/sdk-server.mjs` followed by the full
Hirsel run command for the same fixture with mode `client`. Grant
`Net=Connect 127.0.0.1/32 1-65535` instead of Listen. The script appends
the loopback port and checks that disconnect aborts the SDK handler.
