import { render } from "@solidjs/testing-library";
import { describe, expect, it } from "vitest";
import { TaskState } from "./TaskState";

describe("TaskState", () => {
  it("puts the material headline and findings ahead of conversation content", () => {
    const view = render(() => <TaskState state={{
      revision: 4,
      headline: "2 children · #7 running",
      own_headline: "Release evidence collected",
      findings: ["Linux checks passed", "Android remains"],
      artifact_ids: [],
      checkpoint_at: "2026-09-20T10:00:00Z",
      steering_revision: 1,
    }} />);
    expect(view.getByRole("heading", { name: "Task state" })).toBeInTheDocument();
    expect(view.getByText("2 children · #7 running")).toBeVisible();
    expect(view.getByText("Linux checks passed")).toBeVisible();
    expect(view.getByText("State revision 4")).toBeVisible();
  });

  it("renders a twelve-word normalized headline without substituting the own headline", () => {
    const headline = "one two three four five six seven eight nine ten eleven twelve";
    const view = render(() => <TaskState state={{
      revision: 2,
      headline,
      own_headline: "Worker prose is not the rollup",
      findings: [],
      artifact_ids: [],
      checkpoint_at: null,
      steering_revision: 0,
    }} />);
    expect(view.getByText(headline, { exact: true })).toBeVisible();
    expect(view.queryByText("Worker prose is not the rollup", { exact: true })).toBeNull();
  });
});
