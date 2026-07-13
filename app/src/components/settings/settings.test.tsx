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
