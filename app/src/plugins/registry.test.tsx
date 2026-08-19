// Slot registry rendering, per-contribution error isolation, and plugin_push
// routing.
import { render } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { beforeEach, describe, expect, it, vi } from "vitest";

beforeEach(() => {
  vi.resetModules();
  vi.spyOn(console, "warn").mockImplementation(() => {});
});

describe("PluginSlot", () => {
  it("renders every contribution for its slot, in registration order", async () => {
    const { registerSlot } = await import("./registry");
    const { PluginSlot } = await import("./PluginSlot");

    registerSlot("a", "Plugin A", "home.section", () => <p>from A</p>);
    registerSlot("b", "Plugin B", "home.section", () => <p>from B</p>);
    // A different slot must not leak into this one.
    registerSlot("c", "Plugin C", "settings.section", () => <p>from C</p>);

    const { container } = render(() => <PluginSlot name="home.section" />);
    expect(container.textContent).toBe("from Afrom B");
  });

  it("passes ctx through — task.panel gets the focused Task id", async () => {
    const { registerSlot } = await import("./registry");
    const { PluginSlot } = await import("./PluginSlot");

    registerSlot("a", "Plugin A", "task.panel", (props) => <p>task {props.ctx.taskId}</p>);

    const { getByText } = render(() => <PluginSlot name="task.panel" ctx={{ taskId: 42 }} />);
    expect(getByText("task 42")).toBeTruthy();
  });

  it("contains a throwing component: neighbours still render", async () => {
    const { registerSlot } = await import("./registry");
    const { PluginSlot } = await import("./PluginSlot");

    registerSlot("boom", "Exploding Plugin", "home.section", () => {
      throw new Error("render exploded");
    });
    registerSlot("ok", "Calm Plugin", "home.section", () => <p>still here</p>);

    const { getByText, container } = render(() => <PluginSlot name="home.section" />);

    expect(getByText("still here")).toBeTruthy();
    // The failure is named and visible, not a blank hole.
    const notice = container.querySelector('[data-slot="plugin-error"]');
    expect(notice?.getAttribute("data-plugin")).toBe("boom");
    expect(notice?.textContent).toContain("Exploding Plugin");
    expect(notice?.textContent).toContain("render exploded");
  });

  it("unregistering removes the contribution from the live slot", async () => {
    const { registerSlot } = await import("./registry");
    const { PluginSlot } = await import("./PluginSlot");

    const off = registerSlot("a", "Plugin A", "home.section", () => <p>from A</p>);
    const { container } = render(() => <PluginSlot name="home.section" />);
    expect(container.textContent).toBe("from A");

    off();
    expect(container.textContent).toBe("");
  });
});

describe("plugin_push routing", () => {
  it("delivers only to the subscribing plugin and topic", async () => {
    const { subscribePush, deliverPluginPush } = await import("./registry");

    const tick = vi.fn();
    const other = vi.fn();
    const otherPlugin = vi.fn();
    subscribePush("github", "tick", tick);
    subscribePush("github", "other", other);
    subscribePush("gitlab", "tick", otherPlugin);

    deliverPluginPush({ type: "plugin_push", plugin: "github", topic: "tick", data: { n: 1 } });

    expect(tick).toHaveBeenCalledExactlyOnceWith({ n: 1 });
    expect(other).not.toHaveBeenCalled();
    expect(otherPlugin).not.toHaveBeenCalled();
  });

  it("drops a frame nobody subscribed to, and survives a throwing handler", async () => {
    const { subscribePush, deliverPluginPush } = await import("./registry");

    const after = vi.fn();
    subscribePush("github", "tick", () => {
      throw new Error("handler exploded");
    });
    subscribePush("github", "tick", after);

    expect(() =>
      deliverPluginPush({ type: "plugin_push", plugin: "nobody", topic: "tick", data: null }),
    ).not.toThrow();
    deliverPluginPush({ type: "plugin_push", plugin: "github", topic: "tick", data: 7 });

    // A thrown handler is logged, and the next one still runs.
    expect(after).toHaveBeenCalledExactlyOnceWith(7);
  });

  it("unsubscribes, and unregisterPlugin drops the whole plugin's subscriptions", async () => {
    const { subscribePush, deliverPluginPush, unregisterPlugin } = await import("./registry");

    const a = vi.fn();
    const b = vi.fn();
    const off = subscribePush("github", "tick", a);
    subscribePush("github", "beat", b);

    off();
    deliverPluginPush({ type: "plugin_push", plugin: "github", topic: "tick", data: 1 });
    expect(a).not.toHaveBeenCalled();

    unregisterPlugin("github");
    deliverPluginPush({ type: "plugin_push", plugin: "github", topic: "beat", data: 1 });
    expect(b).not.toHaveBeenCalled();
  });

  it("a push drives a mounted plugin component's signal", async () => {
    const { registerSlot, subscribePush, deliverPluginPush } = await import("./registry");
    const { PluginSlot } = await import("./PluginSlot");

    const [count, setCount] = createSignal(0);
    subscribePush("github", "tick", (data) => setCount(Number(data)));
    registerSlot("github", "GitHub", "home.section", () => <p>ticks: {count()}</p>);

    const { container } = render(() => <PluginSlot name="home.section" />);
    expect(container.textContent).toBe("ticks: 0");

    deliverPluginPush({ type: "plugin_push", plugin: "github", topic: "tick", data: 3 });
    expect(container.textContent).toBe("ticks: 3");
  });
});
