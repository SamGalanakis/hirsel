import { defineConfig } from "vite";
import solid from "@solidjs/vite-plugin";
import { artifactRuntimePlugin } from "./src/artifacts/runtime-plugin";
// `cacheDir` is this config's own: e2e/run.mjs clears it before every run so
// the dependency optimizer is always cold, which makes its "dependencies
// optimized" line a reliable readiness signal instead of a mid-test reload.
export default defineConfig({ cacheDir: "node_modules/.vite-artifact-preview", resolve: { alias: { assert: "assert/" } }, plugins: [solid(), artifactRuntimePlugin()], optimizeDeps: { entries: ["tools/artifact-smoke.html"] }, build: { rolldownOptions: { input: "tools/artifact-smoke.html" } }, server: { host: "127.0.0.1", port: 48594, strictPort: true }, worker: { format: "es" } });
