// Mock events shaped by the wire contract (scratchpad/eventq-contract.md),
// ported from the reference spikes. Host wiring lands later; until then the
// scroller seeds these in DEV so the home is real and demoable. Prod stays
// clean — nothing is seeded, so the queue shows inbox-zero until the host sends
// `event_upsert` / `hello_ok.events`.
import type { EventItem } from "../protocol";
import { dispatch } from "../store/store";

/** Judgments (needs-you) in priority order, then the awareness tail — the same
 * data the spikes render, carried as `ui` node arrays (the renderer unwraps a
 * `card` root or an array alike). */
export const MOCK_EVENTS: EventItem[] = [
  {
    id: 9001,
    kind: "judgment",
    source: { kind: "agent", ref: "hirsel-host" },
    name: "@reopen-op-shape",
    description: "How should reopening a resolved Ping be wired?",
    requires_response: true,
    quick_replies: [],
    status: "open",
    read: false,
    anchor: 0,
    ts: "2026-07-13T09:19:00Z",
    blocking: true,
    ui: [
      { type: "eyebrow", tone: "accent", boundary: true, text: "Deciding unblocks 1 agent" },
      { type: "heading", text: "How should “reopen a resolved Ping” be wired?" },
      {
        type: "text",
        tone: "muted",
        text: "`resolve_ping` is terminal on the wire, but the reopen affordance needs a real op. I stopped before writing it — it's load-bearing.",
      },
      {
        type: "optionList",
        action: "choose",
        options: [
          {
            key: "A",
            recommended: true,
            label: "New `reopen_ping` op",
            detail: "Explicit, symmetric with resolve. +1 msg type across proto·host·client.",
          },
          {
            key: "B",
            label: "Overload `resolve_ping{reopen:true}`",
            detail: "No new type. Muddies a terminal op's meaning.",
          },
          {
            key: "C",
            label: "Client-only optimistic",
            detail: "Zero wire (`resolveOverrides`). Lost on reload / 2nd device.",
          },
        ],
      },
      {
        type: "viewSlot",
        title: "the diff option A implies",
        variant: "diff",
        lines: [
          { op: "ctx", text: "enum HostOp { ResolvePing, …" },
          { op: "add", text: "  ReopenPing { ping_id: PingId }," },
          { op: "del", text: "  ResolvePing { reopen: bool }  // B: rejected" },
        ],
      },
      {
        type: "keyValue",
        items: [
          { label: "matches standing", value: "“wire ops explicit & named, never boolean-flagged”", tone: "muted" },
        ],
      },
    ],
  },
  {
    id: 9002,
    kind: "judgment",
    source: { kind: "subagent", ref: "hirsel-ui" },
    name: "@dense-done-rows",
    description: "Done section on the Pings inbox — dense rows or full cards?",
    requires_response: true,
    quick_replies: [],
    status: "open",
    read: false,
    anchor: 0,
    ts: "2026-07-13T09:35:00Z",
    ui: [
      { type: "heading", text: "Done section on the Pings inbox — dense rows or full cards?" },
      {
        type: "optionList",
        action: "choose",
        options: [
          { key: "A", recommended: true, label: "Dense one-line rows", detail: "Reads like archived mail. Density over consistency." },
          { key: "B", label: "Full cards dimmed 60%", detail: "Consistent with live cards; heavier, more scroll." },
        ],
      },
      { type: "text", tone: "muted", text: "Done is reference, not action — matches your calm + dense standing taste." },
      {
        type: "inset",
        children: [
          { type: "field", name: "note", label: "Attach a standing rule (optional)", placeholder: "e.g. “terminal/archived states are always dense rows”" },
          { type: "submit", action: "choose_with_rule", label: "Ship A + record rule", kbd: "⇧↵" },
        ],
      },
    ],
  },
  {
    id: 9003,
    kind: "judgment",
    source: { kind: "subagent", ref: "hirsel-web" },
    name: "@onboarding-copy-model",
    description: "Which model drafts the first-run onboarding copy?",
    requires_response: true,
    quick_replies: [],
    status: "open",
    read: false,
    anchor: 0,
    ts: "2026-07-13T09:38:00Z",
    ui: [
      { type: "eyebrow", tone: "accent", boundary: true, text: "Deciding unblocks 1 agent" },
      { type: "heading", text: "Which model drafts the first-run onboarding copy?" },
      {
        type: "text",
        tone: "muted",
        text: "Empty-state and first-run strings set hirsel's voice. This is user-facing — `taste` should outrank cost here.",
      },
      {
        type: "optionList",
        action: "choose",
        options: [
          { key: "A", recommended: true, label: "Fable-5 — highest taste", detail: "taste 9. Best voice for user-facing copy; slower, pricier than codex." },
          { key: "B", label: "Opus-4.8", detail: "taste 8. Strong and cheaper; a hair less distinctive." },
          { key: "C", label: "gpt-5.5 via codex", detail: "taste 5. Near-free but reads generic for voice work." },
        ],
      },
      {
        type: "keyValue",
        items: [
          { label: "matches standing", value: "“anything user-facing needs taste ≥ 7”", tone: "muted" },
        ],
      },
    ],
  },
  {
    id: 9004,
    kind: "summary",
    source: { kind: "scheduled", ref: "morning-digest" },
    name: "@overnight-digest",
    description: "Overnight fleet digest",
    requires_response: false,
    quick_replies: [],
    status: "open",
    read: false,
    anchor: 0,
    ts: "2026-07-13T06:00:00Z",
    ui: [
      { type: "text", text: "Overnight the fleet ran 6 turns across 3 repos. One PR opened, one task parked, the web UI held its bar." },
      {
        type: "viewSlot",
        title: "fleet digest",
        variant: "table",
        rows: [
          { label: "continue_as seed fix", value: "PR #212 open", state: "success" },
          { label: "qwc claim renewal", value: "parked · needs lash", state: "warning" },
          { label: "web UI · IA cleanup", value: "91 / 100", state: "running" },
        ],
      },
      {
        type: "keyValue",
        items: [
          { label: "turns", value: "6" },
          { label: "cost", value: "$0.00 (codex)", tone: "muted" },
        ],
      },
    ],
  },
  {
    id: 9005,
    kind: "summary",
    source: { kind: "scheduled", ref: "week-in-review" },
    name: "@week-in-review",
    description: "Week in review",
    requires_response: false,
    quick_replies: [],
    status: "open",
    read: false,
    anchor: 0,
    ts: "2026-07-13T06:00:30Z",
    ui: [
      { type: "text", text: "Week in review: 41 turns across 4 repos, 3 PRs merged, one release cut. The fleet held its bar with no human blocks over the weekend." },
      {
        type: "viewSlot",
        title: "by repo",
        variant: "table",
        rows: [
          { label: "hirsel", value: "23 turns · 2 PRs", state: "success" },
          { label: "lash", value: "11 turns · 1 PR", state: "success" },
          { label: "lashapp", value: "7 turns · parked", state: "warning" },
        ],
      },
      {
        type: "keyValue",
        items: [
          { label: "human blocks", value: "0" },
          { label: "spend", value: "$0.31 (mostly Android CI)", tone: "muted" },
        ],
      },
    ],
  },
  {
    id: 9006,
    kind: "info",
    source: { kind: "monitor", ref: "ci" },
    name: "@ci-green",
    description: "CI green on main",
    requires_response: false,
    quick_replies: [],
    status: "open",
    read: false,
    anchor: 0,
    ts: "2026-07-13T08:44:00Z",
    ui: [{ type: "status", state: "success", label: "monitor: CI green on `main` — all 4 jobs" }],
  },
  {
    id: 9007,
    kind: "info",
    source: { kind: "monitor", ref: "release" },
    name: "@apk-published",
    description: "Signed APK published",
    requires_response: false,
    quick_replies: [],
    status: "open",
    read: false,
    anchor: 0,
    ts: "2026-07-13T07:12:00Z",
    ui: [{ type: "status", state: "success", label: "release: signed APK `hirsel-v0.4.2.apk` published — downloadable" }],
  },
];

/** Seed the mock events into the store (idempotent per id via `event_upsert`).
 * Called by the scroller in DEV when the live event set is empty, so the home is
 * demoable before the host cutover. */
export function seedMockEvents(): void {
  for (const event of MOCK_EVENTS) {
    dispatch({ type: "event_upsert", payload: { type: "event_upsert", event } });
  }
}
