import { render, screen, fireEvent, waitFor } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { describe, expect, it, vi } from "vitest";
import { Renderer, parseOpenUi } from "./Renderer";
import { actionMessageBody } from "./OpenUiArtifact";
import { hirselLibrary } from "./library";
import { COMPONENT_SCHEMAS, promptSpec } from "./schema";

/** One fixture using every component, so a schema without a renderer — or a
 * signature the parser rejects — fails here rather than in a conversation. */
const EVERY_COMPONENT = `root = Stack([band, body, media, panel, tabs, form, replies], "col", "md")
band = Stack([hits, misses], "row", "sm")
hits = Metric("Hits", "1,204", "+8%", "up")
misses = Metric("Misses", "37", "-3", "down")
body = Section("Report", [lede, prose, notice, table, list, chart, code, rule], "Last seven days")
lede = Heading("This week", 2)
prose = Text("Traffic held steady.", "muted", "sm")
notice = Callout("Two sources are stale.", "warning", "Stale data")
table = Table(["Source", "Rows"], [["events", "1204"], ["clicks", "37"]], "By source")
list = List([first, second], true)
first = ListItem("Reindex events", "today")
second = ListItem("Drop clicks", "friday")
chart = Chart("bar", ["Mon", "Tue", "Wed"], [3, 9, 5], "Daily")
code = CodeBlock("select 1", "sql")
rule = Separator()
media = Image("data:image/gif;base64,R0lGODlhAQABAAAAACw=", "A dot", "Figure 1")
panel = Card([md], "Notes", "From the run")
md = Markdown("A **bold** note.")
tabs = Tabs([one, two])
one = TabItem("one", "First", [firstBody])
firstBody = Text("Inside the first tab.")
two = TabItem("two", "Second", [secondBody])
secondBody = Text("Inside the second tab.")
form = Form("triage", [who, why, when, urgent, team, size], [go, skip])
who = Input("who", "Owner", "name", "text", ["required"])
why = Textarea("why", "Why")
when = DatePicker("when", "When")
urgent = Checkbox("urgent", "Urgent")
team = Radio("team", ["Core", "Web"], "Team")
size = Slider("size", "Size", 0, 10, 1, 4)
go = Button("Triage", "triage", "primary")
skip = Button("Skip", "skip", "secondary")
replies = FollowUp(["Show me last week", "Export as CSV"])`;

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
  it("draws every component in the fixture without a parse error", () => {
    const parsed = parseOpenUi(EVERY_COMPONENT);
    expect(parsed.meta.errors).toEqual([]);
    expect(parsed.meta.unresolved).toEqual([]);
    expect(parsed.meta.orphaned).toEqual([]);
    render(() => <Renderer body={EVERY_COMPONENT} />);
    expect(screen.getByText("1,204")).toBeTruthy();
    expect(screen.getByRole("table")).toBeTruthy();
    expect(screen.getByRole("img", { name: /Daily chart/ })).toBeTruthy();
    expect(screen.getByRole("form", { name: "triage" })).toBeTruthy();
    expect(screen.getByLabelText("Owner")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Show me last week" })).toBeTruthy();
    // Tabs draw only the selected panel.
    expect(screen.getByText("Inside the first tab.")).toBeTruthy();
    expect(screen.queryByText("Inside the second tab.")).toBeNull();
  });

  it("renders the structure that has arrived and completes it on the next chunk", async () => {
    const [body, setBody] = createBody("root = Stack([lede])\nlede = Heading(\"Live\", 2)");
    render(() => <Renderer body={body()} />);
    expect(screen.getByText("Live")).toBeTruthy();
    expect(screen.queryByText("Arrived later")).toBeNull();
    setBody("root = Stack([lede, tail])\nlede = Heading(\"Live\", 2)\ntail = Text(\"Arrived later\")");
    await waitFor(() => expect(screen.getByText("Arrived later")).toBeTruthy());
    // The heading that had already arrived is still the same element.
    expect(screen.getByText("Live")).toBeTruthy();
  });

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

  it("reports what was filled in when a form's primary button fires", () => {
    const onAction = vi.fn();
    const body = `root = Stack([form])
form = Form("triage", [who, urgent], [go])
who = Input("who", "Owner", "name", "text", ["required"])
urgent = Checkbox("urgent", "Urgent", true)
go = Button("Triage", "triage", "primary")`;
    render(() => <Renderer body={body} onAction={onAction} />);
    // A required field that is still empty holds the action back.
    fireEvent.click(screen.getByRole("button", { name: "Triage" }));
    expect(onAction).not.toHaveBeenCalled();
    expect(screen.getByRole("alert").textContent).toContain("required");

    fireEvent.input(screen.getByLabelText("Owner"), { target: { value: "Sam" } });
    fireEvent.click(screen.getByRole("button", { name: "Triage" }));
    expect(onAction).toHaveBeenCalledTimes(1);
    expect(onAction.mock.calls[0][0]).toEqual({
      message: "Triage",
      action: "triage",
      params: {},
      formName: "triage",
      formState: { triage: { who: { value: "Sam", componentType: "Input" }, urgent: { value: true, componentType: "Checkbox" } } },
    });
  });

  it("sends a follow-up as its own plain message", () => {
    const onAction = vi.fn();
    render(() => <Renderer body={'root = Stack([replies])\nreplies = FollowUp(["Export as CSV"])'} onAction={onAction} />);
    fireEvent.click(screen.getByRole("button", { name: "Export as CSV" }));
    expect(onAction.mock.calls[0][0].message).toBe("Export as CSV");
    expect(onAction.mock.calls[0][0].action).toBe("follow_up");
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

/** A body that a test can grow, the way an edited artifact grows. */
function createBody(initial: string): [() => string, (next: string) => void] {
  const [body, setBody] = createSignal(initial);
  return [body, (next: string) => setBody(next)];
}
