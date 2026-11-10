import * as vscode from "vscode";
import type { ConnectionManager } from "../connection/ConnectionManager.js";

export function registerConnectionCommands(
  context: vscode.ExtensionContext,
  connectionManager: ConnectionManager
): void {
  // 1. Open Database File (Embedded Mode)
  context.subscriptions.push(
    vscode.commands.registerCommand("lilia.openDatabaseFile", async (fileUri?: vscode.Uri | string) => {
      let targetPath: string | undefined;

      if (typeof fileUri === "string") {
        targetPath = fileUri;
      } else if (fileUri instanceof vscode.Uri) {
        targetPath = fileUri.fsPath;
      } else {
        // QuickPick from discovered databases or file dialog
        const discovered = connectionManager.discoveredDatabases;
        const options: string[] = [...discovered, "Browse for file..."];

        const pick = await vscode.window.showQuickPick(options, {
          placeHolder: "Select a LiliaDB database or browse...",
        });

        if (!pick) return;

        if (pick === "Browse for file...") {
          const selected = await vscode.window.showOpenDialog({
            canSelectFiles: true,
            canSelectFolders: false,
            canSelectMany: false,
            filters: { "LiliaDB Databases": ["lilia", "db", "sqlite"] },
            title: "Select LiliaDB Database File",
          });
          if (selected && selected[0]) {
            targetPath = selected[0].fsPath;
          }
        } else {
          targetPath = pick;
        }
      }

      if (targetPath) {
        try {
          await connectionManager.connectEmbedded(targetPath);
        } catch (err: unknown) {
          const msg = err instanceof Error ? err.message : String(err);
          vscode.window.showErrorMessage(`Failed to open database: ${msg}`);
        }
      }
    })
  );

  // 2. Connect to Daemon
  context.subscriptions.push(
    vscode.commands.registerCommand("lilia.connectDaemon", async () => {
      const active = connectionManager.activeInfo;
      const defaultDb = active?.path || (connectionManager.discoveredDatabases[0] ?? "lilia.lilia");

      const dbPath = await vscode.window.showInputBox({
        prompt: "Enter path to database file (.lilia)",
        value: defaultDb,
      });
      if (!dbPath) return;

      const defaultEndpoint = `${dbPath}.sock`;
      const endpoint = await vscode.window.showInputBox({
        prompt: "Daemon socket endpoint or HTTP URL",
        value: defaultEndpoint,
      });
      if (!endpoint) return;

      const defaultToken = `${dbPath}.token`;
      const tokenFile = await vscode.window.showInputBox({
        prompt: "Path to authorization token file",
        value: defaultToken,
      });

      try {
        await connectionManager.connectDaemon(dbPath, endpoint, tokenFile);
      } catch (err: unknown) {
        const msg = err instanceof Error ? err.message : String(err);
        vscode.window.showErrorMessage(`Failed to connect to daemon: ${msg}`);
      }
    })
  );

  // 3. Switch Mode
  context.subscriptions.push(
    vscode.commands.registerCommand("lilia.switchMode", async () => {
      await connectionManager.switchMode();
    })
  );

  // 4. Disconnect
  context.subscriptions.push(
    vscode.commands.registerCommand("lilia.disconnect", async () => {
      await connectionManager.disconnect();
    })
  );

  // 5. Doctor (Integrity Check)
  context.subscriptions.push(
    vscode.commands.registerCommand("lilia.doctor", async () => {
      const service = connectionManager.service;
      if (!service.isConnected) {
        vscode.window.showWarningMessage("Please connect to a database first.");
        return;
      }
      try {
        vscode.window.withProgress(
          {
            location: vscode.ProgressLocation.Notification,
            title: "LiliaDB: Running integrity check...",
            cancellable: false,
          },
          async () => {
            const ok = await service.integrityCheck();
            if (ok) {
              vscode.window.showInformationMessage("LiliaDB Doctor: Database integrity check passed! (OK)");
            } else {
              vscode.window.showErrorMessage("LiliaDB Doctor: Database integrity check failed.");
            }
          }
        );
      } catch (err: unknown) {
        const msg = err instanceof Error ? err.message : String(err);
        vscode.window.showErrorMessage(`Integrity check error: ${msg}`);
      }
    })
  );

  // 6. Backup
  context.subscriptions.push(
    vscode.commands.registerCommand("lilia.backup", async () => {
      const service = connectionManager.service;
      if (!service.isConnected) {
        vscode.window.showWarningMessage("Please connect to a database first.");
        return;
      }
      const active = connectionManager.activeInfo;
      const defaultDest = active ? `${active.path}.backup-${Date.now()}.lilia` : "backup.lilia";

      const dest = await vscode.window.showInputBox({
        prompt: "Destination path for database backup",
        value: defaultDest,
      });
      if (!dest) return;

      try {
        await vscode.window.withProgress(
          {
            location: vscode.ProgressLocation.Notification,
            title: `LiliaDB: Backing up database to ${dest}...`,
            cancellable: false,
          },
          async () => {
            await service.backup(dest);
            vscode.window.showInformationMessage(`LiliaDB: Backup created successfully at ${dest}`);
          }
        );
      } catch (err: unknown) {
        const msg = err instanceof Error ? err.message : String(err);
        vscode.window.showErrorMessage(`Backup failed: ${msg}`);
      }
    })
  );
}
