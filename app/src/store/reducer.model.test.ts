import { describe, expect, it } from "vitest";
import { reduce } from "./reducer";
import { initialState } from "./types";
import type { ModelSnapshot, SubagentModelCatalog } from "../protocol";

const MODEL: ModelSnapshot = {
  current: { id: "gpt-5.6-sol", variant: "medium" },
  available: [
    {
      id: "gpt-5.6-sol",
      label: "GPT-5.6 Sol",
      variants: ["low", "medium", "high", "xhigh", "max"],
      default_variant: "medium",
    },
  ],
};

const CATALOG: SubagentModelCatalog = {
  providers: [
    {
      provider: "codex",
      label: "Codex CLI",
      models: [
        {
          id: "gpt-5.6-terra",
          label: "Terra",
          variants: ["low", "medium", "high"],
          enabled_variants: ["low", "medium", "high"],
          enabled: true,
        },
      ],
    },
    {
      provider: "claude",
      label: "Claude Code CLI",
      models: [
        {
          id: "claude-opus-4-8",
          label: "Claude Opus 4.8",
          variants: ["low", "medium", "high"],
          enabled_variants: ["low", "medium", "high"],
          enabled: true,
        },
        {
          id: "claude-sonnet-5",
          label: "Claude Sonnet 5",
          variants: ["low", "medium", "high"],
          enabled_variants: ["low", "medium", "high"],
          enabled: true,
        },
      ],
    },
  ],
};

function helloOk(extra: Record<string, unknown>) {
  return reduce(initialState(), {
    type: "hello_ok",
    payload: {
      type: "hello_ok",
      latest_msg_id: 0,
      messages: [],
      pings: [],
      ...extra,
    },
  });
}

describe("model config: hello_ok seeding", () => {
  it("seeds both model and subagentModels from hello_ok", () => {
    const state = helloOk({ model: MODEL, subagent_models: CATALOG });
    expect(state.model).toEqual(MODEL);
    expect(state.subagentModels).toEqual(CATALOG);
  });

  it("defaults both to null when the fields are absent (older host)", () => {
    const state = helloOk({});
    expect(state.model).toBeNull();
    expect(state.subagentModels).toBeNull();
  });

  it("re-seeds authoritatively on a resync, dropping a field an older host omits", () => {
    const seeded = helloOk({ model: MODEL, subagent_models: CATALOG });
    const resynced = reduce(seeded, {
      type: "hello_ok",
      payload: { type: "hello_ok", latest_msg_id: 0, messages: [], pings: [] },
    });
    expect(resynced.model).toBeNull();
    expect(resynced.subagentModels).toBeNull();
  });
});

describe("model config: model_changed", () => {
  it("replaces the whole snapshot", () => {
    const seeded = helloOk({ model: MODEL });
    const changed = reduce(seeded, {
      type: "model_changed",
      model: { ...MODEL, current: { id: "gpt-5.6-sol", variant: "high" } },
    });
    expect(changed.model?.current).toEqual({ id: "gpt-5.6-sol", variant: "high" });
    expect(changed.model?.available).toEqual(MODEL.available);
  });

  // The field-observed defect: the host's snapshot follows the STORED provider,
  // so a move to a curated provider must arrive as a whole new control shape —
  // registry, reasoning ladder, provider id — not a patched selection.
  it("reshapes free-text into a curated registry when the provider moves", () => {
    const seeded = helloOk({
      model: {
        current: { id: "google/gemini-3.7-flash", variant: "default" },
        available: [],
        provider_id: "openrouter",
        free_text_model: true,
      },
    });
    const changed = reduce(seeded, {
      type: "model_changed",
      model: { ...MODEL, provider_id: "codex", free_text_model: false },
    });
    expect(changed.model?.free_text_model).toBe(false);
    expect(changed.model?.provider_id).toBe("codex");
    expect(changed.model?.available).toEqual(MODEL.available);
  });

  it("seeds the snapshot even when hello_ok carried none", () => {
    const base = helloOk({});
    const changed = reduce(base, { type: "model_changed", model: MODEL });
    expect(changed.model).toEqual(MODEL);
  });
});

describe("model config: subagent_models_changed", () => {
  it("replaces the catalog wholesale", () => {
    const seeded = helloOk({ subagent_models: CATALOG });
    const next: SubagentModelCatalog = {
      providers: [
        {
          provider: "codex",
          label: "Codex CLI",
          models: [
            {
              id: "gpt-5.6-terra",
              label: "Terra",
              variants: ["low", "medium", "high"],
              enabled_variants: ["low", "high"],
              enabled: false,
            },
          ],
        },
      ],
    };
    const changed = reduce(seeded, { type: "subagent_models_changed", catalog: next });
    expect(changed.subagentModels).toEqual(next);
  });

  it("seeds the catalog even when none was present before", () => {
    const base = helloOk({});
    const changed = reduce(base, { type: "subagent_models_changed", catalog: CATALOG });
    expect(changed.subagentModels).toEqual(CATALOG);
  });
});

describe("model config: defensiveness", () => {
  it("does not throw on a hello_ok that omits the model fields", () => {
    // Absent model/subagent_models must be tolerated (older host), defaulting
    // to null rather than throwing so the app never white-screens.
    expect(() => helloOk({})).not.toThrow();
  });

  it("does not throw on a model_changed with no prior snapshot", () => {
    expect(() =>
      reduce(initialState(), { type: "model_changed", model: MODEL }),
    ).not.toThrow();
  });

  it("does not throw on a subagent_models_changed with no prior catalog", () => {
    expect(() =>
      reduce(initialState(), { type: "subagent_models_changed", catalog: { providers: [] } }),
    ).not.toThrow();
  });
});
