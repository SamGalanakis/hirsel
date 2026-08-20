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
    current: { id: "gpt-5.6-luna", variant: "medium" },
    available: [
      {
        id: "gpt-5.6-sol",
        label: "GPT-5.6 Sol",
        variants: ["low", "medium", "high"],
        default_variant: "medium",
      },
      {
        id: "gpt-5.6-luna",
        label: "GPT-5.6 Luna",
        variants: ["low", "medium", "high"],
        default_variant: "low",
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
  store.openSettings();
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
      "medium",
    );
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

  it("hides the section for an older host with no prompt snapshot", async () => {
    const { queryByText, queryByLabelText } = await mount();
    expect(queryByText("Prompt")).toBeNull();
    expect(queryByLabelText("Main agent system prompt")).toBeNull();
  });
});
