import * as vscode from "vscode";
import type { ConnectionManager } from "../connection/ConnectionManager.js";
import { VisualizerPanel } from "../webviews/VisualizerPanel.js";

export function registerDataCommands(
  context: vscode.ExtensionContext,
  connectionManager: ConnectionManager
): void {
  // New JSON Document
  context.subscriptions.push(
    vscode.commands.registerCommand("lilia.newJson", async (spaceArg?: { space?: string } | string) => {
      const service = connectionManager.service;
      if (!service.isConnected) {
        vscode.window.showWarningMessage("Please connect to a database first.");
        return;
      }

      let space = typeof spaceArg === "string" ? spaceArg : spaceArg?.space;
      if (!space) {
        const spaces = await service.getSpacesSummary();
        const items = spaces.map(s => s.space);
        const pick = await vscode.window.showQuickPick([...items, "+ New Space..."], {
          placeHolder: "Select space for new document",
        });
        if (!pick) return;
        if (pick === "+ New Space...") {
          space = await vscode.window.showInputBox({ prompt: "Enter space name" });
        } else {
          space = pick;
        }
      }
      if (!space) return;

      const id = await vscode.window.showInputBox({
        prompt: `Enter document ID for space "${space}"`,
        placeHolder: "e.g. doc_1",
      });
      if (!id) return;

      const rawJson = await vscode.window.showInputBox({
        prompt: "Initial JSON payload",
        value: '{"title": "New Document"}',
      });
      if (!rawJson) return;

      try {
        const parsed = JSON.parse(rawJson);
        const res = await service.jsonPut(space, id, parsed);
        vscode.window.showInformationMessage(`LiliaDB: Document "${id}" created (v${res.version}) in space "${space}"`);
        // Reveal in visualizer
        VisualizerPanel.createOrShow(context.extensionUri, connectionManager, {
          model: "json",
          space,
          id,
        });
      } catch (err: unknown) {
        const msg = err instanceof Error ? err.message : String(err);
        vscode.window.showErrorMessage(`Failed to create document: ${msg}`);
      }
    })
  );

  // New KV Entry
  context.subscriptions.push(
    vscode.commands.registerCommand("lilia.newKv", async (nsArg?: { namespace?: string } | string) => {
      const service = connectionManager.service;
      if (!service.isConnected) {
        vscode.window.showWarningMessage("Please connect to a database first.");
        return;
      }

      let namespace = typeof nsArg === "string" ? nsArg : nsArg?.namespace;
      if (!namespace) {
        const namespaces = await service.getNamespacesSummary();
        const items = namespaces.map(n => n.namespace);
        const pick = await vscode.window.showQuickPick([...items, "+ New Namespace..."], {
          placeHolder: "Select namespace for new KV pair",
        });
        if (!pick) return;
        if (pick === "+ New Namespace...") {
          namespace = await vscode.window.showInputBox({ prompt: "Enter namespace name" });
        } else {
          namespace = pick;
        }
      }
      if (!namespace) return;

      const key = await vscode.window.showInputBox({
        prompt: `Enter key for namespace "${namespace}"`,
        placeHolder: "key_name",
      });
      if (!key) return;

      const val = await vscode.window.showInputBox({
        prompt: "Enter value",
        value: "value_data",
      });
      if (val === undefined) return;

      try {
        const res = await service.kvSet(namespace, key, val);
        vscode.window.showInformationMessage(`LiliaDB: Key "${key}" set (v${res.version}) in namespace "${namespace}"`);
        VisualizerPanel.createOrShow(context.extensionUri, connectionManager, {
          model: "kv",
          namespace,
          key,
        });
      } catch (err: unknown) {
        const msg = err instanceof Error ? err.message : String(err);
        vscode.window.showErrorMessage(`Failed to set KV entry: ${msg}`);
      }
    })
  );
}
