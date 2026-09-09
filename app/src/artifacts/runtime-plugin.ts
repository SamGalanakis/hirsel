import { build } from "esbuild";
import { fileURLToPath } from "node:url";

/** Ship one local runtime as text. The preview never imports app code or a CDN. */
export function artifactRuntimePlugin() {
  const virtualId = "virtual:artifact-runtime";
  let bundled: Promise<string> | undefined;
  return {
    name: "hirsel:artifact-runtime",
    resolveId(id: string) { return id === virtualId ? `\0${virtualId}` : null; },
    async load(id: string) {
      if (id !== `\0${virtualId}`) return null;
      bundled ??= build({
        entryPoints: [fileURLToPath(new URL("./runtime-entry.ts", import.meta.url))],
        bundle: true, write: false, tsconfigRaw: {}, format: "iife", platform: "browser",
        conditions: ["browser"], minify: true,
        define: { "process.env.NODE_ENV": '"production"', _SOLID_DEV_: "false" },
      }).then(result => result.outputFiles[0].text);
      return `export default ${JSON.stringify(await bundled)};`;
    },
  };
}
