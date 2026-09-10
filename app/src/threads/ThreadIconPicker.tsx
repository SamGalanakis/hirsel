import { createEffect, createSignal, For, onCleanup, Show } from "solid-js";
import { historyId } from "../lib/history";
import { createOverlayPresence } from "../lib/focus";
import { state } from "../store/store";
import { ThreadAvatar } from "./ThreadAvatar";
import { threadAction } from "./store";
import { threadIconError, threadIconPresets, threadIconTarget, setThreadIconTarget } from "./icon-picker";

const button = "inline-flex min-h-11 items-center justify-center rounded-lg px-3 text-sm hover:bg-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-40";
export function ThreadIconPicker() {
  let dialog: HTMLDialogElement | undefined;
  let restore: HTMLElement | null = null;
  const [value, setValue] = createSignal<string | null>(null);
  const close = () => setThreadIconTarget(null);
  const error = () => threadIconError(value());
  createOverlayPresence(() => threadIconTarget() !== null);
  onCleanup(() => { dialog?.close(); close(); });
  createEffect(() => ({ target: threadIconTarget(), history: historyId() }), ({ target, history }) => {
    if (target && target.history !== history) { close(); return; }
    if (target) {
      setValue(target.thread.icon ?? null);
      restore = document.activeElement instanceof HTMLElement ? document.activeElement : null;
      if (!dialog?.open) dialog?.showModal();
      const frame = requestAnimationFrame(() => dialog?.querySelector<HTMLInputElement>("input")?.focus());
      return () => cancelAnimationFrame(frame);
    } else if (dialog?.open) {
      dialog.close();
      if (restore?.isConnected) restore.focus();
    }
  });
  const save = (event: SubmitEvent) => {
    event.preventDefault();
    const target = threadIconTarget();
    if (!target || target.history !== historyId() || error() || state.connection !== "connected") return;
    threadAction(target.thread.id, "set_icon", { icon: value() }, target.thread.revision);
    close();
  };
  return <dialog ref={node => { dialog = node; }} aria-label="Change thread icon"
    onCancel={event => { event.preventDefault(); close(); }}
    onKeyDown={event => { if (event.key === "Escape") { event.preventDefault(); event.stopPropagation(); close(); } }}
    class="m-auto max-h-[calc(100dvh-2rem)] w-[min(24rem,calc(100vw-2rem))] overflow-y-auto rounded-xl border border-border bg-background p-5 text-foreground backdrop:bg-black/30">
    <Show when={threadIconTarget()}>{target => <form onSubmit={save} class="space-y-4">
      <h2 class="text-base font-semibold">Change thread icon</h2>
      <div class="flex items-center gap-2"><ThreadAvatar thread={{ ...target().thread, icon: value() }} /><span class="min-w-0 break-words text-sm">{target().thread.title}</span></div>
      <div class="grid grid-cols-6 gap-1" role="group" aria-label="Suggested icons"><For each={threadIconPresets}>{preset => <button type="button" class={`${button} px-0 text-xl aria-pressed:bg-muted`} aria-label={preset.label} aria-pressed={value() === preset.icon ? "true" : "false"} title={preset.label} onClick={() => setValue(preset.icon)}>{preset.icon}</button>}</For></div>
      <label class="block space-y-2 text-sm">Custom emoji or symbol<input value={value() ?? ""} onInput={event => setValue(event.currentTarget.value)} aria-invalid={error() ? "true" : undefined} aria-describedby={error() ? "thread-icon-error" : undefined} class="block min-h-11 w-full rounded-lg border border-border bg-transparent px-3 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring" /></label>
      <Show when={error()}><p id="thread-icon-error" role="alert" class="text-sm text-status-danger">{error()}</p></Show>
      <div class="flex flex-wrap justify-between gap-2"><button type="button" class={button} onClick={() => setValue(null)}>Use default</button><div class="flex gap-1"><button type="button" class={button} onClick={close}>Cancel</button><button type="submit" class={`${button} bg-primary text-primary-foreground hover:bg-primary/90`} disabled={Boolean(error()) || state.connection !== "connected"}>Save icon</button></div></div>
    </form>}</Show>
  </dialog>;
}
