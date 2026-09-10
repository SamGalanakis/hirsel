import { describe, expect, it } from "vitest";
import { reduce } from "./reducer";
import { initialState } from "./types";
import {
  canvasViews,
} from "./selectors";
import type { ViewInstance } from "../protocol";

function view(id: string, spec: Record<string, unknown> = { type: "text", text: id }): ViewInstance {
  return { thread_id: 1, instance_id: id, spec: spec as ViewInstance["spec"] };
}

describe("views slice — hello_ok seeding", () => {

  it("accepts an explicitly empty views inventory", () => {
    const state = reduce(initialState(), { type: "hello_ok", payload: { type: "hello_ok", history_id: "test-history", threads: [], processes: [], views: [], host_version: "test", model: null, subagent_models: null, prompts: null, providers: null } });
    expect(state.views).toEqual([]);
  });

  it("hello_ok is authoritative — a reconnect replaces the view set (offline clear)", () => {
    const seeded = reduce(initialState(), {
      type: "view_upsert",
      payload: { thread_id: 1, type: "view_upsert", instance_id: "gone", spec: { type: "text", text: "x" } },
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
      payload: { thread_id: 1, type: "view_upsert", instance_id: "v1", spec: { type: "text", text: "one" } },
    });
    expect(state.views).toHaveLength(1);
    expect(state.views[0].spec).toEqual({ type: "text", text: "one" });
  });

  it("keeps live and reconnect order aligned through update and recreation", () => {
    const upsert = (state: ReturnType<typeof initialState>, id: string, text: string) =>
      reduce(state, {
        type: "view_upsert",
        payload: { thread_id: id === "z" ? 1 : 2, type: "view_upsert", instance_id: id, spec: { type: "text", text } },
      });
    let live = upsert(initialState(), "z", "first");
    live = upsert(live, "a", "second");
    expect(live.views.map((row) => row.instance_id)).toEqual(["z", "a"]);

    live = upsert(live, "z", "updated");
    expect(live.views.map((row) => row.instance_id)).toEqual(["a", "z"]);
    live = reduce(live, { type: "view_removed", payload: { type: "view_removed", instance_id: "a" } });
    live = upsert(live, "a", "recreated");
    expect(live.views.map((row) => row.instance_id)).toEqual(["z", "a"]);

    const reconnected = reduce(initialState(), {
      type: "hello_ok",
      payload: { type: "hello_ok", history_id: "test-history", threads: [], processes: [], views: live.views, host_version: "test", model: null, subagent_models: null, prompts: null, providers: null },
    });
    expect(reconnected.views.map((row) => row.instance_id)).toEqual(["z", "a"]);
  });

  it("view_removed for an unknown id is a no-op", () => {
    const state = reduce(initialState(), {
      type: "view_removed",
      payload: { type: "view_removed", instance_id: "nope" },
    });
    expect(state.views).toEqual([]);
  });
});

describe("Canvas view selectors", () => {
  const views = [
    view("c1"),
    view("c2"),
  ];

  it("returns Canvas views in host order (newest last)", () => {
    expect(canvasViews(views, 1).map((v) => v.instance_id)).toEqual(["c1", "c2"]);
  });
});

it("scopes conversation canvases by exact Thread, including ordinary zero", () => {
  const views = [view("a"), { ...view("zero"), thread_id: 0 }, { ...view("peer"), thread_id: 2 }];
  expect(canvasViews(views, 1).map(view => view.instance_id)).toEqual(["a"]);
  expect(canvasViews(views, 0).map(view => view.instance_id)).toEqual(["zero"]);
  expect(canvasViews(views, null)).toEqual([]);
});
