// hirsel plugin UI — the author template.
//
// A plugin's UI lives here, at `plugins/<id>/ui/index.tsx`, and is compiled as
// part of the app: ordinary TSX, ordinary `solid-js` imports, the app's own
// Solid instance and Tailwind classes. The folder name is the plugin id. The app
// discovers this file at build time and initialises it only when the Host
// reports this plugin enabled.
import { createSignal, onCleanup, Show } from "solid-js";
import type { PluginApi } from "../../../app/src/plugins/types";

// The default export runs ONCE, with the plugin api, and may return a disposer.
// Everything you can do to the app is on this object:
//   api.id                       your plugin id (this folder's name)
//   api.label                    your human label, as the Host reports it
//   api.slots.register(slot, C)  mount a component — "home.section",
//                                "settings.section" or "task.panel"
//   api.fetch(path, init)        authenticated fetch against YOUR Host router;
//                                "/greet" → /api/plugins/hello/greet
//   api.onPush(topic, handler)   subscribe to your own plugin_push frames
export default function hello(api: PluginApi) {
  // A slot component is an ordinary Solid component: it RUNS ONCE and its
  // reactive expressions re-run on their own — do not write it like a React
  // function that re-renders. It receives `{ ctx }`: `{}` for home.section and
  // settings.section, `{ taskId }` for task.panel.
  function HelloCard() {
    const [name, setName] = createSignal("world");
    const [greeting, setGreeting] = createSignal("");
    const [ticks, setTicks] = createSignal(0);

    // A push subscription would live as long as the plugin does; onCleanup ties
    // this one to the component, so an unmount stops the updates.
    const off = api.onPush("tick", (data) => {
      setTicks(Number((data as { count?: number } | null)?.count ?? 0));
    });
    onCleanup(off);

    async function greet() {
      try {
        // Reaches YOUR Host router only — the app scopes the path to api.id, so
        // no plugin can call another's routes. Platform fetch semantics: a
        // non-2xx resolves, so check `ok` yourself.
        const response = await api.fetch("/greet", {
          method: "POST",
          body: JSON.stringify({ name: name() }),
        });
        if (!response.ok) {
          setGreeting(`error: HTTP ${response.status}`);
          return;
        }
        const body = (await response.json()) as { text?: string };
        setGreeting(body.text ?? "");
      } catch (error) {
        setGreeting(`error: ${error instanceof Error ? error.message : String(error)}`);
      }
    }

    return (
      <section class="rounded-xl border border-border bg-card p-4">
        <h3 class="text-sm font-medium text-foreground">{api.label}</h3>
        <div class="mt-3 flex items-center gap-2">
          <input
            class="h-9 w-40 rounded-lg border border-border bg-surface px-2 text-sm outline-none focus-visible:ring-2 focus-visible:ring-ring"
            aria-label="Name to greet"
            value={name()}
            onInput={(e) => setName(e.currentTarget.value)}
          />
          <button
            type="button"
            class="h-9 rounded-lg border border-border px-3 text-sm outline-none transition-colors hover:bg-muted focus-visible:ring-2 focus-visible:ring-ring"
            onClick={() => void greet()}
          >
            Greet
          </button>
        </div>
        {/* Reactive by construction: the signal read re-runs just this text. */}
        <Show when={greeting()}>
          <p class="mt-2 text-sm text-muted-foreground">{greeting()}</p>
        </Show>
        <p class="mt-1 font-mono text-xs text-muted-foreground">ticks: {ticks()}</p>
      </section>
    );
  }

  api.slots.register("home.section", HelloCard);

  // Optional: a returned disposer runs when the app tears the plugin down. Slot
  // registrations and push subscriptions are cleaned up for you.
  return () => {};
}
