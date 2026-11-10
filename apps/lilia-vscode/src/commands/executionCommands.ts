import * as vscode from "vscode";
import type { ConnectionManager } from "../connection/ConnectionManager.js";
import { CommandConsolePanel } from "../webviews/CommandConsolePanel.js";

export function registerExecutionCommands(
  context: vscode.ExtensionContext,
  connectionManager: ConnectionManager
): void {
  // Execute Active File
  context.subscriptions.push(
    vscode.commands.registerCommand("lilia.executeActiveFile", async () => {
      const editor = vscode.window.activeTextEditor;
      if (!editor) {
        vscode.window.showWarningMessage("No active editor found to execute.");
        return;
      }

      const text = editor.document.getText().trim();
      const fileName = editor.document.fileName.toLowerCase();

      if (!text) {
        vscode.window.showWarningMessage("The active file is empty.");
        return;
      }

      const isSql = fileName.endsWith(".sql");
      const isBatch = fileName.endsWith(".lilia.json") || fileName.endsWith(".json");

      if (!isSql && !isBatch) {
        vscode.window.showWarningMessage("Only .sql and .json files can be executed in LiliaDB.");
        return;
      }

      const type = isSql ? "sql" : "batch";
      const consolePanel = CommandConsolePanel.createOrShow(context.extensionUri, connectionManager, {
        type,
        text,
      });

      vscode.window.showInformationMessage(`Loaded ${isSql ? "SQL query" : "Batch operations"} into LiliaDB Console.`);
    })
  );
}
