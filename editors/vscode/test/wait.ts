import { setTimeout } from "node:timers/promises";

export async function waitFor(check: () => boolean | Promise<boolean>, description: string) {
  const deadline = Date.now() + 15_000;
  while (Date.now() < deadline) {
    if (await check()) return;
    await setTimeout(50);
  }
  throw new Error("Timed out waiting for " + description);
}
