<p align="center">
  <a href="https://hirsel.ai"><img src="docs/assets/banner.png" alt="hirsel: Good fences make fast systems." width="720"></a>
</p>

<p align="center">
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
- [video_packager](core/programs/video_packager/) packages CMAF segments and writes HLS and DASH manifests.
- [video_yuv](core/programs/video_yuv/) converts and scales video pixel formats.
- [wire](core/programs/wire/) contains runnable protocol examples for clients and places.

## Runnable examples

- [Subscription cache](sdk/client/tests/subscription-cache.ts) reads a store row from a runner subscription, observes cache changes, and applies an optimistic edit. [Run the SDK check](sdk/client/README.md#run-the-subscription-example).

## License

[MIT](LICENSE) © 2026 Sam Galanakis
