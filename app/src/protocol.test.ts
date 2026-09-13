import { describe, expect, it } from "vitest";
import type { ServerMessage } from "./protocol";

describe("typed Thread icon wire shape", () => {
  const decode = (text: string): ServerMessage => JSON.parse(text) as ServerMessage;

  it("decodes emoji and image variants without string-prefix ambiguity", () => {
    const emoji = decode('{"type":"thread_upsert","thread":{"icon":{"kind":"emoji","value":"🌱"}}}');
    const image = decode('{"type":"thread_upsert","thread":{"icon":{"kind":"image","blob_id":"blob-1"}}}');
    expect(emoji.type === "thread_upsert" && emoji.thread.icon).toEqual({ kind: "emoji", value: "🌱" });
    expect(image.type === "thread_upsert" && image.thread.icon).toEqual({ kind: "image", blob_id: "blob-1" });
  });
});
