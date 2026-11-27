import { cpSync, mkdirSync, mkdtempSync, rmSync } from "node:fs";
import { createRequire } from "node:module";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const cli = createRequire(import.meta.url).resolve("tree-sitter-cli/cli.js");
const grammar = fileURLToPath(new URL("../../tree-sitter-hirsel/", import.meta.url));
const dist = fileURLToPath(new URL("../dist/", import.meta.url));
const output = join(dist, "grammar");
mkdirSync(output, { recursive: true });

// Tree-sitter writes an intermediate WASM file beside parser.c.
const staging = mkdtempSync(join(dist, "tree-sitter-"));
try {
  for (const entry of ["tree-sitter.json", "src"]) {
    cpSync(join(grammar, entry), join(staging, entry), { recursive: true });
  }
  const result = spawnSync(process.execPath, [
    cli, "build", "--wasm", "--output", join(output, "tree-sitter-hirsel.wasm"), staging,
  ], { stdio: "inherit" });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error("Building the Hirsel grammar failed");
  cpSync(join(grammar, "queries", "highlights.scm"), join(output, "highlights.scm"));
} finally {
  rmSync(staging, { recursive: true, force: true });
}
