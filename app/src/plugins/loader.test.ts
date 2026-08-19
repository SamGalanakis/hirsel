// Loader contract: the Host roster gates which compiled-in UI modules run, and
// one bad plugin costs only itself.
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { PluginApi, PluginInfo } from "./types";

function info(id: string, state: PluginInfo["state"] = "running"): PluginInfo {
  return {
    id,
    label: `${id} plugin`,
    version: "1.0.0",
    state,
    settings: [],
    values: {},
  };
}

/** A UI module that mounts one home.section card. */
function mounts(): () => Promise<unknown> {
  return async () => ({
    default: (api: PluginApi) => {
      api.slots.register("home.section", () => null);
    },
  });
}

beforeEach(() => {
  vi.resetModules();
  vi.spyOn(console, "warn").mockImplementation(() => {});
});

describe("plugin loader: roster gating", () => {
  it("loads the UI of plugins the Host reports running", async () => {
    const { loadPlugins } = await import("./loader");
    const { slotEntries } = await import("./registry");

    await loadPlugins({
      list: async () => [info("alpha"), info("beta")],
      modules: { alpha: mounts(), beta: mounts() },
    });

    expect(slotEntries("home.section").map((e) => e.pluginId)).toEqual(["alpha", "beta"]);
  });

  it("does not initialise a compiled-in module for a disabled plugin", async () => {
    const { loadPlugins } = await import("./loader");
    const { slotEntries } = await import("./registry");
    const off = vi.fn(mounts());

    await loadPlugins({
      list: async () => [info("alpha", "disabled")],
      modules: { alpha: off },
    });

    // Its folder is in the build; the Owner switched it off. It must stay inert.
    expect(off).not.toHaveBeenCalled();
    expect(slotEntries("home.section")).toHaveLength(0);
  });

  it("keeps the UI of an errored (crash-looping) plugin mounted", async () => {
    const { loadPlugins } = await import("./loader");
    const { slotEntries } = await import("./registry");

    await loadPlugins({
      list: async () => [info("alpha", "errored")],
      modules: { alpha: mounts() },
    });

    expect(slotEntries("home.section").map((e) => e.pluginId)).toEqual(["alpha"]);
  });

  it("ignores a rostered plugin that ships no UI module", async () => {
    const { loadPlugins } = await import("./loader");
    const { slotEntries, loadFailures } = await import("./registry");

    await loadPlugins({ list: async () => [info("headless")], modules: {} });

    expect(slotEntries("home.section")).toHaveLength(0);
    expect(loadFailures()).toEqual([]);
  });

  it("loads nothing when the roster is unavailable", async () => {
    const { loadPlugins } = await import("./loader");
    const { slotEntries } = await import("./registry");
    const importer = vi.fn(mounts());

    await loadPlugins({
      list: async () => {
        throw new Error("connection refused");
      },
      modules: { alpha: importer },
    });

    expect(importer).not.toHaveBeenCalled();
    expect(slotEntries("home.section")).toHaveLength(0);
  });
});

describe("plugin loader: failure isolation", () => {
  it("keeps loading after a module that fails to import", async () => {
    const { loadPlugins } = await import("./loader");
    const { slotEntries, loadFailures } = await import("./registry");

    await loadPlugins({
      list: async () => [info("bad"), info("good")],
      modules: {
        bad: async () => {
          throw new Error("chunk load failed");
        },
        good: mounts(),
      },
    });

    expect(slotEntries("home.section").map((e) => e.pluginId)).toEqual(["good"]);
    expect(loadFailures()).toEqual([
      { id: "bad", label: "bad plugin", detail: "chunk load failed" },
    ]);
  });

  it("rejects a module whose default export is not a function", async () => {
    const { loadPlugins } = await import("./loader");
    const { slotEntries, loadFailures } = await import("./registry");

    await loadPlugins({
      list: async () => [info("bad")],
      modules: { bad: async () => ({ default: { not: "a function" } }) },
    });

    expect(slotEntries("home.section")).toHaveLength(0);
    expect(loadFailures()[0].detail).toBe("UI module has no default export function");
  });

  it("unwinds registrations a throwing factory made before it threw", async () => {
    const { loadPlugins } = await import("./loader");
    const { slotEntries, loadFailures } = await import("./registry");

    await loadPlugins({
      list: async () => [info("bad")],
      modules: {
        bad: async () => ({
          default: (api: PluginApi) => {
            api.slots.register("home.section", () => null);
            throw new Error("boom");
          },
        }),
      },
    });

    // A half-initialised plugin must leave nothing mounted.
    expect(slotEntries("home.section")).toHaveLength(0);
    expect(loadFailures()[0].detail).toBe("boom");
  });

  it("startPlugins loads once per page load, however often it is called", async () => {
    const { startPlugins } = await import("./loader");
    const list = vi.fn(async () => []);

    startPlugins({ list, modules: {} });
    startPlugins({ list, modules: {} });
    await Promise.resolve();
    await Promise.resolve();

    expect(list).toHaveBeenCalledTimes(1);
  });
});

describe("plugin discovery", () => {
  it("takes the plugin id from its folder name", async () => {
    const { pluginIdFromPath } = await import("./loader");
    expect(pluginIdFromPath("../../../plugins/hello/ui/index.tsx")).toBe("hello");
    expect(pluginIdFromPath("../../../plugins/github-notifier/ui/index.tsx")).toBe(
      "github-notifier",
    );
    expect(pluginIdFromPath("../../../plugins/hello/ui/helpers.tsx")).toBeNull();
  });

  it("discovers the in-repo hello UI module", async () => {
    const { discoveredModules } = await import("./loader");
    // The glob is a build-time fact: the repo's plugins/hello/ui/index.tsx is
    // compiled into the app, so it must show up as a candidate.
    expect(Object.keys(discoveredModules())).toContain("hello");
  });
});
