import assert from "node:assert/strict";
import { readdir } from "node:fs/promises";
import * as vscode from "vscode";
import { waitFor } from "./wait";

export async function run() {
  const extension = vscode.extensions.getExtension("hirsel.hirsel");
  assert.ok(extension);
  assert.ok(!(await readdir(extension.extensionPath)).includes("node_modules"));
  assert.equal(extension.packageJSON.main, "./dist/extension.js");

  const [workspace] = vscode.workspace.workspaceFolders!;
  const uri = vscode.Uri.joinPath(workspace.uri, "main.hirsel");
  const document = await vscode.workspace.openTextDocument(uri);
  const editor = await vscode.window.showTextDocument(document);
  await extension.activate();
  const tokens = await vscode.commands.executeCommand<vscode.SemanticTokens>(
    "vscode.provideDocumentSemanticTokens", uri);
  assert.ok(tokens?.data.length, "the bundled parser and both WASM files loaded");

  const position = document.positionAt(document.getText().lastIndexOf("count"));
  assert.ok(await editor.edit((edit) => edit.insert(document.positionAt(document.getText().length),
    "fn broken() -> u64 { true }\n")));
  await waitFor(() => vscode.languages.getDiagnostics(uri).some(({ code }) =>
    code === "LANG-11" || (typeof code === "object" && code.value === "LANG-11")),
  "a compiler diagnostic for the unsaved detached buffer");
  await waitFor(async () => {
    const hovers = await vscode.commands.executeCommand<vscode.Hover[]>(
      "vscode.executeHoverProvider", uri, position);
    return hovers.some(({ contents }) => contents.some((part) =>
      (typeof part === "string" ? part : part.value).includes("u64")));
  }, "checked hover from the real Hirsel server");
  const targets = await vscode.commands.executeCommand<(vscode.Location | vscode.LocationLink)[]>(
    "vscode.executeDefinitionProvider", uri, position);
  const [target] = targets;
  assert.ok(target);
  const location = "targetUri" in target ? target.targetUri : target.uri;
  assert.equal(location.toString(), uri.toString());

  console.log("VS Code loaded the isolated bundle and used real Hirsel highlighting, hover, definition and diagnostics");
}
