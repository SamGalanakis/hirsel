import { defineConfig } from "vite";
import solid from "@solidjs/vite-plugin";
import { artifactRuntimePlugin } from "./src/artifacts/runtime-plugin";
export default defineConfig({ resolve: { alias: { assert: "assert/" } }, plugins: [solid(), artifactRuntimePlugin()], optimizeDeps: { entries: ["tools/artifact-smoke.html"] }, build: { rolldownOptions: { input: "tools/artifact-smoke.html" } }, server: { host: "127.0.0.1", port: 48594, strictPort: true }, worker: { format: "es" } });
