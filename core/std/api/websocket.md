# WebSocket

[API overview](../API.md). [Operations and contracts](../websocket.hirsel).

The module is pure RFC 6455 protocol logic over bytes. It performs no I/O,
chooses no deadline, and generates no entropy. The socket owner supplies
random client nonces and a fresh four-byte mask for each encoded client frame.

`server` parses a bounded upgrade and returns its response and consumed byte
offset. `client` checks a 101 against the actual challenge and the client's
protocol offer. An offered hirsel.1 must be returned; a caller making no
offer rejects any selected protocol. Unoffered protocols and every extension are refused. Preserve
`input[end..]` as the initial frame input. `request` constructs the fixed
`/hirsel` request. Refused 426 requires a `Sec-WebSocket-Version: 13` response
header. All paths expose Capacity separately from protocol refusal.
