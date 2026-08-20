# Just-in-time scrollback runbook

## Purpose

Prove that the newest-200 `hello_ok.messages` replay continues into full stored
history through correlated `fetch_messages` pages while the real browser keeps
the row being read visually stationary. Also prove the 600-row client cap
evicts only from the newest committed edge and that “Jump to latest” reloads
that edge before pinning.

## Automated run

From `app/`:

```bash
npm run e2e:scrollback
```

The runner starts the in-repo WebSocket mock with 760 deterministic stored rows
and a delayed history response, then starts Vite on a separate controlled
port. It uses a clean headless Chromium context at 1280×800, light scheme, and
reduced motion. The generated report is
[`../../reports/scrollback-latest.md`](../../reports/scrollback-latest.md).

## Gates

1. Initial rendering remains a bounded small window (30–90 rows, allowing the
   1.5-viewport prefetch to warm it) and has no manual earlier-message control.
2. Scrolling to the prefetch margin expands the client render window.
3. Messages present before both a client-window reveal and a Host-page prepend
   keep the same viewport `top` within 1.5px.
4. The browser emits one correlated `fetch_messages` request with limit 100.
5. A quiet loading row exists only while the delayed page is outrun at the top.
6. Repeated top-edge scrolling reaches stored row 1; the settled beginning has
   no button, spinner, or terminal marker.
7. After the bounded range evicts the newest edge, the jump affordance is
   visible and reloads the true latest Host page before pinning.
8. There are no console, page, response, or request failures.

Abort on any failed gate. The runner owns and terminates its mock, Vite, and
browser process groups and records exact geometry/frame evidence in the report.
