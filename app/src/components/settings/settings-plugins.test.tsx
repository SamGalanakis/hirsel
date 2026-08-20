// Settings → Plugins, against a mocked fetch: the roster, the enable toggle,
// and the declared-settings save (including the secret write rule).
import { fireEvent, render, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { PluginInfo } from "../../plugins/types";

// SettingsSheet reads the global `localStorage` (mirrors settings.test.tsx).
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

const GITHUB: PluginInfo = {
  id: "github",
  label: "GitHub Notifier",
  version: "0.3.1",
  state: "running",
  settings: [
    { key: "org", label: "Organisation", kind: "string" },
    { key: "issues", label: "Watch issues", kind: "boolean", default: true },
    { key: "token", label: "API token", kind: "secret" },
  ],
  values: { org: "hirsel", issues: true, token: "<set>" },
};

const BROKEN: PluginInfo = {
  id: "broken",
  label: "Broken Plugin",
  version: "1.0.0",
  state: "errored",
  error: "daemon crash-looped: exit status 1",
  settings: [],
  values: {},
};

/** Records every request the section makes, and replies from `plugins`. */
function stubFetch(plugins: PluginInfo[]) {
  const calls: { url: string; method: string; body: unknown }[] = [];
  const fetchMock = vi.fn(async (url: string, init?: RequestInit) => {
    calls.push({
      url,
      method: init?.method ?? "GET",
      body: init?.body ? JSON.parse(init.body as string) : undefined,
    });
    return {
      ok: true,
      status: 200,
      json: async () => ({ plugins }),
      text: async () => "",
    } as unknown as Response;
  });
  vi.stubGlobal("fetch", fetchMock);
  return calls;
}

async function openPlugins() {
  const store = await import("../../store/store");
  store.openSettings("plugins");
  const { SettingsSheet } = await import("./SettingsSheet");
  return render(() => <SettingsSheet />);
}

beforeEach(() => {
  vi.resetModules();
  memStore.clear();
  memStore.set("hirsel.token", "tok-abcd");
  vi.stubGlobal("localStorage", memLocalStorage);
});

afterEach(() => vi.unstubAllGlobals());

describe("Settings → Plugins", () => {
  it("lists each plugin with its version, state badge, and error text", async () => {
    stubFetch([GITHUB, BROKEN]);
    const { getByText, findByText } = await openPlugins();

    expect(await findByText("GitHub Notifier")).toBeTruthy();
    expect(getByText("0.3.1")).toBeTruthy();
    expect(getByText("Running")).toBeTruthy();
    expect(getByText("Broken Plugin")).toBeTruthy();
    expect(getByText("Error")).toBeTruthy();
    expect(getByText("daemon crash-looped: exit status 1")).toBeTruthy();
  });

  it("renders no roster when the Host reports no plugins", async () => {
    stubFetch([]);
    const { container, queryByText } = await openPlugins();
    await waitFor(() =>
      expect(container.querySelectorAll("[data-slot='plugin-row']")).toHaveLength(0),
    );
    expect(queryByText("GitHub Notifier")).toBeNull();
  });

  it("posts the new enabled state and re-reads the roster", async () => {
    const calls = stubFetch([GITHUB]);
    const { findByLabelText } = await openPlugins();

    fireEvent.click(await findByLabelText("Enable GitHub Notifier"));

    await waitFor(() =>
      expect(calls.some((c) => c.url.endsWith("/api/plugins/github/enabled"))).toBe(true),
    );
    const post = calls.find((c) => c.url.endsWith("/api/plugins/github/enabled"));
    expect(post?.method).toBe("POST");
    // The toggle showed "on" (state running), so the tap asks for off.
    expect(post?.body).toEqual({ enabled: false });
    // The Host owns the resulting state: the section re-reads rather than guessing.
    await waitFor(() =>
      expect(calls.filter((c) => c.url.endsWith("/api/plugins")).length).toBeGreaterThan(1),
    );
  });

  it("sends the bearer token on every plugin request", async () => {
    stubFetch([GITHUB]);
    await openPlugins();
    const fetchMock = globalThis.fetch as unknown as ReturnType<typeof vi.fn>;
    await waitFor(() => expect(fetchMock).toHaveBeenCalled());
    const headers = fetchMock.mock.calls[0][1].headers as Headers;
    expect(headers.get("Authorization")).toBe("Bearer tok-abcd");
  });

  it("saves string and boolean fields, and omits a secret the Owner did not retype", async () => {
    const calls = stubFetch([GITHUB]);
    const { findByLabelText, getByLabelText } = await openPlugins();

    const org = (await findByLabelText("GitHub Notifier: Organisation")) as HTMLInputElement;
    expect(org.value).toBe("hirsel");
    fireEvent.input(org, { target: { value: "acme" } });
    fireEvent.click(getByLabelText("Save GitHub Notifier settings"));

    await waitFor(() =>
      expect(calls.some((c) => c.url.endsWith("/api/plugins/github/settings"))).toBe(true),
    );
    const save = calls.find((c) => c.url.endsWith("/api/plugins/github/settings"));
    // The stored secret is never echoed back as its sentinel.
    expect(save?.body).toEqual({ values: { org: "acme", issues: true } });
  });

  it("renders a stored secret as an empty password field, and writes a retyped one", async () => {
    const calls = stubFetch([GITHUB]);
    const { findByLabelText, getByLabelText } = await openPlugins();

    const secret = (await findByLabelText("GitHub Notifier: API token")) as HTMLInputElement;
    expect(secret.type).toBe("password");
    expect(secret.value).toBe("");
    expect(secret.placeholder).toBe("Stored — type to replace");

    fireEvent.input(secret, { target: { value: "ghp_new" } });
    fireEvent.click(getByLabelText("Save GitHub Notifier settings"));

    await waitFor(() =>
      expect(calls.some((c) => c.url.endsWith("/api/plugins/github/settings"))).toBe(true),
    );
    const save = calls.find((c) => c.url.endsWith("/api/plugins/github/settings"));
    expect(save?.body).toEqual({
      values: { org: "hirsel", issues: true, token: "ghp_new" },
    });
  });
});
