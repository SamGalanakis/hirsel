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
  <a href="docs/COOKBOOK.md">Cookbook</a>
  &nbsp;·&nbsp;
  <a href="docs/spec/index.md">Spec</a>
  &nbsp;·&nbsp;
  <a href="core/std/API-INDEX.md">Std API</a>
  &nbsp;·&nbsp;
  <a href="docs/building.md">Building</a>
  &nbsp;·&nbsp;
  <a href="https://hirsel.ai/llms.txt">llms.txt</a>
</p>

<p align="center">
  <sub>Status: <b>v0.1.0-pre</b> · Linux x86_64 (glibc 2.28+) · Apple Silicon macOS in progress</sub>
</p>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/readme-hero-dark.svg">
  <source media="(prefers-color-scheme: light)" srcset="docs/assets/readme-hero-light.svg">
  <img alt="Hirsel hello world example showing explicit Stdout capability, mutable List and String, and waits effect" src="docs/assets/readme-hero-dark.svg" width="100%">
</picture>

<details>
<summary>View / copy source (<code>main.hirsel</code>)</summary>

```hirsel
pub fn main(out: Stdout) -> Result[(), WriteError] waits {
  let mut names: List[String] = list.new()
  list.push(mut names, "ada")
  list.push(mut names, "grace")
  let mut line = string.new()
  for n in names {
    string.push(mut line, n)
    string.push(mut line, " ")
  }
  string.push_u64(mut line, list.len(names))
  string.push(mut line, "\n")
  stdout.write(out, line)
}
```

</details>

Every outside effect enters as a parameter (`Stdout`, `Dir`, `Clock`, `Net`, `Tasks`),
every allocation is charged against an explicit byte limit, and every function checks
locally against the signatures it names. No imports, no methods, no garbage collector, no
lifetime annotations.

## Install

```sh
curl -fsSL https://hirsel.ai/install.sh | sh
```

Requires **Linux x86_64** with **glibc 2.28+** (Apple Silicon macOS support is in
progress). Re-run the installer to upgrade, or set `HIRSEL_VERSION` to pin a release.

Start a project with the installed binary:

```sh
hirsel new app && cd app
hirsel check
hirsel test
hirsel run
```

See [docs/PRIMER.md](docs/PRIMER.md) for the language in one page,
[docs/COOKBOOK.md](docs/COOKBOOK.md) for tested recipes, and
[docs/building.md](docs/building.md) for CLI options, capabilities, recording/replay, and
building from source.

## For Coding Agents

> **Authority rule:** Always run `hirsel primer` from your installed binary as the exact
> authority for your installed version.

Hirsel's CLI is designed as a self-contained feedback loop for agents:

- `hirsel primer` — prints the exact language primer matching your installed compiler.
- `hirsel explain RULE` (`--list`, `--json`) — prints the normative rule and refused example for any diagnostic ID (e.g., `OWN-7`, `CAP-1`).
- `hirsel inspect --core --interface` / `hirsel inspect --std --interface` — lists every public core and `std` declaration with exact parameter modes (`mut`, `take`).
- `hirsel check --json` — machine-readable diagnostics with rule IDs, spans, and causal sites.
- [hirsel.ai/llms.txt](https://hirsel.ai/llms.txt) — concise agent index.
- [hirsel.ai/llms-full.txt](https://hirsel.ai/llms-full.txt) — single-file agent bundle (intro, primer, cookbook, std API index, and skill).
- [skills/hirsel/SKILL.md](skills/hirsel/SKILL.md) — ready-to-use agent skill for editing, checking, and fixing Hirsel packages.

## Toolchain packages

Seventeen packages ship with the Hirsel toolchain (`std` plus the sixteen packages in
`core-packages/`) and bind by major version in `hirsel.toml`:

```text
[dependencies]
std = { major = 1 }
store = { major = 1 }
runner = { major = 1 }
```

- [std](core/std/API-INDEX.md) — standard library: files, directories, HTTP server/client, router, JSON, codecs, simulation (`std.sim`), property testing (`std.prop`), and crypto.
- [store](core/programs/store/) — copy-on-write B+tree durable store: typed tables, secondary/unique/aggregate indexes, TTL expiry, projections, transactions, snapshots, and replay.
- [runner](core/programs/runner/) — durable calls, live diff subscriptions, Rooms, TypeScript SDK generation ([sdk/client/README.md](sdk/client/README.md)), and place hosting.
- [platform](core/programs/platform/) — deployed place management, cutovers, and recovery.
- [search](core/programs/search/) — BM25 full-text search over a `store` table, callable from a transaction.
- [mcp](core/programs/mcp/) — Model Context Protocol client and server over Streamable HTTP.
- Real-time media and networking (**pure Hirsel, no `Foreign` code**): [webrtc_sfu](core/programs/webrtc_sfu/) (WebRTC SFU with simulcast, VP9/AV1 SVC, dynacast, Opus RED, and E2EE routing), [dtls](core/programs/dtls/) (sans-IO DTLS 1.2), [srtp](core/programs/srtp/) (SRTP/SRTCP), [rtp](core/programs/rtp/) (RTP/RTCP, RTX, BWE, pacing, RED), [rtp_jitter](core/programs/rtp_jitter/) (jitter buffer), [turn](core/programs/turn/) (TURN client/relay), [stun_ice](core/programs/stun_ice/) (STUN and ICE-lite), [sframe](core/programs/sframe/) (RFC 9605 SFrame E2EE), [media_bits](core/programs/media_bits/) (codec bitstream parsers), [media_container](core/programs/media_container/) (Ogg/MP4/WebM containers), and [media_record](core/programs/media_record/) (room publication recorder).

## Built on top, in Hirsel

Additional first-party packages in this repository (bind locally with `path = "..."` in
`[dependencies]`):

**Pure Hirsel libraries (no `Foreign` code):**

- [video_packager](core/programs/video_packager/) — CMAF, HLS, and DASH media packaging.
- [image_core](core/programs/image_core/) — image probing, transforms, and PNG, QOI, GIF, and AVIF codecs.
- [vector](core/programs/vector/) — exact top-k nearest neighbours over a `store` table, read from a transaction.
- [spatial](core/programs/spatial/) — Hilbert-curve spatial indexing over a `store` table.
- [postgres](core/programs/postgres/) — PostgreSQL client with TLS and streaming query results.
- [jwtbind](core/programs/jwtbind/) — JWT verification and runner caller binding.
- [toml](core/programs/toml/) — TOML parser and serializer.
- [yaml](core/programs/yaml/) — YAML parser and serializer.
- [archive](core/programs/archive/) — tar (ustar/pax) and zip archive reader and writer with explicit safety limits.
- [tz](core/programs/tz/) — IANA time zones and instant/local-time conversion.
- [xml](core/programs/xml/) — streaming and tree XML parser and UTF-8 serializer.

**Thin C FFI wrappers (`Foreign`):**

- [opus](core/programs/opus/), [av1](core/programs/av1/), [h264](core/programs/h264/) — thin C FFI wrappers for Opus, AV1, and H.264 codecs (used with [video_yuv](core/programs/video_yuv/), [video_thumb](core/programs/video_thumb/), and [video_transcode](core/programs/video_transcode/)).
- [image_jpeg](core/programs/image_jpeg/), [image_webp](core/programs/image_webp/) — thin C FFI wrappers for JPEG and WebP codecs (used with [image_pipeline](core/programs/image_pipeline/)).
- [sqlite](core/programs/sqlite/) — SQLite bindings with explicit `Foreign` capabilities.

**Applications:**

- [checkbox](core/programs/checkbox/) — shared board of one million checkboxes.

The durable LLM agent harness and its notes example app live in their own repository,
[lash-hirsel](https://github.com/SamGalanakis/lash-hirsel), and build against an installed toolchain.

Internal verification harnesses and benchmarks (`m1`, `m2b`, `ffi_laws`, `store-crash`,
`observe`, `wire`, `omap`, `sha256`) also live under [core/programs/](core/programs/) for
compiler and runtime testing.

## Runnable examples

Standalone recipes that work in any fresh project (`hirsel new app`) are in
[docs/COOKBOOK.md](docs/COOKBOOK.md).

Repository examples (run from a checkout of this repo):

- [Subscription cache](sdk/client/tests/subscription-cache.ts) reads a `store` row from a `runner` subscription, observes cache changes, and applies an optimistic edit ([run the SDK check](sdk/client/README.md#run-the-subscription-example)).
- [PostgreSQL](core/programs/postgres/example.hirsel) reads a typed column and drains the cursor before reusing the connection.
- [SQLite](core/programs/sqlite/example.hirsel) binds a customer ID and reads the total from an in-memory database.
- [MCP](core/programs/mcp/example.hirsel) decodes a tool-list reply and reads the tool name.
- [Search](core/programs/search/example.hirsel) declares search on a `store` table of recipes and queries it in the writing transaction for the published match.
- [JPEG](core/programs/image_jpeg/vectors/example.hirsel), [WebP](core/programs/image_webp/vectors/example.hirsel), [HLS](core/programs/video_packager/example.hirsel), and [Opus](core/programs/opus/vectors/example.hirsel) media codec and packaging examples.

Run the package examples from the repository root with the `hirsel` binary:

```sh
hirsel test core/programs/postgres --filter select_answer
hirsel test core/programs/sqlite --filter sum_orders --grant Foreign=sqlite@1/sqlite/Sqlite
hirsel test core/programs/mcp --filter read_tool_catalog
hirsel test core/programs/search --filter find_document
hirsel test core/programs/video_packager --filter hls_cmaf
```

The codec examples run with the [JPEG](core/programs/image_jpeg/test-vectors.sh),
[WebP](core/programs/image_webp/test-vectors.sh), and
[Opus](core/programs/opus/test-vectors.sh) vector scripts. Each script lists the
reference inputs it needs.

## License

[MIT](LICENSE) © 2026 Sam Galanakis
