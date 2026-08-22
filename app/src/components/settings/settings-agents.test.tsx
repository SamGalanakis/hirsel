import { fireEvent, render, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type {
  ModelSnapshot,
  PromptSnapshot,
  ProviderRoster,
  SubagentModelCatalog,
} from "../../protocol";

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
          id: "gpt-5.6-sol",
          label: "Sol",
          variants: ["low", "medium", "high", "xhigh", "max", "ultra"],
          enabled_variants: ["low", "medium", "high", "xhigh", "max", "ultra"],
          enabled: true,
        },
        {
          id: "gpt-5.6-terra",
          label: "Terra",
          variants: ["low", "medium", "high", "xhigh", "max", "ultra"],
          enabled_variants: ["low", "medium", "high", "xhigh", "max", "ultra"],
          enabled: true,
        },
        {
          id: "gpt-5.6-luna",
          label: "Luna",
          variants: ["low", "medium", "high", "xhigh", "max"],
          enabled_variants: ["low", "medium", "high", "xhigh", "max"],
          enabled: true,
        },
      ],
    },
    {
      provider: "claude",
      label: "Claude Code CLI",
      models: [
        {
          id: "claude-fable-5",
          label: "Fable 5",
          variants: ["low", "medium", "high"],
          enabled_variants: ["low", "medium", "high"],
          enabled: false,
        },
        {
          id: "claude-opus-4-8",
          label: "Opus 4.8",
          variants: ["low", "medium", "high"],
          enabled_variants: ["low", "medium", "high"],
          enabled: true,
        },
        {
          id: "claude-sonnet-5",
          label: "Sonnet 5",
          variants: ["low", "medium", "high"],
          enabled_variants: ["low", "medium", "high"],
          enabled: true,
        },
      ],
    },
  ],
};

/** A roster with one OAuth instance, one Sub-agents-only instance, and one
 * OpenAI-compatible endpoint that takes a free-text model id. */
const ROSTER: ProviderRoster = {
  instances: [
    {
      id: "codex",
      kind: "codex",
      label: "Codex",
      default_model: "gpt-5.6-sol",
      detection: { detected: true, path: "/home/owner/.codex/auth.json" },
      agent_selectable: true,
      removable: false,
    },
    {
      id: "claude",
      kind: "claude",
      label: "Claude",
      detection: { detected: true, path: "/home/owner/.claude/.credentials.json" },
      agent_selectable: false,
      removable: false,
    },
    {
      id: "openrouter",
      kind: "openai_compatible",
      label: "OpenRouter",
      base_url: "https://openrouter.ai/api/v1",
      api_key: { present: true, tail: "9f2c" },
      default_model: "z-ai/glm-5",
      agent_selectable: true,
      removable: true,
    },
  ],
  booted_provider_id: "codex",
};

const PROMPTS: PromptSnapshot = {
  agent: { text: "Bundled main prompt", is_default: true },
  fork: {
    current: { id: "gpt-5.6-luna", variant: "max" },
    available: [
      {
        id: "gpt-5.6-luna",
        label: "GPT-5.6 Luna",
        variants: ["low", "medium", "max"],
        default_variant: "max",
      },
    ],
    prompt: { text: "Bundled fork prompt", is_default: true },
    provider_id: "codex",
  },
};

const setModel = vi.fn();
const setSubagentModel = vi.fn();
const setAgentProvider = vi.fn();

beforeEach(() => {
  vi.resetModules();
  memStore.clear();
  setModel.mockReset();
  setSubagentModel.mockReset();
  setAgentProvider.mockReset();
  vi.stubGlobal("localStorage", memLocalStorage);
  // SettingsSheet uses getClient() to send model commands; mock the module so
  // the sends are observable without a real socket. (clearStoredToken/
  // getStoredToken are the other exports SettingsSheet imports.)
  vi.doMock("../../ws/client", () => ({
    clearStoredToken: vi.fn(),
    getStoredToken: () => "tok-abcd",
    getClient: () => ({ setModel, setSubagentModel, setAgentProvider }),
  }));
});

afterEach(() => vi.unstubAllGlobals());

async function mount(seed?: {
  model?: ModelSnapshot;
  subagent_models?: SubagentModelCatalog;
  prompts?: PromptSnapshot;
  providers?: ProviderRoster;
}) {
  const store = await import("../../store/store");
  store.dispatch({
    type: "hello_ok",
    payload: { type: "hello_ok", latest_msg_id: 0, messages: [], pings: [], ...seed },
  });
  store.openSettings("agents");
  const { SettingsSheet } = await import("./SettingsSheet");
  return render(() => <SettingsSheet />);
}

describe("Settings → Agents: main agent", () => {
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

  // The host broadcasts `model_changed` ONLY when the value actually changed,
  // and a rejected command returns before broadcasting — so the pending state
  // must never depend on an echo arriving.
  it("settles the pending controls on timeout when no broadcast ever arrives", async () => {
    const { getByLabelText } = await mount({ model: MODEL });
    const modelSelect = getByLabelText("Main agent model") as HTMLSelectElement;
    const variantSelect = getByLabelText("Main agent reasoning variant") as HTMLSelectElement;

    vi.useFakeTimers();
    try {
      fireEvent.change(variantSelect, { target: { value: "high" } });
      expect(setModel).toHaveBeenCalledWith("gpt-5.6-sol", "high");
      expect(variantSelect.disabled).toBe(true);
      expect(modelSelect.disabled).toBe(true);

      vi.advanceTimersByTime(4000);

      expect(variantSelect.disabled).toBe(false);
      expect(modelSelect.disabled).toBe(false);
    } finally {
      vi.useRealTimers();
    }

    // …and the settled control still works.
    fireEvent.change(variantSelect, { target: { value: "max" } });
    expect(setModel).toHaveBeenLastCalledWith("gpt-5.6-sol", "max");
  });

  it("settles the pending controls when the host answers with an error frame", async () => {
    const { getByLabelText } = await mount({ model: MODEL });
    const store = await import("../../store/store");
    const modelSelect = getByLabelText("Main agent model") as HTMLSelectElement;
    const variantSelect = getByLabelText("Main agent reasoning variant") as HTMLSelectElement;

    fireEvent.change(variantSelect, { target: { value: "high" } });
    expect(variantSelect.disabled).toBe(true);

    store.setProtocolError("unknown variant");

    await waitFor(() => expect(variantSelect).not.toBeDisabled());
    expect(modelSelect.disabled).toBe(false);
  });

  it("hides the main-agent subsection when model is null", async () => {
    const { queryByLabelText } = await mount({ subagent_models: CATALOG });
    expect(queryByLabelText("Main agent model")).toBeNull();
  });
});

describe("Settings → Agents: sub-agents", () => {
  it("renders provider groups and model rows", async () => {
    const { getByText, getByLabelText, queryByText } = await mount({
      subagent_models: CATALOG,
    });
    expect(getByText("Codex CLI")).toBeTruthy();
    expect(getByText("Claude Code CLI")).toBeTruthy();
    expect(getByText("Terra")).toBeTruthy();
    expect(getByText("Luna")).toBeTruthy();
    expect(getByText("Sol")).toBeTruthy();
    expect(getByLabelText("Enable Terra")).toBeTruthy();
    expect(getByLabelText("Terra enabled variants")).toBeTruthy();
    expect(queryByText("GPT-5.5")).toBeNull();
  });

  it("orders models from smartest to least capable within each provider", async () => {
    const { getByText } = await mount({ subagent_models: CATALOG });
    const precedes = (first: Element, second: Element) =>
      Boolean(first.compareDocumentPosition(second) & Node.DOCUMENT_POSITION_FOLLOWING);

    expect(precedes(getByText("Sol"), getByText("Terra"))).toBe(true);
    expect(precedes(getByText("Terra"), getByText("Luna"))).toBe(true);
    expect(precedes(getByText("Fable 5"), getByText("Opus 4.8"))).toBe(true);
    expect(precedes(getByText("Opus 4.8"), getByText("Sonnet 5"))).toBe(true);
  });

  it("collapses each provider independently", async () => {
    const { getByLabelText, queryByText, getByText } = await mount({
      subagent_models: CATALOG,
    });

    fireEvent.click(getByLabelText("Collapse Codex CLI models"));
    expect(queryByText("Sol")).toBeNull();
    expect(getByText("Fable 5")).toBeTruthy();
    expect(getByLabelText("Expand Codex CLI models").getAttribute("aria-expanded")).toBe(
      "false",
    );

    fireEvent.click(getByLabelText("Expand Codex CLI models"));
    expect(getByText("Sol")).toBeTruthy();
  });

  it("toggling enable sends set_subagent_model with the full row payload", async () => {
    const { getByLabelText } = await mount({ subagent_models: CATALOG });
    fireEvent.click(getByLabelText("Enable Terra"));
    expect(setSubagentModel).toHaveBeenCalledWith(
      "codex",
      "gpt-5.6-terra",
      false,
      ["low", "medium", "high", "xhigh", "max", "ultra"],
    );
  });

  it("variants are independent multi-select controls", async () => {
    const { getByLabelText } = await mount({ subagent_models: CATALOG });
    const high = getByLabelText("Disable Terra high variant");
    expect(high.getAttribute("aria-pressed")).toBe("true");
    fireEvent.click(high);
    expect(setSubagentModel).toHaveBeenCalledWith(
      "codex",
      "gpt-5.6-terra",
      true,
      ["low", "medium", "xhigh", "max", "ultra"],
    );
  });

  it("settles a pending row when the host broadcasts nested catalog changes", async () => {
    const { getByLabelText } = await mount({ subagent_models: CATALOG });
    fireEvent.click(getByLabelText("Disable Terra high variant"));

    const store = await import("../../store/store");
    const terra = CATALOG.providers[0].models.find(
      (model) => model.id === "gpt-5.6-terra",
    )!;
    store.dispatch({
      type: "subagent_models_changed",
      catalog: {
        providers: [
          {
            ...CATALOG.providers[0],
            models: CATALOG.providers[0].models.map((model) =>
              model.id === terra.id
                ? {
                    ...terra,
                    enabled_variants: terra.enabled_variants.filter(
                      (variant) => variant !== "high",
                    ),
                  }
                : model,
            ),
          },
          CATALOG.providers[1],
        ],
      },
    });

    await waitFor(() =>
      expect(getByLabelText("Enable Terra high variant")).not.toBeDisabled(),
    );
    fireEvent.click(getByLabelText("Enable Terra high variant"));
    expect(setSubagentModel).toHaveBeenLastCalledWith(
      "codex",
      "gpt-5.6-terra",
      true,
      ["low", "medium", "high", "xhigh", "max", "ultra"],
    );
  });

  it("settles a pending row on timeout, and on an error frame", async () => {
    const { getByLabelText } = await mount({ subagent_models: CATALOG });
    const toggle = () => getByLabelText("Enable Terra") as HTMLInputElement;

    vi.useFakeTimers();
    try {
      fireEvent.click(toggle());
      expect(toggle()).toBeDisabled();
      vi.advanceTimersByTime(4000);
      expect(toggle()).not.toBeDisabled();
    } finally {
      vi.useRealTimers();
    }

    const store = await import("../../store/store");
    fireEvent.click(toggle());
    expect(toggle()).toBeDisabled();
    store.setProtocolError("model unavailable");
    await waitFor(() => expect(toggle()).not.toBeDisabled());
  });

  it("hides the sub-agent subsection when subagentModels is null", async () => {
    const { queryByText } = await mount({ model: MODEL });
    expect(queryByText("Sub-agents")).toBeNull();
  });
});

describe("Settings → Agents: providers", () => {
  it("offers only agent-selectable providers, so Claude is never one of them", async () => {
    const { getByLabelText } = await mount({
      model: { ...MODEL, provider_id: "codex" },
      prompts: PROMPTS,
      providers: ROSTER,
    });
    for (const name of ["Main agent provider", "Fork agent provider"]) {
      const select = getByLabelText(name) as HTMLSelectElement;
      expect([...select.options].map((option) => option.value)).toEqual(["codex", "openrouter"]);
    }
  });

  it("sends set_agent_provider for the slot whose select changed", async () => {
    const { getByLabelText } = await mount({
      model: { ...MODEL, provider_id: "codex" },
      prompts: PROMPTS,
      providers: ROSTER,
    });
    fireEvent.change(getByLabelText("Main agent provider"), {
      target: { value: "openrouter" },
    });
    expect(setAgentProvider).toHaveBeenCalledWith("main", "openrouter");

    fireEvent.change(getByLabelText("Fork agent provider"), {
      target: { value: "openrouter" },
    });
    expect(setAgentProvider).toHaveBeenLastCalledWith("fork", "openrouter");
  });

  it("says nothing while the chosen provider is the one the host booted on", async () => {
    const { queryByText } = await mount({
      model: { ...MODEL, provider_id: "codex" },
      providers: ROSTER,
    });
    expect(queryByText(/until the host restarts/i)).toBeNull();
  });

  it("names the booted provider once the stored choice has moved on", async () => {
    const { getByText } = await mount({
      model: { ...MODEL, provider_id: "openrouter", free_text_model: true, available: [] },
      providers: ROSTER,
    });
    expect(
      getByText("Saved. The running Agent stays on Codex until the host restarts."),
    ).toBeTruthy();
  });

  // The field-observed defect: booted on OpenRouter, the Owner picks Codex, and
  // the Model row stayed a free-text input with no Reasoning select and no
  // restart notice. The host now broadcasts the whole reshaped snapshot, so the
  // curated controls have to appear on that frame alone — no reconnect.
  it("reshapes the model row when model_changed moves the stored provider", async () => {
    const { getByLabelText, getByText, queryByLabelText } = await mount({
      model: {
        current: { id: "z-ai/glm-5", variant: "default" },
        available: [],
        provider_id: "openrouter",
        free_text_model: true,
      },
      providers: { ...ROSTER, booted_provider_id: "openrouter" },
    });
    expect(getByLabelText("Main agent model id")).toBeTruthy();
    expect(queryByLabelText("Main agent reasoning variant")).toBeNull();
    expect(getByText("Applies from the Agent's next turn.")).toBeTruthy();

    const store = await import("../../store/store");
    store.dispatch({
      type: "model_changed",
      model: { ...MODEL, provider_id: "codex", free_text_model: false },
    });

    await waitFor(() =>
      expect(getByLabelText("Main agent reasoning variant")).toBeTruthy(),
    );
    expect(queryByLabelText("Main agent model id")).toBeNull();
    expect((getByLabelText("Main agent model") as HTMLSelectElement).value).toBe(
      "gpt-5.6-sol",
    );
    // ...and both captions now tell the truth about the restart.
    expect(
      getByText("Saved. The running Agent stays on OpenRouter until the host restarts."),
    ).toBeTruthy();
    expect(getByText("Takes effect when the host restarts.")).toBeTruthy();
  });

  it("takes a free-text model id, refusing a blank one without sending", async () => {
    const { getByLabelText, getByText, queryByLabelText } = await mount({
      model: {
        current: { id: "z-ai/glm-5", variant: "medium" },
        available: [],
        provider_id: "openrouter",
        free_text_model: true,
      },
      providers: ROSTER,
    });
    // The provider chooses the reasoning level in this mode, so there is no
    // variant control to offer.
    expect(queryByLabelText("Main agent reasoning variant")).toBeNull();

    const field = getByLabelText("Main agent model id") as HTMLInputElement;
    expect(field.value).toBe("z-ai/glm-5");
    fireEvent.input(field, { target: { value: "   " } });
    fireEvent.click(getByLabelText("Save Main agent model id"));
    expect(setModel).not.toHaveBeenCalled();
    expect(getByText("Enter a model id to save.")).toBeTruthy();

    fireEvent.input(field, { target: { value: "  moonshot/kimi-k3  " } });
    fireEvent.click(getByLabelText("Save Main agent model id"));
    expect(setModel).toHaveBeenCalledWith("moonshot/kimi-k3", "medium");
  });

  it("keeps both prompt editors under their exact accessible names", async () => {
    const { getByLabelText } = await mount({
      model: MODEL,
      prompts: PROMPTS,
      providers: ROSTER,
    });
    expect(getByLabelText("Main agent system prompt")).toBeTruthy();
    expect(getByLabelText("Save Main agent system prompt")).toBeTruthy();
    expect(getByLabelText("Reset Main agent system prompt to default")).toBeTruthy();
    expect(getByLabelText("Fork agent prompt")).toBeTruthy();
  });

  it("marks the Sub-agent catalog as Claude's only lane", async () => {
    const { getByText } = await mount({ subagent_models: CATALOG, providers: ROSTER });
    expect(getByText(/Claude is\s+available to Sub-agents only/i)).toBeTruthy();
  });
});
