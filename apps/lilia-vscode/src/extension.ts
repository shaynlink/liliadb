import * as vscode from "vscode";
import { ConnectionManager } from "./connection/ConnectionManager.js";
import { LiliaTreeDataProvider } from "./providers/LiliaTreeDataProvider.js";
import { StatusBarProvider } from "./providers/StatusBarProvider.js";
import { VisualizerPanel } from "./webviews/VisualizerPanel.js";
import { CommandConsolePanel } from "./webviews/CommandConsolePanel.js";
import { registerConnectionCommands } from "./commands/connectionCommands.js";
import { registerDataCommands } from "./commands/dataCommands.js";
import { registerExecutionCommands } from "./commands/executionCommands.js";

let connectionManager: ConnectionManager | undefined;

export async function activate(context: vscode.ExtensionContext): Promise<void> {
  connectionManager = new ConnectionManager();
  context.subscriptions.push(connectionManager);

  // Status Bar
  const statusBar = new StatusBarProvider(connectionManager);
  context.subscriptions.push(statusBar);

  // Tree View Explorer
  const treeDataProvider = new LiliaTreeDataProvider(connectionManager);
  const treeView = vscode.window.createTreeView("lilia-explorer", {
    treeDataProvider,
    showCollapseAll: true,
  });
  context.subscriptions.push(treeView);

  // Refresh Command
  context.subscriptions.push(
    vscode.commands.registerCommand("lilia.refresh", () => {
      treeDataProvider.refresh();
      vscode.window.showInformationMessage("LiliaDB: Explorer refreshed.");
    })
  );

  // Visualizer Panel Command
  context.subscriptions.push(
    vscode.commands.registerCommand(
      "lilia.openVisualizer",
      (target?: { model: string; space?: string; namespace?: string; graph?: string; collection?: string; id?: string; key?: string }) => {
        VisualizerPanel.createOrShow(context.extensionUri, connectionManager!, target);
      }
    )
  );

  // Console Command
  context.subscriptions.push(
    vscode.commands.registerCommand(
      "lilia.openConsole",
      (initialCommand?: { type: "sql" | "batch"; text: string }) => {
        CommandConsolePanel.createOrShow(context.extensionUri, connectionManager!, initialCommand);
      }
    )
  );

  // Additional command groups
  registerConnectionCommands(context, connectionManager);
  registerDataCommands(context, connectionManager);
  registerExecutionCommands(context, connectionManager);

  // Auto-discover workspace databases & auto-connect if enabled
  try {
    await connectionManager.initialize();
  } catch (err) {
    console.warn("[LiliaDB] Initialization error:", err);
  }
}

export async function deactivate(): Promise<void> {
  if (connectionManager) {
    await connectionManager.disconnect();
    connectionManager.dispose();
    connectionManager = undefined;
  }
}
