import { execFile } from "node:child_process";
import { constants } from "node:fs";
import { access, stat } from "node:fs/promises";
import { homedir } from "node:os";
import { delimiter, join, resolve } from "node:path";
import { promisify } from "node:util";
import * as vscode from "vscode";
import { LanguageClient, State } from "vscode-languageclient/node";

export const documentSelector = [{ scheme: "file", language: "hirsel" }];
const restartCommand = "hirsel.restartServer";
const outputCommand = "hirsel.showOutput";
const run = promisify(execFile);

async function findExecutable(): Promise<string> {
  const configured = vscode.workspace.getConfiguration("hirsel").get<string>("executable")?.trim();
  if (configured) return configured;

  const name = process.platform === "win32" ? "hirsel.exe" : "hirsel";
  const directories = [
    ...(process.env.PATH?.split(delimiter) ?? []),
    join(homedir(), ".local", "bin"),
    join(homedir(), ".cargo", "bin"),
  ];
  for (const directory of directories) {
    const candidate = resolve(directory, name);
    if (await access(candidate, constants.X_OK).then(() => true, () => false) &&
        (await stat(candidate).catch(() => undefined))?.isFile()) return candidate;
  }
  throw new Error("Install Hirsel on PATH or set hirsel.executable in Settings.");
}

type ServerState = "starting" | "ready" | "error";

export class Server {
  private readonly output = vscode.window.createOutputChannel("Hirsel");
  private readonly status = vscode.window.createStatusBarItem("hirsel.server", vscode.StatusBarAlignment.Left);
  private readonly watchers = ["**/hirsel.lock", "**/*.hcfg"]
    .map((glob) => vscode.workspace.createFileSystemWatcher(glob));
  private client?: LanguageClient;
  private pending = Promise.resolve();
  private closed = false;
  private executable?: string;
  private version?: string;

  constructor(context: vscode.ExtensionContext) {
    this.status.name = "Hirsel Language Server";
    this.status.command = outputCommand;
    context.subscriptions.push(this.output, this.status, ...this.watchers,
      vscode.commands.registerCommand(outputCommand, () => this.output.show()),
      vscode.commands.registerCommand(restartCommand, () => this.restart()),
      vscode.workspace.onDidChangeConfiguration((event) => {
        if (event.affectsConfiguration("hirsel.executable")) void this.restart();
      }),
    );
  }

  restart(): Promise<void> {
    this.pending = this.pending.then(() => this.start());
    return this.pending;
  }

  async stop(): Promise<void> {
    this.closed = true;
    await this.pending;
    await this.disposeClient();
  }

  private async disposeClient() {
    const client = this.client;
    this.client = undefined;
    await client?.dispose();
  }

  private async start() {
    if (this.closed) return;
    this.executable = undefined;
    this.version = undefined;
    this.show("starting");
    try {
      await this.disposeClient();
      this.executable = await findExecutable();
      const { stdout } = await run(this.executable, ["--version"], { timeout: 5000 });
      this.version = /^hirsel\s+(\S+)/i.exec(stdout)?.[1];
      if (!this.version) throw new Error("Expected a version from hirsel --version.");
      if (this.closed) return;
      this.show("starting");

      const client = new LanguageClient("hirsel", "Hirsel", {
        command: this.executable,
        args: ["lsp"],
      }, { documentSelector, outputChannel: this.output, synchronize: { fileEvents: this.watchers } });
      this.client = client;
      let started = false;
      client.onDidChangeState(({ newState }) => {
        if (!started || this.closed || this.client !== client) return;
        if (newState === State.Starting) this.show("starting");
        if (newState === State.Running) this.show("ready");
        if (newState === State.Stopped) queueMicrotask(() => {
          // The client can restart automatically in the same turn.
          if (!this.closed && this.client === client && client.state === State.Stopped) {
            this.fail("Hirsel language server stopped.");
          }
        });
      });
      await client.start();
      started = true;
      this.show("ready");
    } catch (error) {
      this.fail(`Hirsel language server failed to start. ${error instanceof Error ? error.message : error}`);
    }
  }

  private show(state: ServerState, detail?: string) {
    if (this.closed) return;
    const icon = { starting: "$(loading~spin)", ready: "$(check)", error: "$(error)" }[state];
    this.status.text = `Hirsel${this.version ? ` ${this.version}` : ""} ${icon} ${state}`;
    this.status.tooltip = [detail, this.executable, "Show Hirsel output"].filter(Boolean).join("\n");
    this.status.show();
  }

  private fail(message: string) {
    if (this.closed) return;
    this.output.appendLine(message);
    this.show("error", message);
    void vscode.window.showErrorMessage(message, "Show Output", "Restart").then((choice) => {
      if (choice === "Show Output") this.output.show();
      if (choice === "Restart") void this.restart();
    });
  }
}
