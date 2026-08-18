import { afterEach, describe, expect, it, vi } from "vitest";
import { resolveWsUrl } from "./endpoint";

afterEach(() => {
  vi.unstubAllEnvs();
});

describe("resolveWsUrl", () => {
  it("uses the serving origin by default in development", () => {
    vi.stubEnv("VITE_WS_URL", undefined);

    const protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
    expect(resolveWsUrl()).toBe(`${protocol}//${window.location.host}/ws`);
  });

  it("preserves an explicit host endpoint", () => {
    vi.stubEnv("VITE_WS_URL", "ws://127.0.0.1:3089/ws");
    expect(resolveWsUrl()).toBe("ws://127.0.0.1:3089/ws");
  });
});
