import { cpSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";

const resolve = createRequire(import.meta.url).resolve;
const cli = resolve("tree-sitter-cli/cli.js");
const adapter = fileURLToPath(new URL("../", import.meta.url));
const license = fileURLToPath(new URL("../../../LICENSE", import.meta.url));
const grammar = fileURLToPath(new URL("../../tree-sitter-hirsel/", import.meta.url));
const dist = join(adapter, "dist");
const output = join(dist, "grammar");
rmSync(dist, { recursive: true, force: true });
mkdirSync(output, { recursive: true });

// Tree-sitter writes an intermediate WASM file beside parser.c.
const staging = mkdtempSync(join(dist, "tree-sitter-"));
try {
  for (const entry of ["tree-sitter.json", "src", "data/src"]) {
    cpSync(join(grammar, entry), join(staging, entry), { recursive: true });
  }
  for (const [subdir, name, query, stagedQuery] of [
    ["", "hirsel", "queries/highlights.scm", "highlights.scm"],
    ["data", "hirsel_data", "data/queries/hirsel_data/highlights.scm", "data-highlights.scm"],
  ]) {
    const result = spawnSync(process.execPath, [
      cli, "build", "--wasm", "--output", join(output, `tree-sitter-${name}.wasm`), join(staging, subdir),
    ], { stdio: "inherit" });
    if (result.error) throw result.error;
    if (result.status !== 0) throw new Error(`Building the ${name} grammar failed`);
    cpSync(join(grammar, query), join(output, stagedQuery));
  }
  cpSync(resolve("web-tree-sitter/tree-sitter.wasm"), join(dist, "tree-sitter.wasm"));
} finally {
  rmSync(staging, { recursive: true, force: true });
}

const { metafile } = await build({
  absWorkingDir: adapter,
  entryPoints: ["src/extension.ts"],
  outfile: join(dist, "extension.js"),
  bundle: true,
  platform: "node",
  target: "node20",
  external: ["vscode"],
  metafile: true,
  // The CommonJS loader resolves its WASM beside the output file.
  alias: { "web-tree-sitter": resolve("web-tree-sitter") },
});

const bundled = /^(.*\bnode_modules\/(?:@[^/]+\/)?[^/]+)\//;
const packages = new Set(Object.keys(metafile.inputs).map((file) => bundled.exec(file)?.[1]).filter(Boolean));
const notices = [readFileSync(license, "utf8").trimEnd()];
for (const path of [...packages].sort()) {
  const directory = join(adapter, path);
  const files = readdirSync(directory).filter((file) => /^(license|thirdpartynotices)(\..+)?$/i.test(file));
  if (files.length === 0) throw new Error(`Missing license for bundled package ${path}`);
  for (const file of files.sort()) {
    notices.push(`${path.slice(path.lastIndexOf("node_modules/") + "node_modules/".length)} / ${file}\n\n` +
      readFileSync(join(directory, file), "utf8").trim());
  }
}
writeFileSync(join(adapter, "LICENSE"), notices.join("\n\n---\n\n") + "\n");
