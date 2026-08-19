// The in-repo example UI (plugins/hello/ui/index.tsx), exercised end to end
// through the real slot registry: it is compiled with the app, so it shares the
// app's Solid instance and its components are reactive inside a `<PluginSlot>`
// with nothing to shim.
import { fireEvent, render, waitFor } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { PluginApi, SlotComponent, SlotName } from "./types";

beforeEach(() => {
  vi.resetModules();
  vi.spyOn(console, "warn").mockImplementation(() => {});
});

function jsonResponse(body: unknown, ok = true, status = 200): Response {
  return {
    ok,
    status,
    json: async () => body,
    text: async () => JSON.stringify(body),
  } as unknown as Response;
}

async function mountHello() {
  const { registerSlot, subscribePush, deliverPluginPush } = await import("./registry");
  const { PluginSlot } = await import("./PluginSlot");
  const factory = (await import("../../../plugins/hello/ui/index")).default;

  const fetchMock = vi.fn(async (_path: string, _init?: RequestInit) =>
    jsonResponse({ text: "Hello, world!", count: 1 }),
  );
  const api: PluginApi = {
    id: "hello",
    label: "Hello Plugin",
    slots: {
      register: (slot: SlotName, component: SlotComponent) =>
        registerSlot("hello", "Hello Plugin", slot, component),
    },
    fetch: fetchMock as PluginApi["fetch"],
    onPush: (topic, handler) => subscribePush("hello", topic, handler),
  };

  factory(api);
  const screen = render(() => <PluginSlot name="home.section" />);
  return { screen, fetchMock, deliverPluginPush };
}

describe("plugins/hello/ui", () => {
  it("registers a home.section card labelled with the plugin label", async () => {
    const { screen } = await mountHello();
    expect(screen.getByText("Hello Plugin")).toBeTruthy();
  });

  it("POSTs the typed name to its own /greet route and shows the reply", async () => {
    const { screen, fetchMock } = await mountHello();

    const input = screen.getByLabelText("Name to greet") as HTMLInputElement;
    fireEvent.input(input, { target: { value: "hirsel" } });
    fireEvent.click(screen.getByText("Greet"));

    await waitFor(() =>
      expect(fetchMock).toHaveBeenCalledWith("/greet", {
        method: "POST",
        body: JSON.stringify({ name: "hirsel" }),
      }),
    );
    await waitFor(() => expect(screen.container.textContent).toContain("Hello, world!"));
  });

  it("surfaces a non-2xx from its route instead of throwing", async () => {
    const { screen, fetchMock } = await mountHello();
    fetchMock.mockResolvedValueOnce(jsonResponse({ error: "nope" }, false, 502));

    fireEvent.click(screen.getByText("Greet"));

    await waitFor(() => expect(screen.container.textContent).toContain("error: HTTP 502"));
  });

  it("updates its counter live from a plugin_push on its 'tick' topic", async () => {
    const { screen, deliverPluginPush } = await mountHello();
    expect(screen.container.textContent).toContain("ticks: 0");

    deliverPluginPush({
      type: "plugin_push",
      plugin: "hello",
      topic: "tick",
      data: { count: 9 },
    });

    expect(screen.container.textContent).toContain("ticks: 9");
  });
});
