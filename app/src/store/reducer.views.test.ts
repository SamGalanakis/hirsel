import { describe, expect, it } from "vitest";
import { reduce } from "./reducer";
import { initialState } from "./types";
import {
  canvasViews,
} from "./selectors";
import type { ViewInstance } from "../protocol";

function view(id: string, placement: "canvas", spec: Record<string, unknown> = { type: "text", text: id }): ViewInstance {
  return { thread_id: 1, instance_id: id, placement, spec: spec as ViewInstance["spec"] };
}

describe("views slice — hello_ok seeding", () => {

  it("accepts an explicitly empty views inventory", () => {
    const state = reduce(initialState(), { type: "hello_ok", payload: { type: "hello_ok", history_id: "test-history", threads: [], processes: [], views: [], host_version: "test", model: null, subagent_models: null, prompts: null, providers: null } });
    expect(state.views).toEqual([]);
  });

  it("hello_ok is authoritative — a reconnect replaces the view set (offline clear)", () => {
    const seeded = reduce(initialState(), {
      type: "view_upsert",
      payload: { thread_id: 1, type: "view_upsert", instance_id: "gone", placement: "canvas", spec: { type: "text", text: "x" } },
    });
    expect(seeded.views).toHaveLength(1);
    const reconnected = reduce(seeded, { type: "hello_ok", payload: { type: "hello_ok", views: [], history_id: "test-history", threads: [], processes: [], host_version: "test", model: null, subagent_models: null, prompts: null, providers: null } });
    expect(reconnected.views).toEqual([]);
  });
});

describe("views slice — upsert / update-in-place / remove", () => {
  it("appends a new view on view_upsert", () => {
    const state = reduce(initialState(), {
      type: "view_upsert",
      payload: { thread_id: 1, type: "view_upsert", instance_id: "v1", placement: "canvas", spec: { type: "text", text: "one" } },
    });
    expect(state.views).toHaveLength(1);
    expect(state.views[0].spec).toEqual({ type: "text", text: "one" });
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


  ];

  it("canvasViews returns canvas placements in order (newest last)", () => {
    expect(canvasViews(views, 1).map((v) => v.instance_id)).toEqual(["c1", "c2"]);
  });
});

it("scopes conversation canvases by exact Thread, including ordinary zero", () => {
  const views = [view("a", "canvas"), { ...view("zero", "canvas"), thread_id: 0 }, { ...view("peer", "canvas"), thread_id: 2 }];
  expect(canvasViews(views, 1).map(view => view.instance_id)).toEqual(["a"]);
  expect(canvasViews(views, 0).map(view => view.instance_id)).toEqual(["zero"]);
  expect(canvasViews(views, null)).toEqual([]);
});
