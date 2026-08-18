# Scope: current slice and deferred work

Living ledger for the smallest Hirsel Sam can daily-drive. Current visible behavior is governed by `PRODUCT.md`, `DESIGN.md`, `CONTEXT.md`, and `docs/product-direction.md`; ADRs record how the system arrived here.

## Current slice

- **One Host and one Agent.** A single Rust binary embeds lash and owns one long-lived RLM Agent session, SQLite durability, inline effects, transport, and process supervision. [ADR-0001, ADR-0002]
- **Effortless orchestration.** The Agent delegates durable work to Claude Code and Codex through native drivers, remains interactive, wakes on terminal events, and uses timers/monitors rather than holding a turn open. [ADR-0003, ADR-0005]
- **Task Margins client.** The SolidJS reference client rests globally and shows one flat Task inventory. Opening a Task changes the subject, renders its generated instrument and related conversation, and scopes the standing composer. Removing scope speaks globally without closing the Task.
- **Tasks, not workflow machinery.** A Task is the visible projection of a typed Event: stable identity, Anchor, generated UI, explicit open/done lifecycle. It is not a general host-side workflow/task abstraction; recovery and next steps remain Agent judgment. [ADR-0004]
- **Adaptive generated instruments.** Validated JSON UI is the Task's primary interface. Structured actions may either settle a Task or transition its instrument to another stage in place. Unknown nodes degrade safely. [ADR-0013]
- **Temporary utilities.** Processes, Settings, and Canvas dock or overlay the same Task world and restore Task, scope, draft, scroll, and focus on close.
- **Transport.** The protocol is transport-agnostic; v1 uses WSS with a bearer token, with iroh deferred. The PWA is served alongside the Host. [ADR-0006]
- **Reference protocol.** `hello`/`hello_ok`, replayed messages/Events/processes/model state, streaming turn activity, blob transfer, generated View state, and explicit Event actions. Legacy `pings`/`ping_id` spellings remain compatibility fields only.
- **Mobile direction.** Shared Rust client core plus native skins, Android first and iOS later. The native product must reproduce Task Margins rather than the retired Inbox/Chat/Tray IA. [ADR-0010]
- **Verification.** Unit/contract tests plus the deterministic 38-gate Task Margins browser runner cover global/task attribution, adaptive instruments, utilities, keyboard, phone, narrow layout, and error capture.

## Immediate build slice

1. Keep the Host/Agent/sub-agent loop dependable and interactive.
2. Deepen the constrained JSON catalog and structured transition contract only when real Tasks need new instruments.
3. Keep Task attribution, global scope, reload/reopen, and temporary-utility continuity regression-covered.
4. Bring Android onto the Rust client core with the same Task/global conversation model.

## Deferred

- **Standing decision/taste memory.** Build only after enough real decisions exist to prove the retrieval and amendment model.
- **General-purpose memory.** No notebook or observational-memory subsystem until in-context state plus `continue_as` demonstrably fails.
- **Voice/audio.** Hold-to-talk, streaming, and server-side STT are deferred whole.
- **Web Push.** Requires-response Tasks remain in-app until push earns its operational cost.
- **ACP drivers.** Add only for a valuable ACP-native agent. [ADR-0003]
- **Restate durability.** SQLite is sufficient for one Owner; the effect boundary preserves options. [ADR-0001]
- **Additional devices and iOS.** After Android proves the shared-core/native-skin shape.
- **Arbitrary generated HTML and nested mini-apps.** Explicitly rejected. New dynamic UI enters through the constrained cross-client vocabulary.
- **Side Chats or other parallel conversation destinations.** Explicitly retired from the visible product. Contextual dives happen inside a Task while the global Agent remains reachable.
- **Baked-in recurring workflows.** The Host provides timers, monitors, and schedules; the Agent/Owner creates actual workflows.
