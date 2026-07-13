import { fireEvent, render } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ModelSnapshot, SubagentModelCatalog } from "../../protocol";

// SettingsSheet reads the global `localStorage`; back it with an in-memory store
// (mirrors settings.test.tsx).
const memStore = new Map<string, string>();
const memLocalStorage: Storage = {
  getItem: (k) => (memStore.has(k) ? (memStore.get(k) as string) : null),
  setItem: (k, v) => void memStore.set(k, String(v)),
  removeItem: (k) => void memStore.delete(k),
  clear: () => memStore.clear(),
  key: (i) => [...memStore.keys()][i] ?? null,
  get length() {
    return memStore.size;
  },
};

const MODEL: ModelSnapshot = {
  current: { id: "gpt-5.6-sol", variant: "medium" },
  available: [
    {
      id: "gpt-5.6-sol",
      label: "GPT-5.6 Sol",
      variants: ["low", "medium", "high", "xhigh", "max"],
      default_variant: "medium",
    },
  ],
};

const CATALOG: SubagentModelCatalog = {
  providers: [
    {
      provider: "codex",
      label: "Codex CLI",
      models: [
        {
          id: "gpt-5.5",
          label: "GPT-5.5",
          variants: ["low", "medium", "high"],
          default_variant: "high",
          enabled: true,
        },
      ],
    },
    {
      provider: "claude",
      label: "Claude Code CLI",
      models: [
        {
          id: "claude-opus-4-8",
          label: "Claude Opus 4.8",
          variants: ["low", "medium", "high"],
          default_variant: "high",
          enabled: true,
        },
      ],
    },
  ],
};

const setModel = vi.fn();
const setSubagentModel = vi.fn();

beforeEach(() => {
  vi.resetModules();
  memStore.clear();
  setModel.mockReset();
  setSubagentModel.mockReset();
  vi.stubGlobal("localStorage", memLocalStorage);
  // SettingsSheet uses getClient() to send model commands; mock the module so
  // the sends are observable without a real socket. (clearStoredToken/
  // getStoredToken are the other exports SettingsSheet imports.)
  vi.doMock("../../ws/client", () => ({
    clearStoredToken: vi.fn(),
    getStoredToken: () => "tok-abcd",
    getClient: () => ({ setModel, setSubagentModel }),
  }));
});

afterEach(() => vi.unstubAllGlobals());

async function mount(seed?: { model?: ModelSnapshot; subagent_models?: SubagentModelCatalog }) {
  const store = await import("../../store/store");
  store.dispatch({
    type: "hello_ok",
    payload: { type: "hello_ok", latest_msg_id: 0, messages: [], pings: [], ...seed },
  });
  store.setSettingsOpen(true);
  const { SettingsSheet } = await import("./SettingsSheet");
  return render(() => <SettingsSheet />);
}

describe("Settings → Models: main agent", () => {
  it("renders the model + reasoning controls from the seeded snapshot", async () => {
    const { getByLabelText } = await mount({ model: MODEL });
    const modelSelect = getByLabelText("Main agent model") as HTMLSelectElement;
    const variantSelect = getByLabelText("Main agent reasoning variant") as HTMLSelectElement;
    expect(modelSelect.value).toBe("gpt-5.6-sol");
    expect(variantSelect.value).toBe("medium");
    // Variant options come from the selected model's variants.
    expect([...variantSelect.options].map((o) => o.value)).toEqual([
      "low",
      "medium",
      "high",
      "xhigh",
      "max",
    ]);
  });

  it("changing the reasoning variant enqueues set_model with the right payload", async () => {
    const { getByLabelText } = await mount({ model: MODEL });
    const variantSelect = getByLabelText("Main agent reasoning variant") as HTMLSelectElement;
    fireEvent.change(variantSelect, { target: { value: "high" } });
    expect(setModel).toHaveBeenCalledWith("gpt-5.6-sol", "high");
  });

  it("hides the main-agent subsection when model is null", async () => {
    const { queryByLabelText } = await mount({ subagent_models: CATALOG });
    expect(queryByLabelText("Main agent model")).toBeNull();
  });
});

describe("Settings → Models: sub-agent models", () => {
  it("renders provider groups and model rows", async () => {
    const { getByText, getByLabelText } = await mount({ subagent_models: CATALOG });
    expect(getByText("Codex CLI")).toBeTruthy();
    expect(getByText("Claude Code CLI")).toBeTruthy();
    expect(getByText("GPT-5.5")).toBeTruthy();
    expect(getByLabelText("Enable GPT-5.5")).toBeTruthy();
    expect(getByLabelText("GPT-5.5 default variant")).toBeTruthy();
  });

  it("toggling enable sends set_subagent_model with the full row payload", async () => {
    const { getByLabelText } = await mount({ subagent_models: CATALOG });
    // gpt-5.5 starts enabled; toggling sends the full state (now disabled) with
    // its unchanged default_variant.
    fireEvent.click(getByLabelText("Enable GPT-5.5"));
    expect(setSubagentModel).toHaveBeenCalledWith("codex", "gpt-5.5", false, "high");
  });

  it("changing the default variant sends set_subagent_model with the full row payload", async () => {
    const { getByLabelText } = await mount({ subagent_models: CATALOG });
    const select = getByLabelText("GPT-5.5 default variant") as HTMLSelectElement;
    fireEvent.change(select, { target: { value: "low" } });
    // Full state: still enabled, new default_variant.
    expect(setSubagentModel).toHaveBeenCalledWith("codex", "gpt-5.5", true, "low");
  });

  it("hides the sub-agent subsection when subagentModels is null", async () => {
    const { queryByText } = await mount({ model: MODEL });
    expect(queryByText("Sub-agent models")).toBeNull();
  });
});
