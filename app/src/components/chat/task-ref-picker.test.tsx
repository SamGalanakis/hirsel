import { fireEvent, render } from "@solidjs/testing-library";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { EventKind } from "../../protocol";
import type { Blob, EventItem } from "../../protocol";
import type { AttachmentsController } from "./useAttachments";

// The `#` Task-ref picker in the standing composer: what the trigger opens, how
// the keyboard drives it, what the send ends up carrying, and — the one that
// binds it to the rest of the app — that its Esc is its own rung of the ladder
// and never reaches the Task focus behind it.
//
// Fresh store singleton per test (the picker's overlay presence and the Esc
// ladder both read module state), and a real localStorage so the composer's
// draft persistence does not leak a `#` between cases.
function installLocalStorage() {
  const store = new Map<string, string>();
  const ls: Storage = {
    getItem: (k) => (store.has(k) ? (store.get(k) as string) : null),
    setItem: (k, v) => void store.set(k, String(v)),
    removeItem: (k) => void store.delete(k),
    clear: () => store.clear(),
    key: (i) => Array.from(store.keys())[i] ?? null,
    get length() {
      return store.size;
    },
  };
  Object.defineProperty(globalThis, "localStorage", { value: ls, configurable: true });
}

beforeEach(() => {
  vi.resetModules();
  installLocalStorage();
});

function stubAttachments(): AttachmentsController {
  return {
    files: () => [],
    addFiles: () => {},
    addPastedFiles: () => {},
    addPastedText: () => {},
    addFromTransfer: () => {},
    takeText: () => null,
    removeFile: () => {},
    retry: () => {},
    clear: () => {},
    uploadAll: async () => [] as Blob[],
  };
}

function task(id: number, name: string): EventItem {
  return {
    id,
    kind: EventKind.Judgment,
    source: { kind: "agent", ref: "host" },
    name,
    description: name,
    ui: [],
    requires_response: true,
    quick_replies: [],
    status: "open",
    read: false,
    anchor: id * 10,
    ts: "2026-08-18T10:00:00Z",
  };
}

const FIELD = [task(1, "deploy-4821"), task(2, "auth-pr"), task(3, "nightly-backup")];

async function renderComposer(props: { onSend?: (...a: unknown[]) => void; tasks?: EventItem[] } = {}) {
  const { Composer } = await import("./Composer");
  const utils = render(() => (
    <Composer
      attachments={stubAttachments()}
      thinking={false}
      tasks={props.tasks ?? FIELD}
      onSend={(props.onSend ?? (() => {})) as never}
      onStop={() => {}}
      getLastOwnerBody={() => null}
    />
  ));
  const textarea = utils.container.querySelector('[data-composer="main"]') as HTMLTextAreaElement;
  const picker = () => utils.container.querySelector('[data-slot="task-ref-picker"]');
  const options = () =>
    Array.from(utils.container.querySelectorAll<HTMLElement>("[data-task-ref-option]"));
  return { ...utils, textarea, picker, options };
}

describe("the # trigger", () => {
  it("opens on a lone # and offers the whole field, newest-first", async () => {
    const { textarea, picker, options } = await renderComposer();
    await userEvent.type(textarea, "#");
    expect(picker()).not.toBeNull();
    expect(options().map((el) => el.dataset.taskRefOption)).toEqual(["3", "2", "1"]);
  });

  it("filters as you type, by name and by id", async () => {
    const { textarea, options } = await renderComposer();
    await userEvent.type(textarea, "#auth");
    expect(options().map((el) => el.dataset.taskRefOption)).toEqual(["2"]);

    await userEvent.clear(textarea);
    await userEvent.type(textarea, "#3");
    expect(options().map((el) => el.dataset.taskRefOption)).toEqual(["3"]);
  });

  it("stays shut mid-word, and closes once the query names nothing", async () => {
    const { textarea, picker } = await renderComposer();
    await userEvent.type(textarea, "issue#1");
    expect(picker()).toBeNull();

    await userEvent.clear(textarea);
    await userEvent.type(textarea, "#zzz");
    expect(picker()).toBeNull();
  });

  it("never opens when there is nothing to cite", async () => {
    const { textarea, picker } = await renderComposer({ tasks: [] });
    await userEvent.type(textarea, "#");
    expect(picker()).toBeNull();
  });
});

describe("picker keyboard", () => {
  it("moves the active row with the arrows and wraps", async () => {
    const { textarea, options } = await renderComposer();
    await userEvent.type(textarea, "#");
    const selected = () => options().find((el) => el.getAttribute("aria-selected") === "true");
    expect(selected()?.dataset.taskRefOption).toBe("3");

    fireEvent.keyDown(textarea, { key: "ArrowDown" });
    expect(selected()?.dataset.taskRefOption).toBe("2");
    fireEvent.keyDown(textarea, { key: "ArrowUp" });
    expect(selected()?.dataset.taskRefOption).toBe("3");
    fireEvent.keyDown(textarea, { key: "ArrowUp" });
    expect(selected()?.dataset.taskRefOption).toBe("1");
  });

  it("points the composer at the active row for assistive tech", async () => {
    const { textarea } = await renderComposer();
    await userEvent.type(textarea, "#");
    expect(textarea.getAttribute("aria-activedescendant")).toBe("task-ref-picker-option-3");
    expect(textarea.getAttribute("aria-controls")).toBe("task-ref-picker");
  });

  it("inserts the ref on Enter, closes, and does NOT send", async () => {
    const onSend = vi.fn();
    const { textarea, picker } = await renderComposer({ onSend });
    await userEvent.type(textarea, "same as #auth");
    fireEvent.keyDown(textarea, { key: "Enter" });
    expect(onSend).not.toHaveBeenCalled();
    expect(picker()).toBeNull();
    expect(textarea.value).toBe("same as #2 ");
  });

  it("accepts on Tab without queueing the draft", async () => {
    const onSend = vi.fn();
    const { textarea } = await renderComposer({ onSend });
    await userEvent.type(textarea, "#nightly");
    fireEvent.keyDown(textarea, { key: "Tab" });
    expect(onSend).not.toHaveBeenCalled();
    expect(textarea.value).toBe("#3 ");
  });

  it("accepts on a tap, keeping the caret in the composer", async () => {
    const { textarea, options } = await renderComposer();
    await userEvent.type(textarea, "#dep");
    const row = options()[0];
    const event = new MouseEvent("pointerdown", { bubbles: true, cancelable: true });
    row.dispatchEvent(event);
    expect(event.defaultPrevented).toBe(true);
    expect(textarea.value).toBe("#1 ");
  });
});

describe("Esc is the picker's own rung of the ladder", () => {
  it("closes the picker and leaves the focused Task alone", async () => {
    const { focusTask, state } = await import("../../store/store");
    const { installGlobalKeymap } = await import("../../lib/keymap");
    const dispose = installGlobalKeymap();
    try {
      focusTask(1);
      const { textarea, picker } = await renderComposer();
      await userEvent.type(textarea, "#dep");
      expect(picker()).not.toBeNull();

      // One Escape, dispatched exactly as the browser would: from the caret,
      // bubbling toward the window layer that owns the last rung.
      fireEvent.keyDown(textarea, { key: "Escape" });

      expect(picker()).toBeNull();
      expect(state.focusedTaskId).toBe(1);
      expect(textarea.value).toBe("#dep");

      // A second Escape, with nothing left owning it, DOES clear focus.
      fireEvent.keyDown(textarea, { key: "Escape" });
      expect(state.focusedTaskId).toBeNull();
    } finally {
      dispose();
    }
  });

  it("stays dismissed until the Owner types again", async () => {
    const { textarea, picker } = await renderComposer();
    await userEvent.type(textarea, "#dep");
    fireEvent.keyDown(textarea, { key: "Escape" });
    fireEvent.keyUp(textarea, { key: "Escape" });
    expect(picker()).toBeNull();

    await userEvent.type(textarea, "l");
    expect(picker()).not.toBeNull();
  });
});

describe("the send carries what the body says", () => {
  it("resolves every standing ref into mentions, in order and deduped", async () => {
    const onSend = vi.fn();
    const { textarea } = await renderComposer({ onSend });
    fireEvent.input(textarea, { target: { value: "roll #3 back before #1, then #3 again" } });
    fireEvent.keyDown(textarea, { key: "Enter" });
    expect(onSend).toHaveBeenCalledOnce();
    expect(onSend.mock.calls[0][4]).toEqual([3, 1]);
  });

  it("drops a mention the moment its token is deleted", async () => {
    const onSend = vi.fn();
    const { textarea } = await renderComposer({ onSend });
    fireEvent.input(textarea, { target: { value: "look at #2" } });
    fireEvent.input(textarea, { target: { value: "look at" } });
    fireEvent.keyDown(textarea, { key: "Enter" });
    expect(onSend.mock.calls[0][4]).toEqual([]);
  });

  it("sends an unresolvable ref as plain prose", async () => {
    const onSend = vi.fn();
    const { textarea } = await renderComposer({ onSend });
    fireEvent.input(textarea, { target: { value: "what about #99?" } });
    fireEvent.keyDown(textarea, { key: "Enter" });
    expect(onSend.mock.calls[0][0]).toBe("what about #99?");
    expect(onSend.mock.calls[0][4]).toEqual([]);
  });
});
