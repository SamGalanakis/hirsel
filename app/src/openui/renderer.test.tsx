import { render, screen } from "@solidjs/testing-library";
import { describe, expect, it } from "vitest";
import { Renderer, parseOpenUi } from "./Renderer";
import { actionMessageBody } from "./OpenUiArtifact";
import { hirselLibrary } from "./library";
import { COMPONENT_SCHEMAS, promptSpec } from "./schema";

describe("the OpenUI library", () => {
  it("gives every schema a renderer and every renderer a schema", () => {
    const schemas = COMPONENT_SCHEMAS.map(component => component.name).sort();
    expect(Object.keys(hirselLibrary.components).sort()).toEqual(schemas);
  });
  it("derives the agent prompt from the same schemas", () => {
    const spec = promptSpec();
    expect(spec.root).toBe("Stack");
    expect(spec.components["Metric"]?.signature)
      .toBe('Metric(label: string, value: string, delta?: string, trend?: "up" | "down" | "flat")');
  });
});

describe("rendering a program", () => {
  it("drops a line it cannot use and keeps the rest", () => {
    const body = `root = Stack([good, unknown, broken])
good = Text("Kept")
unknown = Hologram("not in the library")
broken = Metric("Missing value")`;
    const parsed = parseOpenUi(body);
    expect(parsed.meta.errors.some(error => error.component === "Metric")).toBe(true);
    render(() => <Renderer body={body} />);
    expect(screen.getByText("Kept")).toBeTruthy();
    expect(screen.queryByText("not in the library")).toBeNull();
    expect(screen.getByRole("button", { name: /dropped/ })).toBeTruthy();
  });
});

describe("what an action sends to the Thread", () => {
  it("is the readable message plus one fenced payload", () => {
    const body = actionMessageBody(7, {
      message: "Triage",
      action: "triage",
      params: { queue: "inbox" },
      formName: "triage",
      formState: { triage: { who: { value: "Sam", componentType: "Input" } } },
    });
    expect(body.startsWith("Triage\n\n```json\n")).toBe(true);
    const payload: unknown = JSON.parse(body.slice(body.indexOf("{"), body.lastIndexOf("}") + 1));
    expect(payload).toEqual({
      artifact_id: 7,
      action: "triage",
      params: { queue: "inbox" },
      form_state: { triage: { who: { value: "Sam", componentType: "Input" } } },
    });
  });
});
