# hirsel-client-core

Pure-Rust client foundation for Hirsel's mobile and desktop skins. It owns the
WebSocket connection, reconnect/resume behavior, ordered offline sends,
conversation reconciliation, the protocol-facing Task/message/process store,
and observer notifications.

The product contract above this core is Task Margins: one global conversation,
one flat Task inventory, Task-scoped messages through Anchor + mention, and
temporary utilities. Protocol records still contain legacy `chat`/`ping`
spellings; those are compatibility names, not native navigation concepts.

This slice intentionally defers running-turn `turn_event` timelines,
attachments/blob transfer, alternate send modes, and turn/queued-send
cancellation. Legacy side-session decoding, where retained, is compatibility
only and must not become a native Side Chat surface. The public records use
owned values, the `Client` handle is cheaply cloneable through `Arc`, and
`ClientObserver` is object-safe so UniFFI can wrap it without moving transport
or reducer logic.
