# F13 — advertised chat views have no renderer

Recommend; high confidence, high priority. Authoritative owner C18 observed consumer contract; C24 owns host tool/ViewManager consumers, not a second finding. Worker C18-01 ../workers/C18-WEB-VIEWS.md. Independently reopened views_show schema, ViewManager show/placement validator, Rust/TS wire, reducer, canvas selector and the mounted Canvas surface. Worker exact conversion query repeated below.

The advertised tool accepts placement=chat and the host validates/publishes it successfully. Rust carries arbitrary placement String. TypeScript advertises only canvas, but runtime JSON is cast and stored, then canvasViews filters chat away. No chat-view renderer is mounted; successful agent work is invisible. Existing host schema test even asserts chat is valid. This is reachable from normal model tool arguments, not a malformed peer frame.

Target current shipped UI: make Canvas the only supported view surface end to end and reject/remove chat at the producer. Prefer deleting the obsolete placement dimension from tool/API/internal/wire shapes where all consumers are Canvas; a closed canvas-only value is a smaller coherent alternative if event routing still needs an explicit surface tag. Do not introduce a new chat surface or compatibility branch. Coordinate tool input/output schema, ViewManager validator/storage, protocol/TS/event consumers and placement docs/fixtures in one clean cutover. No database migration: active views are process-local. Validation must prove unsupported chat cannot be reported successful and a valid show reaches Canvas. Audit ran no tests.

Exact worker consumer query rerun: 48 matching lines.
