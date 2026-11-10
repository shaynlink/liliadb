import * as vscode from "vscode";
import * as fs from "node:fs";
import * as path from "node:path";
import { LiliaDatabaseService } from "./LiliaDatabaseService.js";
import type { ConnectionMode, DatabaseConnectionInfo } from "../types.js";

export class ConnectionManager implements vscode.Disposable {
  private readonly _service: LiliaDatabaseService;
  private readonly _onDidChangeConnection = new vscode.EventEmitter<DatabaseConnectionInfo | undefined>();
  readonly onDidChangeConnection = this._onDidChangeConnection.event;

  private _discoveredDatabases: string[] = [];
  private _disposables: vscode.Disposable[] = [];

  constructor() {
    this._service = new LiliaDatabaseService();
    this._disposables.push(this._onDidChangeConnection);

    // Watch workspace for .lilia files
    const watcher = vscode.workspace.createFileSystemWatcher("**/*.lilia");
    watcher.onDidCreate(() => this.scanWorkspaceDatabases());
    watcher.onDidDelete(() => this.scanWorkspaceDatabases());
    this._disposables.push(watcher);
  }

  get service(): LiliaDatabaseService {
    return this._service;
  }

  get activeInfo(): DatabaseConnectionInfo | undefined {
    return this._service.info;
  }

  get discoveredDatabases(): string[] {
    return [...this._discoveredDatabases];
  }

  async initialize(): Promise<void> {
    await this.scanWorkspaceDatabases();

    const config = vscode.workspace.getConfiguration("liliadb");
    const autoConnect = config.get<boolean>("autoConnect", true);
    const defaultMode = config.get<ConnectionMode>("defaultMode", "embedded");

    if (autoConnect && this._discoveredDatabases.length > 0) {
      const targetDb = this._discoveredDatabases[0];
      if (targetDb) {
        try {
          if (defaultMode === "daemon" && this.isDaemonAvailable(targetDb)) {
            await this.connectDaemon(targetDb);
          } else {
            await this.connectEmbedded(targetDb);
          }
        } catch (err) {
          console.warn(`[LiliaDB] Auto-connect to ${targetDb} failed:`, err);
        }
      }
    }
  }

  async scanWorkspaceDatabases(): Promise<string[]> {
    const uris = await vscode.workspace.findFiles("**/*.lilia", "**/node_modules/**");
    this._discoveredDatabases = uris.map(u => u.fsPath);
    return this._discoveredDatabases;
  }

  isDaemonAvailable(dbPath: string): boolean {
    const sockPath = `${dbPath}.sock`;
    const tokenPath = `${dbPath}.token`;
    return fs.existsSync(sockPath) && fs.existsSync(tokenPath);
  }

  async connectEmbedded(filePath: string): Promise<DatabaseConnectionInfo> {
    const info = await this._service.open({
      path: filePath,
      mode: "embedded",
    });
    this._onDidChangeConnection.fire(info);
    vscode.window.showInformationMessage(`LiliaDB: Connected (Embedded) to ${info.databaseName}`);
    return info;
  }

  async connectDaemon(
    filePath: string,
    endpoint?: string,
    tokenFile?: string
  ): Promise<DatabaseConnectionInfo> {
    const info = await this._service.open({
      path: filePath,
      mode: "daemon",
      endpoint,
      tokenFile,
    });
    this._onDidChangeConnection.fire(info);
    vscode.window.showInformationMessage(`LiliaDB: Connected (Daemon) to ${info.databaseName}`);
    return info;
  }

  async switchMode(): Promise<void> {
    const current = this.activeInfo;
    if (!current) {
      vscode.window.showWarningMessage("No active database to switch mode.");
      return;
    }

    const newMode: ConnectionMode = current.mode === "embedded" ? "daemon" : "embedded";
    try {
      if (newMode === "daemon") {
        await this.connectDaemon(current.path, current.endpoint, current.tokenFile);
      } else {
        await this.connectEmbedded(current.path);
      }
    } catch (err: unknown) {
      const msg = err instanceof Error ? err.message : String(err);
      vscode.window.showErrorMessage(`Failed to switch to ${newMode} mode: ${msg}`);
    }
  }

  async disconnect(): Promise<void> {
    const name = this.activeInfo?.databaseName;
    await this._service.close();
    this._onDidChangeConnection.fire(undefined);
    if (name) {
      vscode.window.showInformationMessage(`LiliaDB: Disconnected from ${name}`);
    }
  }

  dispose(): void {
    this._service.close();
    for (const d of this._disposables) {
      d.dispose();
    }
    this._disposables = [];
  }
}
