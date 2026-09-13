import { describe, expect, it } from "vitest";
import type { ServerMessage } from "./protocol";

describe("typed Thread icon wire shape", () => {
  const decode = (text: string): ServerMessage => JSON.parse(text) as ServerMessage;

  it("decodes symbol and image variants without string-prefix ambiguity", () => {
    const symbol = decode('{"type":"thread_upsert","thread":{"icon":{"kind":"symbol","name":"leaf","tint":"green"}}}');
    const image = decode('{"type":"thread_upsert","thread":{"icon":{"kind":"image","blob_id":"blob-1"}}}');
    expect(symbol.type === "thread_upsert" && symbol.thread.icon).toEqual({ kind: "symbol", name: "leaf", tint: "green" });
    expect(image.type === "thread_upsert" && image.thread.icon).toEqual({ kind: "image", blob_id: "blob-1" });
  });
});
