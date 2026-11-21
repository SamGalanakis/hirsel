# Million checkboxes

From the repository root, with a current Hirsel toolchain and Node.js 22 or later on
`PATH`, start the server and open the board with:

```sh
python3 core/programs/checkbox/run.py --hirsel "$(command -v hirsel)" --work "$HOME/.local/share/hirsel-checkbox" --open
```

The page opens at `http://127.0.0.1:7700/`. Click any pixel to toggle it. Open
another tab to see the shared board. The store persists under `--work/data`.
Use a new work directory for an empty board. The launcher builds the server,
generates the bindings with `hirsel sdk ts`, and bundles the shared browser
client under `--work/web`. The browser loads the HTML and ES modules directly.
Pass `--web DIR` to serve an existing copy of those files.

The runner limits are on by default. Application flags follow `--`, for
example `-- --ingress 127.0.0.1/32` for a local proxy that supplies the source IP
in its sanitized `X-Forwarded-For` header. Use the launcher's `--listen` to
change the IPv4 address or port and its matching Net grant.

## Acceptance

With Chromium installed for `playwright-cli`, run one law against a fresh place:

```sh
python3 -B core/programs/checkbox/qualify.py --hirsel "$(command -v hirsel)" converge
```

`qualify.py --help` lists the selectors and the convergence parameters. The
default `converge` uses 64 Node SDK clients, 1,000 toggles each at a target pace
of 10/s, and one browser. It gives each client one disconnect and checks all
images and a fresh subscription against the returned-call XOR fold.

For an already-running endpoint, invoke `converge.mjs ORIGIN GENERATED
[CLIENTS] [TOGGLES] [PACE] [SEED]` with Node, where `GENERATED` contains the
SDK output for that endpoint. `browser-laws.mjs browser_ticks ORIGIN` checks
two browser pages, and `wire-laws.mjs SELECTOR ORIGIN GENERATED` runs the
socket laws against a fresh place. The query-count selector needs the scratch
server inserted by `qualify.py`.
