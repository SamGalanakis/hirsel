import { describe, expect, it } from "vitest";
import { flush } from "solid-js";
import { dispatch, state } from "./store";
import { initialState } from "./types";
import type { HelloOkMsg, ProcessInfo } from "../protocol";

const PROCESS: ProcessInfo = {
  thread_id: 1, id: "proc-1", name: "Do the thing", trigger: null, trigger_subscription_key: null,
  trigger_revision: null, trigger_enabled: null, trigger_recurring: false, cancellable: true, state: "running",
  started_ts: "2026-07-09T00:00:00Z", last_event_ts: "2026-07-09T00:00:00Z", last_fired_ts: null, last_outcome: null,
};

const HELLO: HelloOkMsg = {
  type: "hello_ok", history_id: "test-history", threads: [], processes: [], views: [],
  host_version: "test-host", model: null, subagent_models: null, prompts: null, providers: null,
};

describe("app store dispatch", () => {
  it("renders every AppState field the reducer produced, named or not", () => {
    // Keyed off initialState rather than a hand-written list: a field added to
    // AppState is covered here the moment it is declared.
    const fields = Object.keys(initialState()) as (keyof ReturnType<typeof initialState>)[];
    const hello: HelloOkMsg = { ...HELLO, host_version: "9.9.9", processes: [PROCESS] };
    flush(() => dispatch({ type: "hello_ok", payload: hello }));
    for (const field of fields) expect(state[field], `AppState.${field} was not applied`).not.toBeUndefined();
    expect(state.hostVersion).toBe("9.9.9");
    expect(state.processes.map(row => row.id)).toEqual(["proc-1"]);
    flush(() => dispatch({ type: "connection_status", status: "reconnecting" }));
    expect(state.connection).toBe("reconnecting");
    expect(state.hostVersion).toBe("9.9.9");
  });
});
