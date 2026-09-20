import { flush } from "solid-js";
import { beforeEach, describe, expect, it } from "vitest";

import { makeThread } from "../threads/fixtures";
import {
  enterSpace,
  resetSpaces,
  spaceForThread,
  spaceState,
  stepIntoWorker,
  topLevelSpaceForThread,
} from "./store";

const threads = [
  makeThread(1, { title: "Hirsel", kind: "space", parent_thread_id: null }),
  makeThread(2, { title: "Planning", kind: "space", parent_thread_id: 1 }),
  makeThread(3, { title: "Space chat contract", kind: "task", parent_thread_id: 2 }),
];

beforeEach(() => flush(resetSpaces));

describe("Space conversation state", () => {
  it("keeps the recipient and worker pairing independent", () => {
    flush(() => enterSpace(1));
    expect(spaceState).toMatchObject({ spaceRecipientId: 1, workerPairingId: null });

    flush(() => stepIntoWorker(threads, 3));
    expect(spaceState).toMatchObject({ spaceRecipientId: 2, workerPairingId: 3 });
  });

  it("finds the nearest and top-level containing Spaces", () => {
    expect(spaceForThread(threads, 3)?.id).toBe(2);
    expect(topLevelSpaceForThread(threads, 3)?.id).toBe(1);
  });
});
