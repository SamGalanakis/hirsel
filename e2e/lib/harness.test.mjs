import assert from "node:assert/strict";
import test from "node:test";
import {
  chromiumLaunchOptions,
  hostBinary,
  hostProfile,
  isolatedUrl,
  resolveChromiumExecutable,
} from "./harness.mjs";

test("Chromium uses Playwright discovery when CHROMIUM_EXECUTABLE is unset", () => {
  assert.equal(resolveChromiumExecutable({}), undefined);
  assert.deepEqual(chromiumLaunchOptions({}, {}), { headless: true });
});

test("CHROMIUM_EXECUTABLE is the only browser path override", () => {
  const environment = { CHROMIUM_EXECUTABLE: "/opt/chromium" };
  assert.equal(resolveChromiumExecutable(environment), "/opt/chromium");
  assert.deepEqual(chromiumLaunchOptions({}, environment), { headless: true, executablePath: "/opt/chromium" });
});

test("Host profile selects a repository-derived debug or release binary", () => {
  assert.equal(hostProfile({}), "debug");
  assert.equal(hostProfile({ HIRSEL_E2E_PROFILE: "release" }), "release");
  assert.equal(hostBinary("/checkout", { CARGO_TARGET_DIR: "/build", HIRSEL_E2E_PROFILE: "release" }), "/build/release/hirsel-host");
  assert.throws(() => hostProfile({ HIRSEL_E2E_PROFILE: "fast" }), /debug.*release/);
});

test("isolated URL validation rejects live and non-loopback services", () => {
  assert.equal(isolatedUrl("http://127.0.0.1:40123/"), "http://127.0.0.1:40123");
  assert.throws(() => isolatedUrl("http://127.0.0.1:3076"), /live Host port/);
  assert.throws(() => isolatedUrl("https://example.com:40123"), /loopback/);
});
