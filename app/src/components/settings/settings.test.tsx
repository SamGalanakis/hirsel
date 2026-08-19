import { fireEvent, render, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// SettingsSheet reads/writes the unqualified global `localStorage` (device
// label, debug flag), which in this runner is Node's unusable experimental Web
// Storage; back it with a plain in-memory Storage so persistence actually lands.
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

beforeEach(() => {
  vi.resetModules();
  memStore.clear();
  vi.stubGlobal("localStorage", memLocalStorage);
});

afterEach(() => vi.unstubAllGlobals());

describe("Settings: device label persistence", () => {
  it("saves a trimmed device label to localStorage", async () => {
    const store = await import("../../store/store");
    store.openSettings();
    const { SettingsSheet } = await import("./SettingsSheet");
    const { getByLabelText, getByText } = render(() => <SettingsSheet />);

    const input = getByLabelText("Device label") as HTMLInputElement;
    fireEvent.input(input, { target: { value: "  Studio laptop  " } });
    fireEvent.click(getByText("Save"));

    expect(memLocalStorage.getItem("hirsel.deviceLabel")).toBe("Studio laptop");
    // The saved label then names "this device" row.
    expect(getByText("Studio laptop")).toBeTruthy();
  });
});

describe("Settings: About & debug", () => {
  it("shows the host version reported over hello_ok", async () => {
    const store = await import("../../store/store");
    store.dispatch({
      type: "hello_ok",
      payload: {
        type: "hello_ok",
        session_id: "s1",
        latest_msg_id: 0,
        messages: [],
        host_version: "0.9.9-test",
      },
    } as never);
    store.openSettings();
    const { SettingsSheet } = await import("./SettingsSheet");
    const { getByText } = render(() => <SettingsSheet />);
    expect(getByText("0.9.9-test")).toBeTruthy();
  });

  it("persists the local 'Show agent code' toggle", async () => {
    const store = await import("../../store/store");
    store.openSettings();
    const { SettingsSheet } = await import("./SettingsSheet");
    const { getByLabelText } = render(() => <SettingsSheet />);

    fireEvent.click(getByLabelText("Show agent code"));
    expect(memLocalStorage.getItem("hirsel.showAgentCode")).toBe("1");
  });
});

describe("Settings: DESIGN conformance of the pane chrome", () => {
  it("uses sentence-case headings on quiet frameless groups, and one close", async () => {
    const store = await import("../../store/store");
    store.openSettings();
    const { SettingsSheet } = await import("./SettingsSheet");
    const { getAllByRole, getAllByLabelText, container } = render(() => <SettingsSheet />);

    // DESIGN §3 rules out all-caps section headings; they carry rank by weight
    // and the whitespace above them instead.
    const headings = getAllByRole("heading", { level: 2 });
    expect(headings.length).toBeGreaterThan(1);
    for (const h of headings) {
      expect(h.className).not.toContain("uppercase");
      expect(h.textContent).not.toBe(h.textContent?.toUpperCase());
    }

    // DESIGN §6 forbids repeated rectangular frames: no section is a bordered,
    // rounded, filled card any more. (The Forget-token alertdialog is a genuine
    // overlay and is not rendered here.)
    expect(container.querySelectorAll(".rounded-xl.border.bg-card")).toHaveLength(0);

    // Exactly one exit, and it is a close.
    expect(getAllByLabelText("Close Settings")).toHaveLength(1);
  });

  it("gives the theme segments and the debug switch thumb-grade hit areas", async () => {
    const store = await import("../../store/store");
    store.openSettings();
    const { SettingsSheet } = await import("./SettingsSheet");
    const { getByLabelText, getAllByRole } = render(() => <SettingsSheet />);

    const coarse = "[@media(pointer:coarse)]:min-h-11";
    for (const segment of getAllByRole("radio")) {
      expect(segment.className).toContain(coarse);
    }
    // The switch's 20x36px track stays the visual; the button around it grows.
    const toggle = getByLabelText("Show agent code");
    expect(toggle.className).toContain(coarse);
    expect(toggle.className).toContain("[@media(pointer:coarse)]:min-w-11");
    expect(toggle.querySelector(".h-5.w-9")).not.toBeNull();
  });
});

describe("Settings: Forget token confirm flow (C5)", () => {
  it("requires confirmation, then clears the stored token and reloads to the gate", async () => {
    const clearStoredToken = vi.fn();
    vi.doMock("../../ws/client", () => ({
      clearStoredToken,
      getStoredToken: () => "tok-abcd",
    }));
    const reload = vi.fn();
    vi.stubGlobal("location", { reload });

    const store = await import("../../store/store");
    store.openSettings();
    const { SettingsSheet } = await import("./SettingsSheet");
    const { getByText, getByRole, queryByRole } = render(() => <SettingsSheet />);

    // No destructive action until the confirm dialog is opened and confirmed.
    expect(queryByRole("alertdialog", { name: "Forget token" })).toBeNull();
    fireEvent.click(getByText("Forget"));

    const dialog = getByRole("alertdialog", { name: "Forget token" });
    expect(clearStoredToken).not.toHaveBeenCalled();

    fireEvent.click(within(dialog).getByText("Forget token"));
    expect(clearStoredToken).toHaveBeenCalledOnce();
    expect(reload).toHaveBeenCalledOnce();
  });

  it("Cancel dismisses the confirm without touching the token", async () => {
    const clearStoredToken = vi.fn();
    vi.doMock("../../ws/client", () => ({
      clearStoredToken,
      getStoredToken: () => "tok-abcd",
    }));

    const store = await import("../../store/store");
    store.openSettings();
    const { SettingsSheet } = await import("./SettingsSheet");
    const { getByText, getByRole, queryByRole } = render(() => <SettingsSheet />);

    fireEvent.click(getByText("Forget"));
    const dialog = getByRole("alertdialog", { name: "Forget token" });
    fireEvent.click(within(dialog).getByText("Cancel"));

    expect(queryByRole("alertdialog", { name: "Forget token" })).toBeNull();
    expect(clearStoredToken).not.toHaveBeenCalled();
  });
});
