import { fireEvent, render, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { PromptSnapshot } from "../../protocol";

const memStore = new Map<string, string>();
const memLocalStorage: Storage = {
  getItem: (key) => memStore.get(key) ?? null,
  setItem: (key, value) => void memStore.set(key, String(value)),
  removeItem: (key) => void memStore.delete(key),
  clear: () => memStore.clear(),
  key: (index) => [...memStore.keys()][index] ?? null,
  get length() {
    return memStore.size;
  },
};

const PROMPTS: PromptSnapshot = {
  agent: { text: "Bundled main prompt", is_default: true },
  fork: {
    current: { id: "gpt-5.6-luna", variant: "max" },
    available: [
      {
        id: "gpt-5.6-luna",
        label: "GPT-5.6 Luna",
        variants: ["low", "medium", "high", "xhigh", "max"],
        default_variant: "max",
      },
      {
        id: "gpt-5.6-sol",
        label: "GPT-5.6 Sol",
        variants: ["low", "medium", "high", "xhigh", "max"],
        default_variant: "medium",
      },
    ],
    prompt: { text: "Bundled fork prompt", is_default: true },
  },
};

const setAgentPrompt = vi.fn();
const setForkPrompt = vi.fn();
const setForkModel = vi.fn();

beforeEach(() => {
  vi.resetModules();
  memStore.clear();
  setAgentPrompt.mockReset();
  setForkPrompt.mockReset();
  setForkModel.mockReset();
  vi.stubGlobal("localStorage", memLocalStorage);
  vi.doMock("../../ws/client", () => ({
    clearStoredToken: vi.fn(),
    getStoredToken: () => "tok-abcd",
    getClient: () => ({ setAgentPrompt, setForkPrompt, setForkModel }),
  }));
});

afterEach(() => vi.unstubAllGlobals());

/** jsdom reports no client rects for anything, and the focus trap only treats
 * elements with a real box as focusable. Give one control an honest box so a
 * restoration assertion measures the trap's contract rather than jsdom's
 * layout engine (same shim as lib/focus.test.ts). */
function visibleBox(element: HTMLElement): HTMLElement {
  Object.defineProperty(element, "getClientRects", {
    configurable: true,
    value: () => [{ width: 44, height: 44 }],
  });
  return element;
}

/** The INLINE editor, addressed by its own id so the expanded dialog — which is
 * labelled by the same prompt name — can never stand in for it. */
const inlineEditor = (container: HTMLElement) =>
  container.querySelector<HTMLTextAreaElement>("#agent-prompt-editor");

async function mount(prompts?: PromptSnapshot) {
  const store = await import("../../store/store");
  store.dispatch({
    type: "hello_ok",
    payload: {
      type: "hello_ok",
      latest_msg_id: 0,
      messages: [],
      pings: [],
      prompts,
    },
  });
  store.openSettings("agents");
  const { SettingsSheet } = await import("./SettingsSheet");
  return { store, ...render(() => <SettingsSheet />) };
}

describe("Settings → Prompt", () => {
  it("shows the effective prompt, next-turn timing, and ephemeral fork contract", async () => {
    const { getByLabelText, getByText } = await mount(PROMPTS);
    expect((getByLabelText("Main agent system prompt") as HTMLTextAreaElement).value).toBe(
      "Bundled main prompt",
    );
    expect(getByText(/applies from the next turn/i)).toBeTruthy();
    expect(getByText(/Host configuration is appended automatically/i)).toBeTruthy();
    expect(getByText(/Runs once per incoming event to triage it/i)).toBeTruthy();
    expect((getByLabelText("Fork agent model") as HTMLSelectElement).value).toBe("gpt-5.6-luna");
    expect((getByLabelText("Fork agent reasoning variant") as HTMLSelectElement).value).toBe(
      "max",
    );
  });

  it("settles a save when the accepted snapshot is unchanged", async () => {
    const { getByLabelText, store } = await mount(PROMPTS);
    const editor = getByLabelText("Main agent system prompt") as HTMLTextAreaElement;
    fireEvent.input(editor, { target: { value: "   " } });
    fireEvent.click(getByLabelText("Save Main agent system prompt"));
    expect(setAgentPrompt).toHaveBeenCalledWith("   ");
    expect(editor).toBeDisabled();

    store.dispatch({ type: "prompts_changed", prompts: PROMPTS });

    await waitFor(() => expect(editor).not.toBeDisabled());
  });

  it("keeps the main prompt local until Save and settles from the broadcast", async () => {
    const { getByLabelText, store } = await mount(PROMPTS);
    const editor = getByLabelText("Main agent system prompt") as HTMLTextAreaElement;
    const save = getByLabelText("Save Main agent system prompt") as HTMLButtonElement;
    expect(save.disabled).toBe(true);

    fireEvent.input(editor, { target: { value: "Owner override" } });
    expect(save.disabled).toBe(false);
    expect(setAgentPrompt).not.toHaveBeenCalled();
    fireEvent.click(save);
    expect(setAgentPrompt).toHaveBeenCalledWith("Owner override");
    expect(editor.disabled).toBe(true);

    store.dispatch({
      type: "prompts_changed",
      prompts: { ...PROMPTS, agent: { text: "Owner override", is_default: false } },
    });
    await waitFor(() => expect(editor).not.toBeDisabled());
    expect(editor.value).toBe("Owner override");
    expect(save.disabled).toBe(true);
  });

  it("resets an override by sending an empty body", async () => {
    const overridden: PromptSnapshot = {
      ...PROMPTS,
      agent: { text: "Owner override", is_default: false },
    };
    const { getByLabelText } = await mount(overridden);
    fireEvent.click(getByLabelText("Reset Main agent system prompt to default"));
    expect(setAgentPrompt).toHaveBeenCalledWith("");
  });

  it("updates the fork model, reasoning variant, and prompt through dedicated ops", async () => {
    const { getByLabelText, store } = await mount(PROMPTS);
    fireEvent.change(getByLabelText("Fork agent model"), {
      target: { value: "gpt-5.6-sol" },
    });
    expect(setForkModel).toHaveBeenCalledWith("gpt-5.6-sol", "medium");

    // The full authoritative frame settles all fork controls.
    store.dispatch({
      type: "prompts_changed",
      prompts: {
        ...PROMPTS,
        fork: {
          ...PROMPTS.fork!,
          current: { id: "gpt-5.6-sol", variant: "medium" },
        },
      },
    });
    await waitFor(() => expect(getByLabelText("Fork agent model")).not.toBeDisabled());
    fireEvent.change(getByLabelText("Fork agent reasoning variant"), {
      target: { value: "high" },
    });
    expect(setForkModel).toHaveBeenLastCalledWith("gpt-5.6-sol", "high");

    store.dispatch({
      type: "prompts_changed",
      prompts: {
        ...PROMPTS,
        fork: {
          ...PROMPTS.fork!,
          current: { id: "gpt-5.6-sol", variant: "high" },
        },
      },
    });
    const editor = getByLabelText("Fork agent prompt") as HTMLTextAreaElement;
    fireEvent.input(editor, { target: { value: "Triage this event." } });
    fireEvent.click(getByLabelText("Save Fork agent prompt"));
    expect(setForkPrompt).toHaveBeenCalledWith("Triage this event.");
  });

  it("expands a prompt to the full viewport carrying the unsaved draft", async () => {
    const { getByLabelText, getByText, container } = await mount(PROMPTS);
    fireEvent.input(inlineEditor(container)!, { target: { value: "Half-written override" } });

    fireEvent.click(getByLabelText("Expand Main agent system prompt"));

    const expanded = getByLabelText(
      "Main agent system prompt (expanded)",
    ) as HTMLTextAreaElement;
    expect(expanded.value).toBe("Half-written override");
    // One editor at a time: the inline row stands down while the overlay is up.
    expect(inlineEditor(container)).toBeNull();
    // A true modal over Settings, with the one honest timing line.
    const panel = expanded.closest('[data-slot="prompt-editor-panel"]') as HTMLElement;
    expect(panel.getAttribute("role")).toBe("dialog");
    expect(panel.getAttribute("aria-modal")).toBe("true");
    expect(getByText(/Applies from the Agent's next turn\./)).toBeTruthy();
    expect(setAgentPrompt).not.toHaveBeenCalled();
  });

  it("Escape collapses the expanded editor, keeping the draft and restoring focus", async () => {
    const { getByLabelText, container } = await mount(PROMPTS);
    visibleBox(getByLabelText("Expand Main agent system prompt"));
    fireEvent.input(inlineEditor(container)!, {
      target: { value: "Kept across the round trip" },
    });
    fireEvent.click(getByLabelText("Expand Main agent system prompt"));
    fireEvent.input(getByLabelText("Main agent system prompt (expanded)"), {
      target: { value: "Edited while expanded" },
    });

    fireEvent.keyDown(window, { key: "Escape" });

    // Back to the inline row with the draft intact — and nothing was sent.
    expect(inlineEditor(container)?.value).toBe("Edited while expanded");
    expect(setAgentPrompt).not.toHaveBeenCalled();
    // Escape dismissed the editor and NOT Settings underneath it, and focus
    // came back to the control that opened it.
    expect(container.querySelector('[data-slot="settings-panel"]')).toBeTruthy();
    await waitFor(() =>
      expect(document.activeElement).toBe(
        visibleBox(getByLabelText("Expand Main agent system prompt")),
      ),
    );
  });

  it("saves from the expanded editor and settles on the prompts broadcast", async () => {
    const { getByLabelText, store, container } = await mount(PROMPTS);
    fireEvent.click(getByLabelText("Expand Main agent system prompt"));
    const expanded = getByLabelText(
      "Main agent system prompt (expanded)",
    ) as HTMLTextAreaElement;
    fireEvent.input(expanded, { target: { value: "Written in the expanded editor" } });
    fireEvent.click(getByLabelText("Save Main agent system prompt"));

    expect(setAgentPrompt).toHaveBeenCalledWith("Written in the expanded editor");
    expect(expanded).toBeDisabled();

    store.dispatch({
      type: "prompts_changed",
      prompts: { ...PROMPTS, agent: { text: "Written in the expanded editor", is_default: false } },
    });

    await waitFor(() => expect(expanded).not.toBeDisabled());
    expect(expanded.value).toBe("Written in the expanded editor");
    expect((getByLabelText("Save Main agent system prompt") as HTMLButtonElement).disabled).toBe(
      true,
    );
    // The editor stays open — saving is not leaving.
    expect(inlineEditor(container)).toBeNull();
  });

  it("expands the fork prompt on its own, with its own caption", async () => {
    const { getByLabelText, getByText, container } = await mount(PROMPTS);
    fireEvent.click(getByLabelText("Expand Fork agent prompt"));
    expect(
      (getByLabelText("Fork agent prompt (expanded)") as HTMLTextAreaElement).value,
    ).toBe("Bundled fork prompt");
    expect(getByText("Stored for the fork runtime. The running Agent is unaffected.")).toBeTruthy();
    // The main agent's editor is untouched by the fork's expansion.
    expect(inlineEditor(container)).toBeTruthy();
  });

  it("hides the prompt editors for an older host with no prompt snapshot", async () => {
    const { queryByLabelText } = await mount();
    expect(queryByLabelText("Main agent system prompt")).toBeNull();
    expect(queryByLabelText("Fork agent prompt")).toBeNull();
  });
});
