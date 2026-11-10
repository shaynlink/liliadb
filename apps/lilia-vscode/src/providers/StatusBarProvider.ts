import * as vscode from "vscode";
import type { ConnectionManager } from "../connection/ConnectionManager.js";
import type { DatabaseConnectionInfo } from "../types.js";

export class StatusBarProvider implements vscode.Disposable {
  private readonly _item: vscode.StatusBarItem;
  private _disposables: vscode.Disposable[] = [];

  constructor(private readonly connectionManager: ConnectionManager) {
    this._item = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Right, 100);
    this._item.command = "lilia.switchMode";
    this._disposables.push(this._item);

    this.update(connectionManager.activeInfo);

    this._disposables.push(
      connectionManager.onDidChangeConnection(info => {
        this.update(info);
      })
    );

    this._item.show();
  }

  update(info?: DatabaseConnectionInfo): void {
    if (!info || !info.connected) {
      this._item.text = "$(plug) Lilia: Disconnected";
      this._item.tooltip = "Click to connect to a LiliaDB database";
      this._item.command = "lilia.openDatabaseFile";
    } else if (info.mode === "embedded") {
      this._item.text = `$(database) Lilia: Embedded (${info.databaseName})`;
      this._item.tooltip = `Connected in Embedded (Direct File) mode.\nPath: ${info.path}\nClick to switch mode.`;
      this._item.command = "lilia.switchMode";
    } else {
      this._item.text = `$(radio-tower) Lilia: Daemon (${info.databaseName})`;
      this._item.tooltip = `Connected via Daemon.\nEndpoint: ${info.endpoint || "default"}\nClick to switch mode.`;
      this._item.command = "lilia.switchMode";
    }
  }

  dispose(): void {
    for (const d of this._disposables) {
      d.dispose();
    }
    this._disposables = [];
  }
}
