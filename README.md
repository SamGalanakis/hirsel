<p align="center">
  <a href="https://hirsel.ai"><img src="docs/assets/banner.png" alt="hirsel: Good fences make fast systems." width="720"></a>
</p>

<p align="center">
  A language for agents 🐑<br>
  A native systems language with explicit authority, logical byte budgets, and recorded replay.<br>
  Unbounded generation, bounded execution.
</p>

<p align="center">
  <a href="https://hirsel.ai"><b>hirsel.ai</b></a>
  &nbsp;·&nbsp;
  <a href="docs/PRIMER.md">Primer</a>
  &nbsp;·&nbsp;
  <a href="docs/spec/index.md">Spec</a>
  &nbsp;·&nbsp;
  <a href="core/std/API-INDEX.md">Std API</a>
  &nbsp;·&nbsp;
  <a href="docs/building.md">Building</a>
</p>

## Install

```sh
curl -fsSL https://hirsel.ai/install.sh | sh
```

See [docs/building.md](docs/building.md) for CLI commands, capabilities, testing, recording/replay, and building from source.

## Subscriptions

The TypeScript SDK exposes a frozen subscription image through `rows(slot)`, `map(slot)`, and `get(slot, key)`. Reads share row references. Each `onChange(sub, change)` callback sees the applied Batch and receives typed insertions, updates, and deletions. Reset and connection loss clear the image and send a data-free invalidation. [SDK subscription contract](docs/spec/sdk.md#subscriptions)

## Packages

- [av1](core/programs/av1/) encodes and decodes AV1 video.
- [checkbox](core/programs/checkbox/) runs a shared board of one million checkboxes.
- [ffi_laws](core/programs/ffi_laws/) checks native FFI calls, resource lifetimes, and authority.
- [h264](core/programs/h264/) encodes and decodes H.264 video.
- [harness](core/programs/harness/) runs durable agent sessions, tools, approvals, and model calls.
- [harness_dev_serve](core/programs/harness/serve/) serves the notes agent through the development gateway.
- [harness_ts_gateway](core/programs/harness/ts-client/gateway/) serves the notes agent for the TypeScript gateway client checks.
- [image_core](core/programs/image_core/) probes and transforms images and handles PNG, QOI, GIF, and AVIF data.
- [image_jpeg](core/programs/image_jpeg/) encodes and decodes JPEG images.
- [image_webp](core/programs/image_webp/) encodes and decodes WebP images and animations.
- [m1](core/programs/m1/) contains runnable programs used to check the native backends.
- [m2b](core/programs/m2b/) contains runnable programs for dispatch, transactions, and replay.
- [mcp](core/programs/mcp/) implements the Model Context Protocol client and server over Streamable HTTP.
- [media_bits](core/programs/media_bits/) parses codec headers and packet metadata.
- [media_container](core/programs/media_container/) reads and writes MP4, WebM, and Ogg containers.
- [notes](core/programs/notes/) runs an offline notes agent with tools and an approval step.
- [observe](core/programs/observe/) exercises observation, session positions, and version tracking.
- [omap](core/programs/omap/) runs the persistent ordered map benchmark.
- [opus](core/programs/opus/) encodes and decodes Opus audio.
- [platform](core/programs/platform/) starts and manages deployed places, including cutovers and recovery.
- [postgres](core/programs/postgres/) connects to PostgreSQL with TLS and streams query results.
- [runner](core/programs/runner/) registers calls and subscriptions and serves them from a place.
- [search](core/programs/search/) indexes and ranks text with BM25 over store snapshots.
- [sha256](core/programs/sha256/) runs the SHA-256 benchmark.
- [sqlite](core/programs/sqlite/) opens SQLite databases and runs SQL with explicit capabilities.
- [store](core/programs/store/) provides typed tables, durable transactions, snapshots, and replay.
- [store-crash](core/programs/store-crash/) checks store, runner, and platform recovery after crashes.
- [stun_ice](core/programs/stun_ice/) encodes STUN messages and checks ICE connectivity.
- [tz](core/programs/tz/) resolves IANA time zones and converts between instants and local time.
- [video_packager](core/programs/video_packager/) packages CMAF segments and writes HLS and DASH manifests.
- [video_yuv](core/programs/video_yuv/) converts and scales video pixel formats.
- [wire](core/programs/wire/) contains runnable protocol examples for clients and places.
- [xml](core/programs/xml/) parses XML as events or a document tree and writes it back as UTF-8.

## Runnable examples

- [Subscription cache](sdk/client/tests/subscription-cache.ts) reads a store row from a runner subscription, observes cache changes, and applies an optimistic edit. [Run the SDK check](sdk/client/README.md#run-the-subscription-example).
- [PostgreSQL](core/programs/postgres/example.hirsel) reads a typed column and drains the cursor before reusing the connection.
- [SQLite](core/programs/sqlite/example.hirsel) binds a customer ID and reads the total from an in-memory database.
- [MCP](core/programs/mcp/example.hirsel) decodes a tool-list reply and reads the tool name.
- [Search](core/programs/search/example.hirsel) indexes two recipes in store and queries a snapshot for the published match.
- [JPEG](core/programs/image_jpeg/vectors/example.hirsel) probes an image with `image_core` and reads packed RGB strips.
- [WebP](core/programs/image_webp/vectors/example.hirsel) sizes an RGBA buffer from `image_core` metadata and decodes a lossless still.
- [HLS](core/programs/video_packager/example.hirsel) packages MP4 track samples and writes their VOD playlist.
- [Opus](core/programs/opus/vectors/example.hirsel) decodes a 20 ms stereo packet to PCM.

Run the package examples from the repository root with the Hirsel toolchain.

```sh
hirsel test core/programs/postgres --exact postgres@1/sim_tests/select_answer
hirsel test core/programs/sqlite --exact sqlite@1/example/sum_orders --grant Foreign=sqlite@1/sqlite/Sqlite
hirsel test core/programs/mcp --exact mcp@1/example/read_tool_catalog
hirsel test core/programs/search --exact search@1/example/find_document
hirsel test core/programs/video_packager --exact video_packager@1/tests/hls_cmaf
```

The codec examples run with the existing [JPEG](core/programs/image_jpeg/test-vectors.sh),
[WebP](core/programs/image_webp/test-vectors.sh), and [Opus](core/programs/opus/test-vectors.sh)
vector scripts. Each script lists the reference inputs it needs.

## License

[MIT](LICENSE) © 2026 Sam Galanakis
