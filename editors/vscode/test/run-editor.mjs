import { mkdir, mkdtemp, readFile, readdir, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { runTests } from "@vscode/test-electron";

const adapter = fileURLToPath(new URL("../", import.meta.url));
const scratch = await mkdtemp(join(tmpdir(), "hirsel-vscode-"));
const logs = join(scratch, "user/logs");
try {
  const executable = join(scratch, "hirsel");
  const settings = {
    "hirsel.executable": executable,
    "[hirsel]": { "editor.defaultFormatter": "hirsel.hirsel", "editor.formatOnSave": true },
  };
  const workspace = join(scratch, "workspace");
  await mkdir(workspace);
  const user = join(scratch, "user");
  await mkdir(join(user, "User"), { recursive: true });
  await writeFile(join(user, "User/settings.json"), JSON.stringify(settings));
  await writeFile(join(workspace, "hirsel.toml"), "");
  await writeFile(join(workspace, "main.hirsel"), "fn  main() -> u8 { 0 }\n");
  await writeFile(join(workspace, "target.hirsel"), "fn target() -> u8 { 0 }\n");
  await symlink(join(adapter, "test/mock-lsp.py"), executable);
  await runTests({
    version: "1.105.1",
    extensionDevelopmentPath: adapter,
    extensionTestsPath: join(adapter, "dist/test/editor.js"),
    launchArgs: ["--disable-extensions", "--skip-welcome", "--skip-release-notes",
      "--user-data-dir", user, "--extensions-dir", join(scratch, "extensions"),
      workspace],
    extensionTestsEnv: {
      HIRSEL_MOCK_LOG: join(scratch, "wire.jsonl"),
    },
  });
} catch (error) {
  for (const path of await readdir(logs, { recursive: true }).catch(() => [])) {
    if (path.endsWith("-Hirsel.log")) {
      console.error(await readFile(join(logs, path), "utf8"));
    }
  }
  console.error(await readFile(join(scratch, "wire.jsonl"), "utf8").catch(() => ""));
  throw error;
} finally {
  await rm(scratch, { recursive: true, force: true });
}
