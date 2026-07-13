import { describe, expect, it } from "vitest";
import { reduce } from "./reducer";
import { initialState } from "./types";
import {
  canvasViews,
  chatViews,
  parsePingPlacement,
  viewsForPing,
} from "./selectors";
import type { ViewInstance } from "../protocol";

function view(id: string, placement: string, spec: Record<string, unknown> = { type: "text", text: id }): ViewInstance {
  return { instance_id: id, placement, spec: spec as ViewInstance["spec"] };
}

describe("views slice — hello_ok seeding", () => {
  it("seeds views from hello_ok.views", () => {
    const state = reduce(initialState(), {
      type: "hello_ok",
      payload: {
        type: "hello_ok",
        latest_msg_id: 0,
        messages: [],
        pings: [],
        views: [view("a", "canvas"), view("b", "chat")],
      },
    });
    expect(state.views.map((v) => v.instance_id)).toEqual(["a", "b"]);
  });

  it("treats an absent views field as an empty active set", () => {
    const state = reduce(initialState(), {
      type: "hello_ok",
      payload: { type: "hello_ok", latest_msg_id: 0, messages: [], pings: [] },
    });
    expect(state.views).toEqual([]);
  });

  it("hello_ok is authoritative — a reconnect replaces the view set (offline clear)", () => {
    const seeded = reduce(initialState(), {
      type: "view_upsert",
      payload: { type: "view_upsert", instance_id: "gone", placement: "canvas", spec: { type: "text", text: "x" } },
    });
    expect(seeded.views).toHaveLength(1);
    const reconnected = reduce(seeded, {
      type: "hello_ok",
      payload: { type: "hello_ok", latest_msg_id: 0, messages: [], pings: [], views: [] },
    });
    expect(reconnected.views).toEqual([]);
  });
});

describe("views slice — upsert / update-in-place / remove", () => {
  it("appends a new view on view_upsert", () => {
    const state = reduce(initialState(), {
      type: "view_upsert",
      payload: { type: "view_upsert", instance_id: "v1", placement: "canvas", spec: { type: "text", text: "one" } },
    });
    expect(state.views).toHaveLength(1);
    expect(state.views[0].spec).toEqual({ type: "text", text: "one" });
  });

  it("updates in place by instance_id (same id → replace, keep position)", () => {
    let state = reduce(initialState(), {
      type: "view_upsert",
      payload: { type: "view_upsert", instance_id: "v1", placement: "canvas", spec: { type: "text", text: "one" } },
    });
    state = reduce(state, {
      type: "view_upsert",
      payload: { type: "view_upsert", instance_id: "v2", placement: "chat", spec: { type: "text", text: "two" } },
    });
    // Re-upsert v1 with new spec — must replace, not append, and hold position.
    state = reduce(state, {
      type: "view_upsert",
      payload: { type: "view_upsert", instance_id: "v1", placement: "canvas", spec: { type: "text", text: "updated" } },
    });
    expect(state.views).toHaveLength(2);
    expect(state.views.map((v) => v.instance_id)).toEqual(["v1", "v2"]);
    expect(state.views[0].spec).toEqual({ type: "text", text: "updated" });
  });

  it("drops exactly the removed id on view_removed", () => {
    let state = reduce(initialState(), {
      type: "view_upsert",
      payload: { type: "view_upsert", instance_id: "v1", placement: "canvas", spec: { type: "text", text: "one" } },
    });
    state = reduce(state, {
      type: "view_upsert",
      payload: { type: "view_upsert", instance_id: "v2", placement: "chat", spec: { type: "text", text: "two" } },
    });
    state = reduce(state, {
      type: "view_removed",
      payload: { type: "view_removed", instance_id: "v1" },
    });
    expect(state.views.map((v) => v.instance_id)).toEqual(["v2"]);
  });

  it("view_removed for an unknown id is a no-op", () => {
    const state = reduce(initialState(), {
      type: "view_removed",
      payload: { type: "view_removed", instance_id: "nope" },
    });
    expect(state.views).toEqual([]);
  });
});

describe("view placement selectors", () => {
  const views = [
    view("c1", "canvas"),
    view("c2", "canvas"),
    view("chat1", "chat"),
    view("p7", "ping:7"),
    view("p12", "ping:12"),
  ];

  it("parsePingPlacement extracts the ping id, else null", () => {
    expect(parsePingPlacement("ping:7")).toBe(7);
    expect(parsePingPlacement("ping:012")).toBe(12);
    expect(parsePingPlacement("canvas")).toBeNull();
    expect(parsePingPlacement("chat")).toBeNull();
    expect(parsePingPlacement("ping:")).toBeNull();
    expect(parsePingPlacement("ping:abc")).toBeNull();
  });

  it("canvasViews returns canvas placements in order (newest last)", () => {
    expect(canvasViews(views).map((v) => v.instance_id)).toEqual(["c1", "c2"]);
  });

  it("chatViews returns chat placements", () => {
    expect(chatViews(views).map((v) => v.instance_id)).toEqual(["chat1"]);
  });

  it("viewsForPing matches only that ping's placement", () => {
    expect(viewsForPing(views, 7).map((v) => v.instance_id)).toEqual(["p7"]);
    expect(viewsForPing(views, 12).map((v) => v.instance_id)).toEqual(["p12"]);
    expect(viewsForPing(views, 99)).toEqual([]);
  });
});
