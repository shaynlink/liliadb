import * as vscode from "vscode";
import type { ConnectionManager } from "../connection/ConnectionManager.js";
import type {
  FtsSummary,
  GraphSummary,
  NamespaceSummary,
  SpaceSummary,
} from "../types.js";

type TreeItemType =
  | "connectionHeader"
  | "workspaceDatabasesHeader"
  | "workspaceDatabaseItem"
  | "spacesHeader"
  | "spaceItem"
  | "spaceIndexesHeader"
  | "spaceIndexItem"
  | "spaceDocItem"
  | "namespacesHeader"
  | "namespaceItem"
  | "kvKeyItem"
  | "graphsHeader"
  | "graphItem"
  | "ftsHeader"
  | "ftsItem"
  | "toolsHeader"
  | "toolItem";

export class LiliaTreeItem extends vscode.TreeItem {
  constructor(
    label: string,
    collapsibleState: vscode.TreeItemCollapsibleState,
    public readonly itemType: TreeItemType,
    public readonly data?: unknown
  ) {
    super(label, collapsibleState);
    this.contextValue = itemType;
  }
}

export class LiliaTreeDataProvider implements vscode.TreeDataProvider<LiliaTreeItem> {
  private readonly _onDidChangeTreeData = new vscode.EventEmitter<LiliaTreeItem | undefined | void>();
  readonly onDidChangeTreeData = this._onDidChangeTreeData.event;

  constructor(private readonly connectionManager: ConnectionManager) {
    connectionManager.onDidChangeConnection(() => this.refresh());
  }

  refresh(): void {
    this._onDidChangeTreeData.fire();
  }

  getTreeItem(element: LiliaTreeItem): vscode.TreeItem {
    return element;
  }

  async getChildren(element?: LiliaTreeItem): Promise<LiliaTreeItem[]> {
    const service = this.connectionManager.service;
    const info = this.connectionManager.activeInfo;

    if (!element) {
      // Root level
      const items: LiliaTreeItem[] = [];

      // 1. Active connection
      if (info && info.connected) {
        const modeBadge = info.mode === "embedded" ? "Embedded (Direct)" : "Daemon";
        const connItem = new LiliaTreeItem(
          `Connected: ${info.databaseName} [${modeBadge}]`,
          vscode.TreeItemCollapsibleState.None,
          "connectionHeader",
          info
        );
        connItem.iconPath = new vscode.ThemeIcon(info.mode === "embedded" ? "database" : "radio-tower");
        connItem.description = info.path;
        connItem.command = {
          command: "lilia.switchMode",
          title: "Switch Mode",
        };
        items.push(connItem);
      } else {
        const disconnectedItem = new LiliaTreeItem(
          "No Database Connected",
          vscode.TreeItemCollapsibleState.None,
          "connectionHeader"
        );
        disconnectedItem.iconPath = new vscode.ThemeIcon("plug");
        disconnectedItem.description = "Click to connect";
        disconnectedItem.command = {
          command: "lilia.openDatabaseFile",
          title: "Open Database File",
        };
        items.push(disconnectedItem);
      }

      // 2. Discovered workspace databases
      const dbs = this.connectionManager.discoveredDatabases;
      const dbHeader = new LiliaTreeItem(
        `Workspace Databases (${dbs.length})`,
        dbs.length > 0 ? vscode.TreeItemCollapsibleState.Collapsed : vscode.TreeItemCollapsibleState.None,
        "workspaceDatabasesHeader"
      );
      dbHeader.iconPath = new vscode.ThemeIcon("folder-opened");
      items.push(dbHeader);

      if (service.isConnected) {
        // 3. JSON Spaces
        const spaces = await service.getSpacesSummary();
        const spacesHeader = new LiliaTreeItem(
          `JSON Spaces (${spaces.length})`,
          spaces.length > 0 ? vscode.TreeItemCollapsibleState.Expanded : vscode.TreeItemCollapsibleState.None,
          "spacesHeader",
          spaces
        );
        spacesHeader.iconPath = new vscode.ThemeIcon("file-code");
        items.push(spacesHeader);

        // 4. KV Namespaces
        const namespaces = await service.getNamespacesSummary();
        const namespacesHeader = new LiliaTreeItem(
          `Key-Value Stores (${namespaces.length})`,
          namespaces.length > 0 ? vscode.TreeItemCollapsibleState.Collapsed : vscode.TreeItemCollapsibleState.None,
          "namespacesHeader",
          namespaces
        );
        namespacesHeader.iconPath = new vscode.ThemeIcon("symbol-key");
        items.push(namespacesHeader);

        // 5. Graphs
        const graphs = await service.getGraphsSummary();
        const graphsHeader = new LiliaTreeItem(
          `Graphs (${graphs.length})`,
          graphs.length > 0 ? vscode.TreeItemCollapsibleState.Collapsed : vscode.TreeItemCollapsibleState.None,
          "graphsHeader",
          graphs
        );
        graphsHeader.iconPath = new vscode.ThemeIcon("type-hierarchy");
        items.push(graphsHeader);

        // 6. FTS
        const fts = await service.getFtsSummary();
        const ftsHeader = new LiliaTreeItem(
          `Full-Text Search (${fts.length})`,
          fts.length > 0 ? vscode.TreeItemCollapsibleState.Collapsed : vscode.TreeItemCollapsibleState.None,
          "ftsHeader",
          fts
        );
        ftsHeader.iconPath = new vscode.ThemeIcon("search");
        items.push(ftsHeader);

        // 7. Tools
        const toolsHeader = new LiliaTreeItem(
          "Tools & Actions",
          vscode.TreeItemCollapsibleState.Expanded,
          "toolsHeader"
        );
        toolsHeader.iconPath = new vscode.ThemeIcon("tools");
        items.push(toolsHeader);
      }

      return items;
    }

    // Children of headers
    switch (element.itemType) {
      case "workspaceDatabasesHeader": {
        const dbs = this.connectionManager.discoveredDatabases;
        return dbs.map(dbPath => {
          const name = dbPath.split("/").pop() || dbPath;
          const isActive = this.connectionManager.activeInfo?.path === dbPath;
          const item = new LiliaTreeItem(
            name,
            vscode.TreeItemCollapsibleState.None,
            "workspaceDatabaseItem",
            dbPath
          );
          item.description = isActive ? "(Active)" : dbPath;
          item.iconPath = new vscode.ThemeIcon(isActive ? "pass-filled" : "database");
          item.command = {
            command: "lilia.openDatabaseFile",
            title: "Connect",
            arguments: [dbPath],
          };
          return item;
        });
      }

      case "spacesHeader": {
        const spaces = (element.data as SpaceSummary[]) || (await service.getSpacesSummary());
        return spaces.map(s => {
          const item = new LiliaTreeItem(
            s.space,
            vscode.TreeItemCollapsibleState.Collapsed,
            "spaceItem",
            s
          );
          item.description = `${s.count} doc${s.count === 1 ? "" : "s"}, ${s.indexes.length} idx`;
          item.iconPath = new vscode.ThemeIcon("json");
          item.command = {
            command: "lilia.openVisualizer",
            title: "View Space",
            arguments: [{ model: "json", space: s.space }],
          };
          return item;
        });
      }

      case "spaceItem": {
        const space = element.data as SpaceSummary;
        const items: LiliaTreeItem[] = [];

        // Secondary Indexes sub-branch
        if (space.indexes.length > 0) {
          const idxHeader = new LiliaTreeItem(
            `Indexes (${space.indexes.length})`,
            vscode.TreeItemCollapsibleState.Collapsed,
            "spaceIndexesHeader",
            space.indexes
          );
          idxHeader.iconPath = new vscode.ThemeIcon("list-tree");
          items.push(idxHeader);
        }

        // Preview documents
        try {
          const docs = await service.jsonScan(space.space, undefined, 20);
          for (const doc of docs) {
            const preview = JSON.stringify(doc.value);
            const truncated = preview.length > 30 ? preview.slice(0, 30) + "..." : preview;
            const docItem = new LiliaTreeItem(
              `ID: ${doc.id} (v${doc.version})`,
              vscode.TreeItemCollapsibleState.None,
              "spaceDocItem",
              { space: space.space, doc }
            );
            docItem.description = truncated;
            docItem.iconPath = new vscode.ThemeIcon("file");
            docItem.command = {
              command: "lilia.openVisualizer",
              title: "Open Document",
              arguments: [{ model: "json", space: space.space, id: doc.id }],
            };
            items.push(docItem);
          }
        } catch (err) {
          console.error("Failed to load documents for space:", err);
        }

        return items;
      }

      case "spaceIndexesHeader": {
        const indexes = (element.data as SpaceSummary["indexes"]) || [];
        return indexes.map(idx => {
          const item = new LiliaTreeItem(
            idx.indexName,
            vscode.TreeItemCollapsibleState.None,
            "spaceIndexItem",
            idx
          );
          item.description = idx.jsonPath;
          item.iconPath = new vscode.ThemeIcon("tag");
          return item;
        });
      }

      case "namespacesHeader": {
        const namespaces = (element.data as NamespaceSummary[]) || (await service.getNamespacesSummary());
        return namespaces.map(ns => {
          const item = new LiliaTreeItem(
            ns.namespace,
            vscode.TreeItemCollapsibleState.Collapsed,
            "namespaceItem",
            ns
          );
          item.description = `${ns.count} item${ns.count === 1 ? "" : "s"}`;
          item.iconPath = new vscode.ThemeIcon("key");
          item.command = {
            command: "lilia.openVisualizer",
            title: "View Namespace",
            arguments: [{ model: "kv", namespace: ns.namespace }],
          };
          return item;
        });
      }

      case "namespaceItem": {
        const ns = element.data as NamespaceSummary;
        try {
          const entries = await service.kvScan(ns.namespace, undefined, 20);
          return entries.map(entry => {
            const keyStr = Buffer.from(entry.key).toString("utf8");
            const item = new LiliaTreeItem(
              keyStr,
              vscode.TreeItemCollapsibleState.None,
              "kvKeyItem",
              { namespace: ns.namespace, entry }
            );
            item.description = `v${entry.version}`;
            item.iconPath = new vscode.ThemeIcon("symbol-field");
            item.command = {
              command: "lilia.openVisualizer",
              title: "View Entry",
              arguments: [{ model: "kv", namespace: ns.namespace, key: keyStr }],
            };
            return item;
          });
        } catch {
          return [];
        }
      }

      case "graphsHeader": {
        const graphs = (element.data as GraphSummary[]) || (await service.getGraphsSummary());
        return graphs.map(g => {
          const item = new LiliaTreeItem(
            g.graph,
            vscode.TreeItemCollapsibleState.None,
            "graphItem",
            g
          );
          item.description = `${g.nodeCount} nodes, ${g.edgeCount} edges`;
          item.iconPath = new vscode.ThemeIcon("git-merge");
          item.command = {
            command: "lilia.openVisualizer",
            title: "View Graph",
            arguments: [{ model: "graph", graph: g.graph }],
          };
          return item;
        });
      }

      case "ftsHeader": {
        const ftsList = (element.data as FtsSummary[]) || (await service.getFtsSummary());
        return ftsList.map(fts => {
          const item = new LiliaTreeItem(
            fts.collection,
            vscode.TreeItemCollapsibleState.None,
            "ftsItem",
            fts
          );
          item.description = `${fts.count} docs`;
          item.iconPath = new vscode.ThemeIcon("search");
          item.command = {
            command: "lilia.openVisualizer",
            title: "Search FTS Collection",
            arguments: [{ model: "fts", collection: fts.collection }],
          };
          return item;
        });
      }

      case "toolsHeader": {
        const items: LiliaTreeItem[] = [];

        const visualizer = new LiliaTreeItem("Data Visualizer", vscode.TreeItemCollapsibleState.None, "toolItem");
        visualizer.iconPath = new vscode.ThemeIcon("table");
        visualizer.command = { command: "lilia.openVisualizer", title: "Open Data Visualizer" };
        items.push(visualizer);

        const consoleItem = new LiliaTreeItem("Command & Query Console", vscode.TreeItemCollapsibleState.None, "toolItem");
        consoleItem.iconPath = new vscode.ThemeIcon("terminal");
        consoleItem.command = { command: "lilia.openConsole", title: "Open Command & Query Console" };
        items.push(consoleItem);

        const doctor = new LiliaTreeItem("Run Integrity Check (Doctor)", vscode.TreeItemCollapsibleState.None, "toolItem");
        doctor.iconPath = new vscode.ThemeIcon("pass");
        doctor.command = { command: "lilia.doctor", title: "Run Integrity Check" };
        items.push(doctor);

        const backup = new LiliaTreeItem("Create Backup...", vscode.TreeItemCollapsibleState.None, "toolItem");
        backup.iconPath = new vscode.ThemeIcon("archive");
        backup.command = { command: "lilia.backup", title: "Create Backup" };
        items.push(backup);

        return items;
      }

      default:
        return [];
    }
  }
}
