# MCP over Streamable HTTP

This package targets MCP revision [2026-07-28](https://modelcontextprotocol.io/specification/2026-07-28).
The official TypeScript SDK v2 supports this revision. It uses per-request
metadata in place of initialization and transport sessions.

The `rpc` module reads and writes single JSON-RPC 2.0 messages. It rejects
batches and uses string or integer request IDs. Prompts, sampling, elicitation
and OAuth are outside the package's scope.
