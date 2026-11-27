import * as vscode from "vscode";
import { LanguageClient } from "vscode-languageclient/node";
import type { Tree } from "web-tree-sitter";
import { createHighlighter, highlightRanges, splitLines, tokenModifiers, tokenTypes } from "./highlights";

const documentSelector = [{ scheme: "file", language: "hirsel" }];
let client: LanguageClient | undefined;

export async function activate(context: vscode.ExtensionContext): Promise<void> {
  const output = vscode.window.createOutputChannel("Hirsel");
  context.subscriptions.push(output);

  const executable = vscode.workspace.getConfiguration("hirsel").get<string>("executable");
  client = new LanguageClient("hirsel", "Hirsel", {
    command: executable || "hirsel",
    args: ["lsp"],
    // The default transport is stdio and keeps the CLI arguments as written.
  }, { documentSelector, outputChannel: output });

  const { parser, query } = await createHighlighter(context.asAbsolutePath("dist/grammar"));
  const trees = new Map<vscode.TextDocument, { version: number; tree: Tree }>();
  const release = (document: vscode.TextDocument) => {
    trees.get(document)?.tree.delete();
    trees.delete(document);
  };

  const legend = new vscode.SemanticTokensLegend(tokenTypes, tokenModifiers);
  context.subscriptions.push(
    vscode.languages.registerDocumentSemanticTokensProvider(documentSelector, {
      provideDocumentSemanticTokens(document) {
        const source = document.getText();
        let entry = trees.get(document);
        if (entry?.version !== document.version) {
          release(document);
          const tree = parser.parse(source);
          if (!tree) return undefined;
          entry = { version: document.version, tree };
          trees.set(document, entry);
        }

        const tokens = new vscode.SemanticTokensBuilder(legend);
        for (const range of highlightRanges(query, entry.tree)) {
          for (const token of splitLines(source, range)) {
            const position = document.positionAt(token.start);
            tokens.push(position.line, position.character, token.end - token.start,
              token.type, token.modifiers);
          }
        }
        return tokens.build();
      },
    }, legend),
    vscode.workspace.onDidCloseTextDocument(release),
    { dispose() {
      for (const document of trees.keys()) release(document);
      query.delete();
      parser.delete();
    } },
  );

  // Keep the local highlighter available if the installed server cannot start.
  void client.start().catch((error: unknown) => output.appendLine(String(error)));
}

export async function deactivate(): Promise<void> {
  await client?.dispose();
  client = undefined;
}
