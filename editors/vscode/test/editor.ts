import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { setTimeout } from "node:timers/promises";
import * as vscode from "vscode";

async function waitFor(check: () => boolean | Promise<boolean>, description: string) {
  const deadline = Date.now() + 15_000;
  while (Date.now() < deadline) {
    if (await check()) return;
    await setTimeout(50);
  }
  throw new Error("Timed out waiting for " + description);
}

async function wire() {
  const content = await readFile(process.env.HIRSEL_MOCK_LOG!, "utf8").catch((error) => {
    if (error.code === "ENOENT") return "";
    throw error;
  });
  return content.trim().split("\n").filter(Boolean)
    .map((line) => JSON.parse(line) as { argv?: string[]; method?: string });
}

export async function run() {
  const [workspace] = vscode.workspace.workspaceFolders!;
  const uri = vscode.Uri.joinPath(workspace.uri, "main.hirsel");
  const document = await vscode.workspace.openTextDocument(uri);
  const editor = await vscode.window.showTextDocument(document);
  assert.equal(document.languageId, "hirsel");
  await waitFor(async () => (await wire()).some((message) => message.method === "textDocument/didOpen"),
    "didOpen");

  const tokens = await vscode.commands.executeCommand<vscode.SemanticTokens>(
    "vscode.provideDocumentSemanticTokens", uri);
  assert.ok(tokens?.data.length);

  const replaceDigit = async (from: string, to: string) => {
    const start = document.getText().lastIndexOf(from);
    assert.ok(start >= 0);
    assert.ok(await editor.edit((edit) => edit.replace(
      new vscode.Range(document.positionAt(start), document.positionAt(start + 1)), to)));
  };
  await replaceDigit("0", "1");
  await waitFor(() => vscode.languages.getDiagnostics(uri).some((diagnostic) => diagnostic.code === "MOCK-1"),
    "the diagnostic from an unsaved edit");

  const position = new vscode.Position(0, 1);
  const hovers = await vscode.commands.executeCommand<vscode.Hover[]>("vscode.executeHoverProvider", uri, position);
  assert.ok(hovers.flatMap((hover) => hover.contents).some((part) =>
    typeof part === "string" ? part === "`u8`" : part.value === "`u8`"));
  const locations = await vscode.commands.executeCommand<vscode.Location[]>(
    "vscode.executeDefinitionProvider", uri, position);
  assert.equal(locations[0].uri.toString(), vscode.Uri.joinPath(workspace.uri, "target.hirsel").toString());

  assert.ok(await document.save());
  assert.equal(await readFile(uri.fsPath, "utf8"), "fn main() -> u8 { 1 }\n");
  await replaceDigit("1", "2");
  assert.ok(await document.save());
  assert.equal(await readFile(uri.fsPath, "utf8"), "fn main() -> u8 { 2 }\n");

  const messages = await wire();
  assert.deepEqual(messages[0].argv, ["hirsel", "lsp"]);
  const methods = messages.map((message) => message.method);
  for (const method of ["initialize", "didOpen", "didChange", "hover", "definition"]) {
    assert.ok(methods.includes(method === "initialize" ? method : "textDocument/" + method), method);
  }
  assert.equal(methods.filter((method) => method === "textDocument/formatting").length, 2);
  console.log("VS Code stdio, semantic tokens and two format-on-save operations passed");
}
