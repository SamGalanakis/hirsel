# hirsel-client-core

Pure-Rust client foundation for the Hirsel mobile and desktop UI skins. It owns
the WebSocket connection, reconnect/resume behavior, ordered offline sends,
optimistic chat reconciliation, the local protocol-facing store, and observer
notifications.

This slice intentionally defers running-turn `turn_event` timelines, side
chats, attachments/blob transfer, alternate send modes, and turn/queued-send
cancellation. The public records use owned values, the `Client` handle is
cheaply cloneable through `Arc`, and `ClientObserver` is object-safe so step 2
can add a thin UniFFI wrapper without moving transport or reducer logic.
