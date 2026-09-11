import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ModelSnapshot, ProviderRoster } from "../../protocol";
import type { SettingsTab } from "../../store/store";

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

const MODEL: ModelSnapshot = {
  current: { id: "gpt-5.6-sol", variant: "medium" },
  available: [
    {
      id: "gpt-5.6-sol",
      label: "GPT-5.6 Sol",
      variants: ["low", "medium", "high"],
      default_variant: "medium",
    },
  ],
  provider_id: "codex",
};

const ROSTER: ProviderRoster = {
  instances: [
    {
      id: "codex",
      kind: "codex",
      label: "Codex",
      detection: { detected: true, path: "/home/owner/.codex/auth.json" },
      agent_selectable: true,
      removable: false,
    },
  ],
  booted_provider_id: "codex",
};

/** The tab list, in the order the rail shows it. */
const TAB_LABELS = [
  "Appearance",
  "Thread models",
  "Providers",
  "Connection & devices",
  "Notifications",
  "Guide",
  "About & debug",
  "Plugins",
];

beforeEach(() => {
  vi.resetModules();
  memStore.clear();
  vi.stubGlobal("localStorage", memLocalStorage);
  vi.doMock("../../ws/client", () => ({
    clearStoredToken: vi.fn(),
    getStoredToken: () => "tok-abcd",
    getClient: () => ({}),
  }));
});

afterEach(() => vi.unstubAllGlobals());

async function mount(tab?: SettingsTab) {
  const store = await import("../../store/store");
  store.dispatch({ type: "hello_ok", payload: { type: "hello_ok", model: MODEL, providers: ROSTER, history_id: "test-history", threads: [], processes: [], views: [], host_version: "test", subagent_models: null, prompts: null } });
  store.openSettings(tab);
  const { SettingsSheet } = await import("./SettingsSheet");
  return render(() => <SettingsSheet />);
}

describe("Settings: side-tab navigation", () => {
  it("offers every section as a tab, and mounts only the active panel", async () => {
    const { getAllByRole, getByRole, queryByLabelText, getByLabelText } = await mount();

    expect(getAllByRole("tab").map((tab) => tab.textContent)).toEqual(TAB_LABELS);
    expect(getByRole("tab", { name: "Appearance" }).getAttribute("aria-selected")).toBe("true");
    // Appearance is up; the Agents panel is not merely hidden, it is unmounted.
    expect(getByLabelText("Theme")).toBeTruthy();
    expect(queryByLabelText("Main agent model")).toBeNull();

    fireEvent.click(getByRole("tab", { name: "Thread models" }));
    expect(getByLabelText("Main agent model")).toBeTruthy();
    expect(queryByLabelText("Theme")).toBeNull();
  });

  it("wires the panel to its tab and keeps the list to one tab stop", async () => {
    const { getAllByRole, getByRole } = await mount();
    const panel = getByRole("tabpanel");
    const active = getByRole("tab", { name: "Appearance" });
    expect(panel.getAttribute("aria-labelledby")).toBe(active.id);
    expect(active.getAttribute("aria-controls")).toBe(panel.id);
    expect(panel.getAttribute("tabindex")).toBe("0");

    // Roving tabindex: exactly one tab is reachable with Tab.
    const reachable = getAllByRole("tab").filter((tab) => tab.getAttribute("tabindex") === "0");
    expect(reachable).toHaveLength(1);
  });

  it("moves the selection with the arrow keys, Home and End", async () => {
    const { getAllByRole, getByRole } = await mount();
    const selected = () =>
      getAllByRole("tab").find((tab) => tab.getAttribute("aria-selected") === "true")?.textContent;

    fireEvent.keyDown(getByRole("tab", { name: "Appearance" }), { key: "ArrowDown" });
    expect(selected()).toBe("Thread models");

    fireEvent.keyDown(getByRole("tab", { name: "Thread models" }), { key: "ArrowRight" });
    expect(selected()).toBe("Providers");

    fireEvent.keyDown(getByRole("tab", { name: "Providers" }), { key: "ArrowLeft" });
    expect(selected()).toBe("Thread models");

    fireEvent.keyDown(getByRole("tab", { name: "Thread models" }), { key: "End" });
    expect(selected()).toBe("Plugins");

    fireEvent.keyDown(getByRole("tab", { name: "Plugins" }), { key: "Home" });
    expect(selected()).toBe("Appearance");

    // Activation follows focus, so each of those moves swapped the panel too.
    expect(getByRole("tabpanel").getAttribute("aria-labelledby")).toBe(
      getByRole("tab", { name: "Appearance" }).id,
    );
  });

  it("lands on the tab the caller asked for", async () => {
    const { getByRole, getByText } = await mount("providers");
    expect(getByRole("tab", { name: "Providers" }).getAttribute("aria-selected")).toBe("true");
    expect(getByText("Codex")).toBeTruthy();
  });
});

describe("Settings: Guide", () => {
  /** The concepts the page promises, in the order it explains them. */
  const HEADINGS = [
    "What hirsel is",
    "Your workspace",
    "Spaces and Tasks",
    "Talking to it",
    "Artifacts and execution",
    "The agents",
    "Keyboard, on a desktop",
    "Where to poke around",
  ];

  it("explains every concept, in order, with no wire calls", async () => {
    const { getAllByRole } = await mount("guide");
    const headings = getAllByRole("heading").map((h) => h.textContent);
    expect(headings).toEqual(HEADINGS);
  });

  it("spells the keyboard shortcuts as key chips", async () => {
    const { container, getByText } = await mount("guide");
    const keys = [...container.querySelectorAll("kbd")].map((k) => k.textContent);
    expect(keys).toContain("⌘/Ctrl");
    expect(keys).toContain("Esc");
    // The `g` chords render as two chips joined by "then".
    expect(keys.filter((k) => k === "g")).toHaveLength(4);
    expect(getByText("Open Settings.")).toBeTruthy();
  });
});

describe("Settings: one entry point", () => {

  it("gives the command palette exactly one Open Settings command", async () => {
    vi.doMock("../../lib/focus", () => ({
      anyOverlayOpen: () => false,
      createOverlayPresence: () => {},
      createFocusTrap: () => {},
      focusMainComposer: () => {},
    }));
    const { CommandPalette } = await import("../CommandPalette");
    render(() => <CommandPalette open onOpenChange={() => {}} />);
    await waitFor(() => expect(screen.getByRole("combobox")).toBeInTheDocument());
    const options = screen.getAllByRole("option");
    expect(options.filter((option) => option.textContent?.includes("Open Settings"))).toHaveLength(
      1,
    );
  });
});
