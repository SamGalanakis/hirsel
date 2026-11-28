import assert from "node:assert/strict";
import { cp, mkdir, mkdtemp, readFile, readdir, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { delimiter, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { runTests } from "@vscode/test-electron";

const adapter = fileURLToPath(new URL("../", import.meta.url));
const real = process.argv.includes("--real");
const driver = process.env.HIRSEL_TEST_DRIVER;
assert.ok(!real || driver, "Set HIRSEL_TEST_DRIVER to the real hirsel binary");
const scratch = await mkdtemp(join(tmpdir(), "hirsel-vscode-"));
const logs = join(scratch, "user/logs");
try {
  const packaged = join(scratch, "package");
  await mkdir(packaged);
  const files = ["dist", "package.json", "language-configuration.json", "README.md", "LICENSE"];
  for (const file of files) {
    await cp(join(adapter, file), join(packaged, file), { recursive: true });
  }
  assert.deepEqual((await readdir(packaged)).sort(), files.sort());
  const bin = join(scratch, "bin");
  await mkdir(bin);
  const executable = join(bin, "hirsel");
  const settings = {
    ...(real ? {} : { "hirsel.executable": executable }),
    "[hirsel]": { "editor.defaultFormatter": "hirsel.hirsel", "editor.formatOnSave": true },
  };
  const workspace = join(scratch, "workspace");
  await mkdir(workspace);
  const user = join(scratch, "user");
  await mkdir(join(user, "User"), { recursive: true });
  await writeFile(join(user, "User/settings.json"), JSON.stringify(settings));
  if (real) {
    await writeFile(join(workspace, "main.hirsel"), "fn answer() -> u64 { let count = 42u64; count }\n");
    await symlink(resolve(driver), executable);
  } else {
    await writeFile(join(workspace, "hirsel.toml"), "");
    await writeFile(join(workspace, "main.hirsel"), "fn  main() -> u8 { 0 }\n");
    await writeFile(join(workspace, "target.hirsel"), "fn target() -> u8 { 0 }\n");
    await symlink(join(adapter, "test/mock-lsp.py"), executable);
  }
  await runTests({
    version: "1.105.1",
    vscodeExecutablePath: process.env.VSCODE_TEST_EXECUTABLE,
    extensionDevelopmentPath: packaged,
    extensionTestsPath: join(adapter, `.test-build/test/${real ? "bundle" : "editor"}.js`),
    launchArgs: ["--disable-extensions", "--skip-welcome", "--skip-release-notes",
      "--user-data-dir", user, "--extensions-dir", join(scratch, "extensions"),
      workspace],
    extensionTestsEnv: {
      PATH: [bin, process.env.PATH].join(delimiter),
      NODE_PATH: "",
      HIRSEL_MOCK_LOG: join(scratch, "wire.jsonl"),
    },
  });
} catch (error) {
  for (const path of await readdir(logs, { recursive: true }).catch(() => [])) {
    if (path.endsWith("-Hirsel.log") || path.endsWith("/exthost.log")) {
      console.error(await readFile(join(logs, path), "utf8"));
    }
  }
  console.error(await readFile(join(scratch, "wire.jsonl"), "utf8").catch(() => ""));
  throw error;
} finally {
  await rm(scratch, { recursive: true, force: true });
}
