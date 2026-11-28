import * as vscode from "vscode";
import type { Tree } from "web-tree-sitter";
import { createHighlighter, highlightRanges, splitLines, tokenModifiers, tokenTypes } from "./highlights";
import { Server, dataLanguageId, documentSelector } from "./server";

let server: Server | undefined;

export async function activate(context: vscode.ExtensionContext): Promise<void> {
  const directory = context.asAbsolutePath("dist/grammar");
  const sourceHighlighter = await createHighlighter(directory);
  const dataHighlighter = await createHighlighter(directory, true);
  const trees = new Map<vscode.TextDocument, { version: number; tree: Tree }>();
  const release = (document: vscode.TextDocument) => {
    trees.get(document)?.tree.delete();
    trees.delete(document);
  };

  const legend = new vscode.SemanticTokensLegend(tokenTypes, tokenModifiers);
  context.subscriptions.push(
    vscode.languages.registerDocumentSemanticTokensProvider(documentSelector, {
      provideDocumentSemanticTokens(document) {
        const { parser, query } = document.languageId === dataLanguageId ? dataHighlighter : sourceHighlighter;
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
      for (const { parser, query } of [sourceHighlighter, dataHighlighter]) {
        query.delete();
        parser.delete();
      }
    } },
  );

  server = new Server(context);
  void server.restart();
}

export async function deactivate(): Promise<void> {
  await server?.stop();
  server = undefined;
}
