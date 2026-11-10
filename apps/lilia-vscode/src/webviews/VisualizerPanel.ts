import * as vscode from "vscode";
import type { ConnectionManager } from "../connection/ConnectionManager.js";
import type { WebviewMessage } from "../types.js";

export class VisualizerPanel {
  public static currentPanel: VisualizerPanel | undefined;
  private readonly _panel: vscode.WebviewPanel;
  private _disposables: vscode.Disposable[] = [];

  public static createOrShow(
    extensionUri: vscode.Uri,
    connectionManager: ConnectionManager,
    initialTarget?: { model: string; space?: string; namespace?: string; graph?: string; collection?: string; id?: string; key?: string }
  ): VisualizerPanel {
    const column = vscode.window.activeTextEditor
      ? vscode.window.activeTextEditor.viewColumn
      : undefined;

    if (VisualizerPanel.currentPanel) {
      VisualizerPanel.currentPanel._panel.reveal(column);
      if (initialTarget) {
        VisualizerPanel.currentPanel.handleInitialTarget(initialTarget);
      }
      return VisualizerPanel.currentPanel;
    }

    const panel = vscode.window.createWebviewPanel(
      "liliaVisualizer",
      "LiliaDB Data Visualizer",
      column || vscode.ViewColumn.One,
      {
        enableScripts: true,
        retainContextWhenHidden: true,
        localResourceRoots: [extensionUri],
      }
    );

    VisualizerPanel.currentPanel = new VisualizerPanel(panel, extensionUri, connectionManager, initialTarget);
    return VisualizerPanel.currentPanel;
  }

  private constructor(
    panel: vscode.WebviewPanel,
    private readonly _extensionUri: vscode.Uri,
    private readonly _connectionManager: ConnectionManager,
    private readonly _initialTarget?: { model: string; space?: string; namespace?: string; graph?: string; collection?: string; id?: string; key?: string }
  ) {
    this._panel = panel;
    this._panel.onDidDispose(() => this.dispose(), null, this._disposables);
    this._panel.webview.html = this._getHtmlForWebview();

    this._panel.webview.onDidReceiveMessage(
      async (message: WebviewMessage) => {
        await this._handleMessage(message);
      },
      null,
      this._disposables
    );

    this._connectionManager.onDidChangeConnection(() => {
      this._sendInitialState();
    });
  }

  public handleInitialTarget(target: { model: string; space?: string; namespace?: string; graph?: string; collection?: string; id?: string; key?: string }): void {
    this._panel.webview.postMessage({
      type: "setTarget",
      target,
    });
  }

  private async _sendInitialState(): Promise<void> {
    const service = this._connectionManager.service;
    if (!service.isConnected) {
      this._panel.webview.postMessage({
        type: "state",
        connected: false,
      });
      return;
    }

    try {
      const [spaces, namespaces, graphs, fts] = await Promise.all([
        service.getSpacesSummary(),
        service.getNamespacesSummary(),
        service.getGraphsSummary(),
        service.getFtsSummary(),
      ]);

      this._panel.webview.postMessage({
        type: "state",
        connected: true,
        info: this._connectionManager.activeInfo,
        spaces,
        namespaces,
        graphs,
        fts,
        initialTarget: this._initialTarget,
      });
    } catch (err: unknown) {
      const msg = err instanceof Error ? err.message : String(err);
      this._panel.webview.postMessage({
        type: "error",
        message: `Failed to load database overview: ${msg}`,
      });
    }
  }

  private async _handleMessage(message: WebviewMessage): Promise<void> {
    const service = this._connectionManager.service;

    switch (message.type) {
      case "ready":
      case "refresh":
        await this._sendInitialState();
        break;

      case "loadJsonPage": {
        try {
          const docs = await service.jsonScan(message.space, message.after, message.limit ?? 50);
          this._panel.webview.postMessage({
            type: "jsonPageLoaded",
            space: message.space,
            docs: docs.map(d => ({ ...d, version: d.version.toString() })),
          });
        } catch (err: unknown) {
          const msg = err instanceof Error ? err.message : String(err);
          this._panel.webview.postMessage({ type: "error", message: msg });
        }
        break;
      }

      case "putJson": {
        try {
          const ifVer = message.ifVersion ? BigInt(message.ifVersion) : undefined;
          const res = await service.jsonPut(message.space, message.id, message.value, { ifVersion: ifVer });
          vscode.window.showInformationMessage(`Document ${message.id} saved (v${res.version})`);
          this._panel.webview.postMessage({
            type: "mutationSuccess",
            space: message.space,
            id: message.id,
            version: res.version?.toString(),
          });
        } catch (err: unknown) {
          const msg = err instanceof Error ? err.message : String(err);
          vscode.window.showErrorMessage(`Failed to save document: ${msg}`);
          this._panel.webview.postMessage({ type: "error", message: msg });
        }
        break;
      }

      case "deleteJson": {
        try {
          const ifVer = message.ifVersion ? BigInt(message.ifVersion) : undefined;
          await service.jsonDelete(message.space, message.id, { ifVersion: ifVer });
          vscode.window.showInformationMessage(`Document ${message.id} deleted`);
          this._panel.webview.postMessage({
            type: "deleteSuccess",
            space: message.space,
            id: message.id,
          });
        } catch (err: unknown) {
          const msg = err instanceof Error ? err.message : String(err);
          vscode.window.showErrorMessage(`Failed to delete document: ${msg}`);
          this._panel.webview.postMessage({ type: "error", message: msg });
        }
        break;
      }

      case "loadKvPage": {
        try {
          const entries = await service.kvScan(message.namespace, message.after, message.limit ?? 50);
          this._panel.webview.postMessage({
            type: "kvPageLoaded",
            namespace: message.namespace,
            entries: entries.map(e => ({
              namespace: e.namespace,
              key: Buffer.from(e.key).toString("utf8"),
              value: Buffer.from(e.value).toString("utf8"),
              version: e.version.toString(),
              expiresAtMs: e.expiresAtMs,
            })),
          });
        } catch (err: unknown) {
          const msg = err instanceof Error ? err.message : String(err);
          this._panel.webview.postMessage({ type: "error", message: msg });
        }
        break;
      }

      case "setKv": {
        try {
          const ifVer = message.ifVersion ? BigInt(message.ifVersion) : undefined;
          const res = await service.kvSet(message.namespace, message.key, message.value, {
            ifVersion: ifVer,
            expiresAtMs: message.expiresAtMs,
          });
          vscode.window.showInformationMessage(`KV key "${message.key}" saved (v${res.version})`);
          this._panel.webview.postMessage({
            type: "kvSavedSuccess",
            namespace: message.namespace,
            key: message.key,
            version: res.version?.toString(),
          });
        } catch (err: unknown) {
          const msg = err instanceof Error ? err.message : String(err);
          vscode.window.showErrorMessage(`Failed to save KV entry: ${msg}`);
          this._panel.webview.postMessage({ type: "error", message: msg });
        }
        break;
      }

      case "deleteKv": {
        try {
          const ifVer = message.ifVersion ? BigInt(message.ifVersion) : undefined;
          await service.kvDelete(message.namespace, message.key, { ifVersion: ifVer });
          vscode.window.showInformationMessage(`KV key "${message.key}" deleted`);
          this._panel.webview.postMessage({
            type: "kvDeletedSuccess",
            namespace: message.namespace,
            key: message.key,
          });
        } catch (err: unknown) {
          const msg = err instanceof Error ? err.message : String(err);
          vscode.window.showErrorMessage(`Failed to delete KV entry: ${msg}`);
          this._panel.webview.postMessage({ type: "error", message: msg });
        }
        break;
      }

      case "searchFts": {
        try {
          const hits = await service.ftsSearch(message.collection, message.query, message.limit ?? 20);
          this._panel.webview.postMessage({
            type: "ftsResults",
            collection: message.collection,
            query: message.query,
            hits,
          });
        } catch (err: unknown) {
          const msg = err instanceof Error ? err.message : String(err);
          this._panel.webview.postMessage({ type: "error", message: msg });
        }
        break;
      }

      case "graphTraverse": {
        try {
          const neighbors = await service.graphNeighbors(message.graph, message.node, message.direction, message.kind);
          this._panel.webview.postMessage({
            type: "graphTraverseResult",
            graph: message.graph,
            node: message.node,
            neighbors,
          });
        } catch (err: unknown) {
          const msg = err instanceof Error ? err.message : String(err);
          this._panel.webview.postMessage({ type: "error", message: msg });
        }
        break;
      }
    }
  }

  public dispose(): void {
    VisualizerPanel.currentPanel = undefined;
    this._panel.dispose();
    for (const d of this._disposables) {
      d.dispose();
    }
    this._disposables = [];
  }

  private _getHtmlForWebview(): string {
    return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>LiliaDB Visualizer</title>
  <style>
    :root {
      --bg: var(--vscode-editor-background);
      --fg: var(--vscode-editor-foreground);
      --border: var(--vscode-panel-border, #3c3c3c);
      --accent: var(--vscode-button-background, #007acc);
      --accent-hover: var(--vscode-button-hoverBackground, #0062a3);
      --badge-bg: var(--vscode-badge-background, #4d4d4d);
      --input-bg: var(--vscode-input-background, #252526);
      --input-fg: var(--vscode-input-foreground, #cccccc);
      --card-bg: var(--vscode-editorWidget-background, #252526);
    }
    body {
      font-family: var(--vscode-font-family, -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, Helvetica, Arial, sans-serif);
      margin: 0;
      padding: 0;
      background: var(--bg);
      color: var(--fg);
      display: flex;
      flex-direction: column;
      height: 100vh;
      overflow: hidden;
    }
    header {
      padding: 10px 16px;
      border-bottom: 1px solid var(--border);
      display: flex;
      align-items: center;
      justify-content: space-between;
      gap: 12px;
      background: var(--card-bg);
    }
    .tabs {
      display: flex;
      gap: 6px;
    }
    .tab-btn {
      background: transparent;
      border: 1px solid transparent;
      color: var(--fg);
      padding: 6px 12px;
      cursor: pointer;
      border-radius: 4px;
      font-weight: 500;
      font-size: 13px;
    }
    .tab-btn.active {
      background: var(--accent);
      color: #fff;
    }
    .controls {
      display: flex;
      align-items: center;
      gap: 8px;
    }
    select, input, button, textarea {
      background: var(--input-bg);
      color: var(--input-fg);
      border: 1px solid var(--border);
      border-radius: 4px;
      padding: 6px 10px;
      font-size: 13px;
    }
    button.primary {
      background: var(--accent);
      color: #fff;
      border: none;
      cursor: pointer;
    }
    button.primary:hover {
      background: var(--accent-hover);
    }
    .content-area {
      flex: 1;
      display: flex;
      overflow: hidden;
    }
    .main-view {
      flex: 1;
      display: flex;
      flex-direction: column;
      overflow-y: auto;
      padding: 16px;
    }
    .inspector-panel {
      width: 420px;
      border-left: 1px solid var(--border);
      display: flex;
      flex-direction: column;
      padding: 16px;
      background: var(--card-bg);
      overflow-y: auto;
    }
    table {
      width: 100%;
      border-collapse: collapse;
      font-size: 13px;
    }
    th, td {
      border: 1px solid var(--border);
      padding: 8px 12px;
      text-align: left;
    }
    th {
      background: var(--card-bg);
      position: sticky;
      top: 0;
    }
    tr:hover {
      background: rgba(255, 255, 255, 0.05);
      cursor: pointer;
    }
    tr.selected {
      background: rgba(0, 122, 204, 0.25);
    }
    .badge {
      display: inline-block;
      padding: 2px 6px;
      border-radius: 10px;
      font-size: 11px;
      background: var(--badge-bg);
      color: #fff;
    }
    .badge-v {
      background: #007acc;
    }
    .json-code {
      font-family: var(--vscode-editor-font-family, monospace);
      font-size: 12px;
      white-space: pre-wrap;
      word-break: break-all;
    }
    .hidden {
      display: none !important;
    }
    .snippet b {
      background: rgba(255, 215, 0, 0.35);
      color: #ffeb3b;
      padding: 0 2px;
    }
    .btn-row {
      display: flex;
      gap: 8px;
      margin-top: 12px;
    }
    .btn-danger {
      background: #d9534f;
      color: #fff;
      border: none;
      cursor: pointer;
    }
  </style>
</head>
<body>
  <header>
    <div class="tabs">
      <button class="tab-btn active" id="tab-json" onclick="switchTab('json')">📄 JSON Documents</button>
      <button class="tab-btn" id="tab-kv" onclick="switchTab('kv')">🔑 Key-Value</button>
      <button class="tab-btn" id="tab-graph" onclick="switchTab('graph')">🕸️ Graphs</button>
      <button class="tab-btn" id="tab-fts" onclick="switchTab('fts')">🔍 Full-Text Search</button>
    </div>
    <div class="controls">
      <span id="conn-badge" class="badge">Checking...</span>
      <button onclick="refreshData()">🔄 Refresh</button>
    </div>
  </header>

  <div class="content-area">
    <div class="main-view">
      <!-- JSON Tab Content -->
      <div id="view-json">
        <div style="display: flex; justify-content: space-between; margin-bottom: 12px;">
          <div style="display: flex; gap: 8px; align-items: center;">
            <label for="select-space"><strong>Space:</strong></label>
            <select id="select-space" onchange="onSpaceChanged()"></select>
            <input type="text" id="json-filter" placeholder="Filter in table..." oninput="filterJsonTable()">
          </div>
          <button class="primary" onclick="openNewDocModal()">+ New Document</button>
        </div>
        <div id="json-table-container">
          <p>Select a space to load documents.</p>
        </div>
      </div>

      <!-- KV Tab Content -->
      <div id="view-kv" class="hidden">
        <div style="display: flex; justify-content: space-between; margin-bottom: 12px;">
          <div style="display: flex; gap: 8px; align-items: center;">
            <label for="select-namespace"><strong>Namespace:</strong></label>
            <select id="select-namespace" onchange="onNamespaceChanged()"></select>
            <input type="text" id="kv-filter" placeholder="Filter keys..." oninput="filterKvTable()">
          </div>
          <button class="primary" onclick="openNewKvModal()">+ New KV Entry</button>
        </div>
        <div id="kv-table-container">
          <p>Select a namespace to load entries.</p>
        </div>
      </div>

      <!-- Graph Tab Content -->
      <div id="view-graph" class="hidden">
        <div style="display: flex; gap: 8px; margin-bottom: 12px; align-items: center;">
          <label for="select-graph"><strong>Graph:</strong></label>
          <select id="select-graph"></select>
          <input type="text" id="graph-node-input" placeholder="Node ID...">
          <select id="graph-dir">
            <option value="outgoing">Outgoing</option>
            <option value="incoming">Incoming</option>
            <option value="both">Both</option>
          </select>
          <button class="primary" onclick="traverseGraph()">Traverse Neighbors</button>
        </div>
        <div id="graph-results-container">
          <p>Select a graph and enter a node to inspect neighbors.</p>
        </div>
      </div>

      <!-- FTS Tab Content -->
      <div id="view-fts" class="hidden">
        <div style="display: flex; gap: 8px; margin-bottom: 12px; align-items: center;">
          <label for="select-fts"><strong>Collection:</strong></label>
          <select id="select-fts"></select>
          <input type="text" id="fts-query-input" placeholder="Search query (e.g. piece OR sqlite)..." style="flex: 1;">
          <button class="primary" onclick="executeFtsSearch()">Search</button>
        </div>
        <div id="fts-results-container">
          <p>Run a search query to view highlighted snippet matches.</p>
        </div>
      </div>
    </div>

    <!-- Inspector / Editor Panel -->
    <div class="inspector-panel" id="inspector">
      <h3 id="inspector-title">Record Inspector</h3>
      <div id="inspector-body">
        <p style="color: #888;">Select a record in the table to inspect and edit its contents.</p>
      </div>
    </div>
  </div>

  <script>
    const vscode = acquireVsCodeApi();
    let currentTab = 'json';
    let appState = { spaces: [], namespaces: [], graphs: [], fts: [], currentDocs: [], currentKv: [] };
    let selectedRecord = null;

    vscode.postMessage({ type: 'ready' });

    window.addEventListener('message', event => {
      const msg = event.data;
      switch (msg.type) {
        case 'state':
          updateState(msg);
          break;
        case 'jsonPageLoaded':
          appState.currentDocs = msg.docs;
          renderJsonTable(msg.docs);
          break;
        case 'kvPageLoaded':
          appState.currentKv = msg.entries;
          renderKvTable(msg.entries);
          break;
        case 'ftsResults':
          renderFtsResults(msg.hits);
          break;
        case 'graphTraverseResult':
          renderGraphResults(msg.neighbors);
          break;
        case 'setTarget':
          if (msg.target.model) switchTab(msg.target.model);
          break;
      }
    });

    function updateState(msg) {
      const badge = document.getElementById('conn-badge');
      if (!msg.connected) {
        badge.innerText = 'Disconnected';
        badge.style.background = '#d9534f';
        return;
      }
      badge.innerText = msg.info.mode === 'embedded' ? 'Embedded Direct' : 'Daemon';
      badge.style.background = '#28a745';

      appState.spaces = msg.spaces || [];
      appState.namespaces = msg.namespaces || [];
      appState.graphs = msg.graphs || [];
      appState.fts = msg.fts || [];

      // Populate selects
      populateSelect('select-space', appState.spaces.map(s => s.space));
      populateSelect('select-namespace', appState.namespaces.map(n => n.namespace));
      populateSelect('select-graph', appState.graphs.map(g => g.graph));
      populateSelect('select-fts', appState.fts.map(f => f.collection));

      if (appState.spaces.length > 0) onSpaceChanged();
      if (appState.namespaces.length > 0) onNamespaceChanged();
    }

    function populateSelect(id, items) {
      const el = document.getElementById(id);
      el.innerHTML = items.map(item => \`<option value="\${item}">\${item}</option>\`).join('');
    }

    function switchTab(tab) {
      currentTab = tab;
      ['json', 'kv', 'graph', 'fts'].forEach(t => {
        document.getElementById('tab-' + t).classList.toggle('active', t === tab);
        document.getElementById('view-' + t).classList.toggle('hidden', t !== tab);
      });
      clearInspector();
    }

    function onSpaceChanged() {
      const space = document.getElementById('select-space').value;
      if (!space) return;
      vscode.postMessage({ type: 'loadJsonPage', space });
    }

    function onNamespaceChanged() {
      const namespace = document.getElementById('select-namespace').value;
      if (!namespace) return;
      vscode.postMessage({ type: 'loadKvPage', namespace });
    }

    function refreshData() {
      vscode.postMessage({ type: 'refresh' });
    }

    function renderJsonTable(docs) {
      const container = document.getElementById('json-table-container');
      if (!docs || docs.length === 0) {
        container.innerHTML = '<p>(Space is empty)</p>';
        return;
      }
      let html = '<table><thead><tr><th>ID</th><th>Version</th><th>Preview</th><th>Actions</th></tr></thead><tbody>';
      docs.forEach((doc, idx) => {
        const preview = JSON.stringify(doc.value);
        const truncated = preview.length > 80 ? preview.slice(0, 80) + '...' : preview;
        html += \`<tr onclick="selectJsonDoc(\${idx})">
          <td><strong>\${doc.id}</strong></td>
          <td><span class="badge badge-v">v\${doc.version}</span></td>
          <td class="json-code">\${escapeHtml(truncated)}</td>
          <td><button onclick="event.stopPropagation(); selectJsonDoc(\${idx})">Edit</button></td>
        </tr>\`;
      });
      html += '</tbody></table>';
      container.innerHTML = html;
    }

    function selectJsonDoc(idx) {
      const doc = appState.currentDocs[idx];
      if (!doc) return;
      selectedRecord = { type: 'json', doc };
      const space = document.getElementById('select-space').value;

      const inspector = document.getElementById('inspector-body');
      inspector.innerHTML = \`
        <p><strong>Space:</strong> \${space}</p>
        <p><strong>ID:</strong> \${doc.id} <span class="badge badge-v">v\${doc.version}</span></p>
        <label><strong>Document JSON:</strong></label>
        <textarea id="edit-json-val" rows="18" style="width: 100%; font-family: monospace;">\${escapeHtml(JSON.stringify(doc.value, null, 2))}</textarea>
        <div class="btn-row">
          <button class="primary" onclick="saveSelectedJson()">Save Changes</button>
          <button class="btn-danger" onclick="deleteSelectedJson()">Delete</button>
        </div>
      \`;
    }

    function saveSelectedJson() {
      if (!selectedRecord || selectedRecord.type !== 'json') return;
      const space = document.getElementById('select-space').value;
      const text = document.getElementById('edit-json-val').value;
      try {
        const val = JSON.parse(text);
        vscode.postMessage({
          type: 'putJson',
          space,
          id: selectedRecord.doc.id,
          value: val,
          ifVersion: selectedRecord.doc.version
        });
      } catch (err) {
        alert('Invalid JSON: ' + err.message);
      }
    }

    function deleteSelectedJson() {
      if (!selectedRecord || selectedRecord.type !== 'json') return;
      if (!confirm('Are you sure you want to delete document "' + selectedRecord.doc.id + '"?')) return;
      const space = document.getElementById('select-space').value;
      vscode.postMessage({
        type: 'deleteJson',
        space,
        id: selectedRecord.doc.id,
        ifVersion: selectedRecord.doc.version
      });
      clearInspector();
    }

    function openNewDocModal() {
      const space = document.getElementById('select-space').value;
      const inspector = document.getElementById('inspector-body');
      inspector.innerHTML = \`
        <h4>Create New Document</h4>
        <p><strong>Space:</strong> \${space}</p>
        <label><strong>Document ID:</strong></label>
        <input type="text" id="new-doc-id" placeholder="e.g. user_123" style="width: 100%; margin-bottom: 8px;">
        <label><strong>JSON Payload:</strong></label>
        <textarea id="new-doc-val" rows="14" style="width: 100%; font-family: monospace;">{\\n  "title": "New Document"\\n}</textarea>
        <div class="btn-row">
          <button class="primary" onclick="createDoc()">Create Document</button>
          <button onclick="clearInspector()">Cancel</button>
        </div>
      \`;
    }

    function createDoc() {
      const space = document.getElementById('select-space').value;
      const id = document.getElementById('new-doc-id').value.trim();
      const text = document.getElementById('new-doc-val').value;
      if (!id) {
        alert('Please provide a document ID.');
        return;
      }
      try {
        const value = JSON.parse(text);
        vscode.postMessage({ type: 'putJson', space, id, value });
        clearInspector();
      } catch (err) {
        alert('Invalid JSON: ' + err.message);
      }
    }

    function renderKvTable(entries) {
      const container = document.getElementById('kv-table-container');
      if (!entries || entries.length === 0) {
        container.innerHTML = '<p>(Namespace is empty)</p>';
        return;
      }
      let html = '<table><thead><tr><th>Key</th><th>Version</th><th>Value</th><th>TTL Expiry</th><th>Actions</th></tr></thead><tbody>';
      entries.forEach((e, idx) => {
        html += \`<tr onclick="selectKvEntry(\${idx})">
          <td><strong>\${escapeHtml(e.key)}</strong></td>
          <td><span class="badge badge-v">v\${e.version}</span></td>
          <td class="json-code">\${escapeHtml(e.value.slice(0, 80))}</td>
          <td>\${e.expiresAtMs ? new Date(e.expiresAtMs).toLocaleTimeString() : 'None'}</td>
          <td><button onclick="event.stopPropagation(); selectKvEntry(\${idx})">Edit</button></td>
        </tr>\`;
      });
      html += '</tbody></table>';
      container.innerHTML = html;
    }

    function selectKvEntry(idx) {
      const entry = appState.currentKv[idx];
      if (!entry) return;
      selectedRecord = { type: 'kv', entry };
      const ns = document.getElementById('select-namespace').value;

      const inspector = document.getElementById('inspector-body');
      inspector.innerHTML = \`
        <p><strong>Namespace:</strong> \${ns}</p>
        <p><strong>Key:</strong> \${escapeHtml(entry.key)} <span class="badge badge-v">v\${entry.version}</span></p>
        <label><strong>Value:</strong></label>
        <textarea id="edit-kv-val" rows="12" style="width: 100%; font-family: monospace;">\${escapeHtml(entry.value)}</textarea>
        <div class="btn-row">
          <button class="primary" onclick="saveSelectedKv()">Save Changes</button>
          <button class="btn-danger" onclick="deleteSelectedKv()">Delete</button>
        </div>
      \`;
    }

    function saveSelectedKv() {
      if (!selectedRecord || selectedRecord.type !== 'kv') return;
      const ns = document.getElementById('select-namespace').value;
      const val = document.getElementById('edit-kv-val').value;
      vscode.postMessage({
        type: 'setKv',
        namespace: ns,
        key: selectedRecord.entry.key,
        value: val,
        ifVersion: selectedRecord.entry.version
      });
    }

    function deleteSelectedKv() {
      if (!selectedRecord || selectedRecord.type !== 'kv') return;
      if (!confirm('Delete KV key "' + selectedRecord.entry.key + '"?')) return;
      const ns = document.getElementById('select-namespace').value;
      vscode.postMessage({
        type: 'deleteKv',
        namespace: ns,
        key: selectedRecord.entry.key,
        ifVersion: selectedRecord.entry.version
      });
      clearInspector();
    }

    function openNewKvModal() {
      const ns = document.getElementById('select-namespace').value;
      const inspector = document.getElementById('inspector-body');
      inspector.innerHTML = \`
        <h4>Create New KV Entry</h4>
        <p><strong>Namespace:</strong> \${ns}</p>
        <label><strong>Key:</strong></label>
        <input type="text" id="new-kv-key" placeholder="key_name" style="width: 100%; margin-bottom: 8px;">
        <label><strong>Value:</strong></label>
        <textarea id="new-kv-val" rows="10" style="width: 100%; font-family: monospace;"></textarea>
        <div class="btn-row">
          <button class="primary" onclick="createKv()">Set Entry</button>
          <button onclick="clearInspector()">Cancel</button>
        </div>
      \`;
    }

    function createKv() {
      const ns = document.getElementById('select-namespace').value;
      const key = document.getElementById('new-kv-key').value.trim();
      const value = document.getElementById('new-kv-val').value;
      if (!key) {
        alert('Key is required.');
        return;
      }
      vscode.postMessage({ type: 'setKv', namespace: ns, key, value });
      clearInspector();
    }

    function executeFtsSearch() {
      const collection = document.getElementById('select-fts').value;
      const query = document.getElementById('fts-query-input').value.trim();
      if (!collection || !query) return;
      vscode.postMessage({ type: 'searchFts', collection, query });
    }

    function renderFtsResults(hits) {
      const container = document.getElementById('fts-results-container');
      if (!hits || hits.length === 0) {
        container.innerHTML = '<p>No matches found.</p>';
        return;
      }
      let html = '<table><thead><tr><th>Doc ID</th><th>Rank</th><th>Snippet</th></tr></thead><tbody>';
      hits.forEach(hit => {
        html += \`<tr>
          <td><strong>\${hit.docId}</strong></td>
          <td>\${Number(hit.rank).toFixed(4)}</td>
          <td class="snippet">\${hit.snippet}</td>
        </tr>\`;
      });
      html += '</tbody></table>';
      container.innerHTML = html;
    }

    function traverseGraph() {
      const graph = document.getElementById('select-graph').value;
      const node = document.getElementById('graph-node-input').value.trim();
      const direction = document.getElementById('graph-dir').value;
      if (!graph || !node) {
        alert('Please specify a graph and a node ID.');
        return;
      }
      vscode.postMessage({ type: 'graphTraverse', graph, node, direction });
    }

    function renderGraphResults(neighbors) {
      const container = document.getElementById('graph-results-container');
      if (!neighbors || neighbors.length === 0) {
        container.innerHTML = '<p>No neighbors found for this node.</p>';
        return;
      }
      let html = \`<h4>Neighbors (\${neighbors.length}):</h4><ul>\`;
      neighbors.forEach(n => {
        html += \`<li><strong>\${escapeHtml(n)}</strong></li>\`;
      });
      html += '</ul>';
      container.innerHTML = html;
    }

    function clearInspector() {
      selectedRecord = null;
      document.getElementById('inspector-body').innerHTML = '<p style="color: #888;">Select a record in the table to inspect and edit its contents.</p>';
    }

    function escapeHtml(str) {
      return String(str).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
    }
  </script>
</body>
</html>`;
  }
}
