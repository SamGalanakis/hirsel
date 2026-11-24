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

## License

[MIT](LICENSE) © 2026 Sam Galanakis
