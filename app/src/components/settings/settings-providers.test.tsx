import { fireEvent, render, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ProviderRoster, SubagentModelCatalog } from "../../protocol";
import { PENDING_MS } from "../../lib/pending";

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

/** A key the host would never send: the client is never given key material, so
 * this string must not appear anywhere in the rendered DOM. */
const FULL_KEY = "sk-or-v1-000102030405060708090a0b0c0d0e0f9f2c";

const ROSTER: ProviderRoster = {
  instances: [
    {
      id: "codex",
      kind: "codex",
      label: "Codex",
      detection: {
        detected: true,
        path: "/home/owner/.codex/auth.json",
        account_hint: "owner@example.com",
      },
      agent_selectable: true,
      removable: false,
    },
    {
      id: "claude",
      kind: "claude",
      label: "Claude",
      detection: {
        detected: false,
        path: "/home/owner/.claude/.credentials.json",
        detail: "no credentials file",
      },
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

const addProvider = vi.fn();
const updateProvider = vi.fn();
const removeProvider = vi.fn();
const redetectProvider = vi.fn();

beforeEach(() => {
  vi.resetModules();
  memStore.clear();
  addProvider.mockReset();
  updateProvider.mockReset();
  removeProvider.mockReset();
  redetectProvider.mockReset();
  vi.stubGlobal("localStorage", memLocalStorage);
  vi.doMock("../../ws/client", () => ({
    clearStoredToken: vi.fn(),
    getStoredToken: () => "tok-abcd",
    getClient: () => ({ addProvider, updateProvider, removeProvider, redetectProvider }),
  }));
});

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

/** A catalog whose native worker routes through the roster's OpenRouter
 * instance — the only part of the catalog this tab reads. */
const CATALOG: SubagentModelCatalog = {
  providers: [],
  native_worker: {
    label: "Native worker",
    enabled: true,
    provider_id: "openrouter",
    eligible_provider_ids: ["openrouter"],
    model: "deepseek/deepseek-v4.1-flash",
    default_model: "deepseek/deepseek-v4.1-flash",
    model_override: null,
    unavailable_reason: null,
  },
};

async function mount(
  roster: ProviderRoster | null = ROSTER,
  catalog: SubagentModelCatalog | null = null,
) {
  const store = await import("../../store/store");
  store.dispatch({ type: "hello_ok", payload: { type: "hello_ok", providers: roster, history_id: "test-history", threads: [], processes: [], views: [], host_version: "test", model: null, subagent_models: catalog, prompts: null } });
  store.openSettings("providers");
  const { SettingsSheet } = await import("./SettingsSheet");
  return { store, ...render(() => <SettingsSheet />) };
}

describe("Settings → Providers: the roster", () => {
  it("reports a stored key by presence and tail, and never holds the key itself", async () => {
    const { getByText, getByLabelText, container } = await mount();
    expect(getByText(/Key set · ends 9f2c/)).toBeTruthy();
    expect(container.innerHTML).not.toContain(FULL_KEY);

    // The editor's key field starts empty on every open: the client has nothing
    // to prefill it with.
    fireEvent.click(getByLabelText("Edit OpenRouter"));
    const field = getByLabelText("API key") as HTMLInputElement;
    expect(field.type).toBe("password");
    expect(field.getAttribute("autocomplete")).toBe("off");
    expect(field.value).toBe("");
    expect(container.innerHTML).not.toContain(FULL_KEY);
  });

  it("shows a boot notice as one quiet standing line, and nothing when there is none", async () => {
    const { queryByText, unmount } = await mount();
    expect(queryByText(/is unavailable at boot/)).toBeNull();
    unmount();

    const notice =
      'configured provider "acme" is unavailable at boot: no API key is stored — running on Codex';
    const { getByText } = await mount({ ...ROSTER, boot_notice: notice });
    const line = getByText(notice);
    // Degraded but running speaks in the muted voice, not an alert colour.
    expect(line.className).toContain("text-muted-foreground");
  });

  it("omits an untouched key from the patch: unchanged, not cleared", async () => {
    const { getByLabelText, getByText } = await mount();
    fireEvent.click(getByLabelText("Edit OpenRouter"));
    fireEvent.input(getByLabelText("Label"), { target: { value: "OpenRouter EU" } });
    fireEvent.click(getByText("Save"));
    expect(updateProvider).toHaveBeenCalledWith("openrouter", {
      label: "OpenRouter EU",
      base_url: "https://openrouter.ai/api/v1",
      default_model: "z-ai/glm-5",
    });
  });

  it("clears a stored key only when asked to, explicitly", async () => {
    const { getByLabelText } = await mount();
    fireEvent.click(getByLabelText("Edit OpenRouter"));
    fireEvent.click(getByLabelText("Clear OpenRouter key"));
    expect(updateProvider).toHaveBeenCalledWith("openrouter", { api_key: "" });
  });

  it("sends a retyped key and refuses an empty required field", async () => {
    const { getByLabelText, getByText } = await mount();
    fireEvent.click(getByLabelText("Edit OpenRouter"));
    fireEvent.input(getByLabelText("Base URL"), { target: { value: "  " } });
    fireEvent.click(getByText("Save"));
    expect(updateProvider).not.toHaveBeenCalled();
    expect(getByText("Label, base URL and default model are required.")).toBeTruthy();

    fireEvent.input(getByLabelText("Base URL"), { target: { value: "https://eu.example/v1" } });
    fireEvent.input(getByLabelText("API key"), { target: { value: FULL_KEY } });
    fireEvent.click(getByText("Save"));
    expect(updateProvider).toHaveBeenCalledWith("openrouter", {
      label: "OpenRouter",
      base_url: "https://eu.example/v1",
      default_model: "z-ai/glm-5",
      api_key: FULL_KEY,
    });
  });

  it("confirms a removal before sending it", async () => {
    const { getByLabelText, getByRole, queryByRole } = await mount();
    fireEvent.click(getByLabelText("Remove OpenRouter"));
    const dialog = getByRole("alertdialog", { name: "Remove provider" });
    expect(removeProvider).not.toHaveBeenCalled();

    fireEvent.click(within(dialog).getByText("Remove provider"));
    expect(removeProvider).toHaveBeenCalledWith("openrouter");
    expect(queryByRole("alertdialog", { name: "Remove provider" })).toBeNull();
  });

  it("refuses a reserved or malformed id before sending an add", async () => {
    const { getByText, getByLabelText, getByRole } = await mount();
    fireEvent.click(getByRole("button", { name: "Add provider" }));

    const fill = () => {
      fireEvent.input(getByLabelText("Provider label"), { target: { value: "Local" } });
      fireEvent.input(getByLabelText("Provider base URL"), {
        target: { value: "http://localhost:8080/v1" },
      });
      fireEvent.input(getByLabelText("Provider default model"), {
        target: { value: "qwen3-max" },
      });
    };

    fireEvent.input(getByLabelText("Provider id"), { target: { value: "claude" } });
    fill();
    fireEvent.click(getByRole("button", { name: "Add provider" }));
    expect(addProvider).not.toHaveBeenCalled();
    expect(getByText("codex and claude are built in — choose another id.")).toBeTruthy();

    fireEvent.input(getByLabelText("Provider id"), { target: { value: "Local Llama" } });
    fireEvent.click(getByRole("button", { name: "Add provider" }));
    expect(addProvider).not.toHaveBeenCalled();
    expect(
      getByText("An id is lower-case letters, digits, - or _, up to 32 characters."),
    ).toBeTruthy();

    fireEvent.input(getByLabelText("Provider id"), { target: { value: "local-llama" } });
    fireEvent.click(getByRole("button", { name: "Add provider" }));
    expect(addProvider).toHaveBeenCalledWith({
      id: "local-llama",
      label: "Local",
      base_url: "http://localhost:8080/v1",
      api_key: "",
      default_model: "qwen3-max",
    });
  });

  it("closes the open editor when the authoritative roster lands", async () => {
    const { store, getByLabelText, queryByLabelText, getByText } = await mount();
    fireEvent.click(getByLabelText("Edit OpenRouter"));
    fireEvent.click(getByText("Save"));
    store.dispatch({ type: "providers_changed", roster: ROSTER });
    await waitFor(() => expect(queryByLabelText("API key")).toBeNull());
  });

  it("keeps a rejected edit draft through a later unrelated roster refresh", async () => {
    const { store, getByLabelText, getByText } = await mount();
    fireEvent.click(getByLabelText("Edit OpenRouter"));
    fireEvent.input(getByLabelText("Label"), { target: { value: "Rejected edit" } });
    fireEvent.click(getByText("Save"));

    store.setProtocolError("provider rejected");
    await waitFor(() => expect(getByLabelText("Label")).not.toBeDisabled());
    fireEvent.input(getByLabelText("Label"), { target: { value: "Continued edit draft" } });

    store.dispatch({ type: "providers_changed", roster: ROSTER });
    await waitFor(() => expect(getByLabelText("Label")).toHaveValue("Continued edit draft"));
  });

  it("keeps a rejected Add draft through a later unrelated roster refresh", async () => {
    const { store, getByLabelText, getByRole } = await mount();
    fireEvent.click(getByRole("button", { name: "Add provider" }));
    fireEvent.input(getByLabelText("Provider id"), { target: { value: "local-llama" } });
    fireEvent.input(getByLabelText("Provider label"), { target: { value: "Local" } });
    fireEvent.input(getByLabelText("Provider base URL"), {
      target: { value: "http://localhost:8080/v1" },
    });
    fireEvent.input(getByLabelText("Provider default model"), {
      target: { value: "qwen3-max" },
    });
    fireEvent.click(getByRole("button", { name: "Add provider" }));

    store.setProtocolError("provider rejected");
    await waitFor(() => expect(getByLabelText("Provider label")).not.toBeDisabled());
    fireEvent.input(getByLabelText("Provider label"), { target: { value: "Continued local" } });

    store.dispatch({ type: "providers_changed", roster: ROSTER });
    await waitFor(() => expect(getByLabelText("Provider label")).toHaveValue("Continued local"));
  });

  it("keeps a timed-out edit draft through a later unrelated roster refresh", async () => {
    vi.useFakeTimers();
    const { store, getByLabelText, getByText } = await mount();
    fireEvent.click(getByLabelText("Edit OpenRouter"));
    fireEvent.input(getByLabelText("Label"), { target: { value: "Timed-out edit" } });
    fireEvent.click(getByText("Save"));

    vi.advanceTimersByTime(PENDING_MS);
    await waitFor(() => expect(getByLabelText("Label")).not.toBeDisabled());
    fireEvent.input(getByLabelText("Label"), { target: { value: "Continued timed-out edit" } });

    store.dispatch({ type: "providers_changed", roster: ROSTER });
    await waitFor(() =>
      expect(getByLabelText("Label")).toHaveValue("Continued timed-out edit"),
    );
  });

  it("keeps a timed-out Add draft through a later unrelated roster refresh", async () => {
    vi.useFakeTimers();
    const { store, getByLabelText, getByRole } = await mount();
    fireEvent.click(getByRole("button", { name: "Add provider" }));
    fireEvent.input(getByLabelText("Provider id"), { target: { value: "local-llama" } });
    fireEvent.input(getByLabelText("Provider label"), { target: { value: "Local" } });
    fireEvent.input(getByLabelText("Provider base URL"), {
      target: { value: "http://localhost:8080/v1" },
    });
    fireEvent.input(getByLabelText("Provider default model"), {
      target: { value: "qwen3-max" },
    });
    fireEvent.click(getByRole("button", { name: "Add provider" }));

    vi.advanceTimersByTime(PENDING_MS);
    await waitFor(() => expect(getByLabelText("Provider label")).not.toBeDisabled());
    fireEvent.input(getByLabelText("Provider label"), { target: { value: "Continued local" } });

    store.dispatch({ type: "providers_changed", roster: ROSTER });
    await waitFor(() => expect(getByLabelText("Provider label")).toHaveValue("Continued local"));
  });

  it("preserves an unsent edit draft on a passive roster refresh", async () => {
    const { store, getByLabelText } = await mount();
    fireEvent.click(getByLabelText("Edit OpenRouter"));
    fireEvent.input(getByLabelText("Label"), { target: { value: "Unsent local draft" } });

    store.dispatch({ type: "providers_changed", roster: ROSTER });
    await waitFor(() => expect(getByLabelText("Label")).toHaveValue("Unsent local draft"));
  });

  it("reads the OAuth providers' detection, and offers only a re-probe", async () => {
    const { getByText, getByLabelText, queryByLabelText } = await mount();
    expect(getByText("Detected · owner@example.com")).toBeTruthy();
    expect(getByText("/home/owner/.codex/auth.json")).toBeTruthy();
    expect(getByText("Not detected")).toBeTruthy();
    expect(getByText("no credentials file")).toBeTruthy();
    expect(
      getByText("Log in with the claude CLI on the host machine, then check again."),
    ).toBeTruthy();
    // Nothing here pretends to authenticate, and nothing here is editable.
    expect(queryByLabelText("Edit Codex")).toBeNull();
    expect(queryByLabelText("Remove Codex")).toBeNull();

    fireEvent.click(getByLabelText("Check Codex again"));
    expect(redetectProvider).toHaveBeenCalledWith("codex");
  });

  it("says Claude is a Sub-agent lane, and keeps it out of the agent selects", async () => {
    const { getByText } = await mount();
    expect(
      getByText("Available to Sub-agents only — it cannot run the main Agent or the fork."),
    ).toBeTruthy();
    const claude = ROSTER.instances.find((instance) => instance.id === "claude");
    expect(claude?.agent_selectable).toBe(false);
  });

  it("says so plainly when the host reports no roster", async () => {
    const { getByText, queryByText } = await mount(null);
    expect(getByText("This host reports no provider roster.")).toBeTruthy();
    expect(queryByText("Add provider")).toBeNull();
  });
});

describe("Settings → Providers: the native worker marker", () => {
  it("marks the instance the native worker routes through, and only that one", async () => {
    const { getAllByText, getByText } = await mount(ROSTER, CATALOG);
    expect(getAllByText("Native worker")).toHaveLength(1);
    // The marker sits beside the instance's own label, not on a built-in login.
    const row = getByText("OpenRouter").parentElement!;
    expect(within(row).getByText("Native worker")).toBeTruthy();
  });

  it("shows no marker when the worker is off or the host reports no catalog", async () => {
    const { queryByText, unmount } = await mount(ROSTER, {
      ...CATALOG,
      native_worker: { ...CATALOG.native_worker, enabled: false },
    });
    expect(queryByText("Native worker")).toBeNull();
    unmount();

    const bare = await mount(ROSTER, null);
    expect(bare.queryByText("Native worker")).toBeNull();
  });
});
