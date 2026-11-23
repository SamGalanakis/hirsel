// A browser client for the harness gateway's HTTP front (gateway_http.hirsel). It has
// no framework and no build step: the syntax is erasable, so `node client.ts` runs it
// and a page can load it once its types are stripped.
//
// The browser is the session's Caller. It sends `Authorization: Bearer <base64 credential>`
// on every request and `Hirsel-Position` on each POST. Positions are sequential per Caller
// across connections, so the client sends one POST at a time, resends an unanswered one
// with the same bytes at the same position, and moves on only after an answer. Keep
// `position` with the credential (for example in localStorage) to continue after a reload.

export type View = { Status: string } | { Line: [string, string] };
export type Shown = { at: number; line: View };
// Compare keys by run, entry, tool and attempt: `dispatch` holds u64s past 2^53.
export type DraftKey = { run: number; entry: number; tool?: number; attempt: number };
export type Draft = { key: DraftKey; offset: number; text: string };
export type Decision = { run: number; round: number; ordinal: number; key: string; allow: boolean };
export type State = {
  status: string; rows: Shown[]; draft: Draft | null;
  live: boolean; older: boolean; // whether the first image has arrived; whether earlier rows remain
};
export type Options = {
  base: string; session: number; credential: string; position?: number;
  tail?: number; page?: number; staleMs?: number; onChange: (s: State) => void;
};

const bytes = (s: string) => new TextEncoder().encode(s).length;

export class Client {
  position: number;
  private o: Options;
  private rows = new Map<number, View>();
  private draft: Draft | null = null;
  private live = false;
  private older = true;
  private generation = 0; // bumped on every Reset; stale history pages are dropped
  private abort: AbortController | null = null;
  private queue: Promise<unknown> = Promise.resolve();
  private stopped = false;

  constructor(o: Options) { this.o = o; this.position = o.position ?? 0; }

  private headers(): Record<string, string> { return { Authorization: `Bearer ${this.o.credential}` }; }
  private url(path: string) { return `${this.o.base}/sessions/${this.o.session}${path}`; }

  state(): State {
    const rows = [...this.rows].filter(([at]) => at > 0).sort((a, b) => a[0] - b[0])
      .map(([at, line]) => ({ at, line }));
    const s = this.rows.get(0);
    return { status: s && "Status" in s ? s.Status : "", rows, draft: this.draft, live: this.live, older: this.older };
  }
  private changed() { this.o.onChange(this.state()); }

  // ---- POSTs: one at a time, each retried at its own position until it is answered ----
  send(text: string, clientKey: string) {
    return this.post("/messages", JSON.stringify({ client_key: clientKey, text }));
  }
  decide(d: Decision) { return this.post("/decisions", JSON.stringify(d)); }

  private post(path: string, body: string): Promise<{ status: number; body: unknown }> {
    const run = async () => {
      for (let wait = 50; ; wait = Math.min(wait * 2, 2000)) {
        let r: Response | null = null;
        try {
          r = await fetch(this.url(path), {
            method: "POST", body,
            headers: { ...this.headers(), "Hirsel-Position": String(this.position), "Content-Type": "application/json" },
          });
        } catch { /* no answer: resend the same bytes at the same position */ }
        if (r && r.status !== 503) {
          this.position += 1;
          const text = await r.text();
          return { status: r.status, body: text ? JSON.parse(text) : null };
        }
        if (this.stopped) throw new Error("client stopped");
        await new Promise((ok) => setTimeout(ok, wait));
      }
    };
    const next = this.queue.then(run, run);
    this.queue = next.catch(() => undefined);
    return next;
  }

  // ---- the event stream ----
  start() { this.stopped = false; void this.follow(); }
  stop() { this.stopped = true; this.abort?.abort(); }

  // Each connection is a Reset: the rows it sends before its first batch are the
  // complete image, which replaces everything held, earlier pages included.
  private async follow() {
    for (let wait = 100; !this.stopped; wait = Math.min(wait * 2, 5000)) {
      const generation = ++this.generation;
      const abort = (this.abort = new AbortController());
      let heard = Date.now();
      const stale = setInterval(() => { if (Date.now() - heard > (this.o.staleMs ?? 15000)) abort.abort(); }, 1000);
      try {
        const tail = this.o.tail ? `?tail=${this.o.tail}` : "";
        const r = await fetch(this.url(`/events${tail}`), { headers: this.headers(), signal: abort.signal });
        if (!r.ok || !r.body) throw new Error(`events: HTTP ${r.status}`);
        this.draft = null;
        let pending: Shown[] = [];
        let first = true;
        let buffer = "";
        const reader = r.body.pipeThrough(new TextDecoderStream()).getReader();
        for (;;) {
          const { value, done } = await reader.read();
          if (done) break;
          heard = Date.now();
          buffer += value;
          let cut: number;
          while ((cut = buffer.indexOf("\n\n")) >= 0) {
            const block = buffer.slice(0, cut);
            buffer = buffer.slice(cut + 2);
            let event = "message";
            const data: string[] = [];
            for (const line of block.split("\n")) {
              if (line.startsWith("event:")) event = line.slice(6).trim();
              else if (line.startsWith("data:")) data.push(line.slice(5).replace(/^ /, ""));
            }
            if (data.length === 0) continue; // a heartbeat comment
            const payload = data.join("\n");
            if (event === "message") pending.push(JSON.parse(payload));
            else if (event === "batch") {
              if (first) { this.rows.clear(); this.older = true; first = false; this.live = true; wait = 100; }
              for (const row of pending) this.rows.set(row.at, row.line);
              pending = [];
              this.supersede();
              this.changed();
            } else if (!this.preview(event, payload)) { abort.abort(); break; }
          }
        }
      } catch { /* the connection ended; reconnect */ }
      clearInterval(stale);
      if (generation === this.generation) { this.live = false; this.changed(); }
      if (!this.stopped) await new Promise((ok) => setTimeout(ok, wait));
    }
  }

  // A snapshot replaces the draft, a delta must append at its end, a clear drops it.
  // A delta that does not meet the end means text was lost: the stream starts again.
  private preview(event: string, payload: string): boolean {
    if (event === "preview") this.draft = JSON.parse(payload);
    else if (event === "preview-clear") this.draft = null;
    else if (event === "preview-delta") {
      const d: { at: number; text: string } = JSON.parse(payload);
      if (!this.draft || d.at !== this.draft.offset + bytes(this.draft.text)) return false;
      this.draft = { ...this.draft, text: this.draft.text + d.text };
    } else return true;
    this.supersede();
    this.changed();
    return true;
  }

  // The committed row of the draft's entry (key `entry + 1`) supersedes the draft.
  private supersede() {
    if (this.draft && this.rows.has(this.draft.key.entry + 1)) this.draft = null;
  }

  // ---- history: pages before the oldest held row, each with its own pinned cutoff ----
  async loadOlder(): Promise<boolean> {
    const held = [...this.rows.keys()].filter((at) => at > 0);
    if (!this.live || !this.older || held.length === 0) return false;
    const before = Math.min(...held);
    if (before <= 1) { this.older = false; this.changed(); return false; }
    const generation = this.generation;
    const r = await fetch(this.url(`/history?before=${before}&lines=${this.o.page ?? 64}`), { headers: this.headers() });
    if (!r.ok) throw new Error(`history: HTTP ${r.status}`);
    const page: { from: number; rows: Shown[] } = await r.json();
    if (generation !== this.generation) return false; // a Reset came first; its image stands
    for (const row of page.rows) if (!this.rows.has(row.at)) this.rows.set(row.at, row.line);
    this.older = page.from > 1;
    this.changed();
    return this.older;
  }
}
