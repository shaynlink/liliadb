import * as vscode from "vscode";
import type { ConnectionManager } from "../connection/ConnectionManager.js";
import type { CommandExecutionResult } from "../types.js";
import type { BatchOperation } from "@liliadb/node";

export class CommandConsolePanel {
  public static currentPanel: CommandConsolePanel | undefined;
  private readonly _panel: vscode.WebviewPanel;
  private _disposables: vscode.Disposable[] = [];
  private readonly _history: CommandExecutionResult[] = [];

  public static createOrShow(
    extensionUri: vscode.Uri,
    connectionManager: ConnectionManager,
    initialCommand?: { type: "sql" | "batch"; text: string }
  ): CommandConsolePanel {
    const column = vscode.window.activeTextEditor
      ? vscode.window.activeTextEditor.viewColumn
      : undefined;

    if (CommandConsolePanel.currentPanel) {
      CommandConsolePanel.currentPanel._panel.reveal(column);
      if (initialCommand) {
        CommandConsolePanel.currentPanel.setCommandText(initialCommand);
      }
      return CommandConsolePanel.currentPanel;
    }

    const panel = vscode.window.createWebviewPanel(
      "liliaCommandConsole",
      "LiliaDB Command & Query Console",
      column || vscode.ViewColumn.Two,
      {
        enableScripts: true,
        retainContextWhenHidden: true,
        localResourceRoots: [extensionUri],
      }
    );

    CommandConsolePanel.currentPanel = new CommandConsolePanel(panel, extensionUri, connectionManager, initialCommand);
    return CommandConsolePanel.currentPanel;
  }

  private constructor(
    panel: vscode.WebviewPanel,
    private readonly _extensionUri: vscode.Uri,
    private readonly _connectionManager: ConnectionManager,
    private readonly _initialCommand?: { type: "sql" | "batch"; text: string }
  ) {
    this._panel = panel;
    this._panel.onDidDispose(() => this.dispose(), null, this._disposables);
    this._panel.webview.html = this._getHtmlForWebview();

    this._panel.webview.onDidReceiveMessage(
      async message => {
        await this._handleMessage(message);
      },
      null,
      this._disposables
    );
  }

  public setCommandText(cmd: { type: "sql" | "batch"; text: string }): void {
    this._panel.webview.postMessage({
      type: "setCommand",
      cmd,
    });
  }

  private async _handleMessage(message: { type: string; sql?: string; batchJson?: string }): Promise<void> {
    const service = this._connectionManager.service;
    if (!service.isConnected) {
      vscode.window.showErrorMessage("LiliaDB: Please connect to a database before executing commands.");
      this._panel.webview.postMessage({
        type: "executionError",
        error: "Not connected to any LiliaDB database.",
      });
      return;
    }

    if (message.type === "executeSql") {
      const sql = message.sql?.trim() || "";
      if (!sql) return;

      const startTime = performance.now();
      try {
        const result = await service.sqlQuery(sql);
        const durationMs = Math.round((performance.now() - startTime) * 100) / 100;

        const execResult: CommandExecutionResult = {
          ok: true,
          durationMs,
          type: "sql",
          sqlResult: result,
          timestamp: Date.now(),
        };
        this._history.unshift(execResult);

        this._panel.webview.postMessage({
          type: "executionResult",
          result: execResult,
          rawQuery: sql,
        });
      } catch (err: unknown) {
        const durationMs = Math.round((performance.now() - startTime) * 100) / 100;
        const msg = err instanceof Error ? err.message : String(err);

        const execResult: CommandExecutionResult = {
          ok: false,
          durationMs,
          type: "sql",
          error: msg,
          timestamp: Date.now(),
        };
        this._history.unshift(execResult);

        this._panel.webview.postMessage({
          type: "executionResult",
          result: execResult,
          rawQuery: sql,
        });
      }
    } else if (message.type === "executeBatch") {
      const jsonText = message.batchJson?.trim() || "";
      if (!jsonText) return;

      const startTime = performance.now();
      try {
        const parsed = JSON.parse(jsonText);
        const operations: BatchOperation[] = Array.isArray(parsed) ? parsed : [parsed];

        const mutations = await service.batch(operations);
        const durationMs = Math.round((performance.now() - startTime) * 100) / 100;

        const execResult: CommandExecutionResult = {
          ok: true,
          durationMs,
          type: "batch",
          mutations,
          timestamp: Date.now(),
        };
        this._history.unshift(execResult);

        this._panel.webview.postMessage({
          type: "executionResult",
          result: execResult,
          rawQuery: jsonText,
        });
      } catch (err: unknown) {
        const durationMs = Math.round((performance.now() - startTime) * 100) / 100;
        const msg = err instanceof Error ? err.message : String(err);

        const execResult: CommandExecutionResult = {
          ok: false,
          durationMs,
          type: "batch",
          error: msg,
          timestamp: Date.now(),
        };
        this._history.unshift(execResult);

        this._panel.webview.postMessage({
          type: "executionResult",
          result: execResult,
          rawQuery: jsonText,
        });
      }
    }
  }

  public dispose(): void {
    CommandConsolePanel.currentPanel = undefined;
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
  <title>LiliaDB Console</title>
  <style>
    :root {
      --bg: var(--vscode-editor-background);
      --fg: var(--vscode-editor-foreground);
      --border: var(--vscode-panel-border, #3c3c3c);
      --accent: var(--vscode-button-background, #007acc);
      --accent-hover: var(--vscode-button-hoverBackground, #0062a3);
      --input-bg: var(--vscode-input-background, #252526);
      --input-fg: var(--vscode-input-foreground, #cccccc);
      --card-bg: var(--vscode-editorWidget-background, #252526);
    }
    body {
      font-family: var(--vscode-font-family, monospace);
      margin: 0;
      padding: 0;
      background: var(--bg);
      color: var(--fg);
      display: flex;
      flex-direction: column;
      height: 100vh;
      overflow: hidden;
    }
    .top-bar {
      padding: 10px 16px;
      border-bottom: 1px solid var(--border);
      background: var(--card-bg);
      display: flex;
      align-items: center;
      justify-content: space-between;
    }
    .tabs {
      display: flex;
      gap: 8px;
    }
    .tab-btn {
      background: transparent;
      border: 1px solid transparent;
      color: var(--fg);
      padding: 6px 12px;
      cursor: pointer;
      border-radius: 4px;
      font-size: 13px;
    }
    .tab-btn.active {
      background: var(--accent);
      color: #fff;
    }
    .editor-section {
      padding: 12px 16px;
      display: flex;
      flex-direction: column;
      gap: 8px;
      border-bottom: 1px solid var(--border);
      background: var(--card-bg);
    }
    textarea {
      background: var(--input-bg);
      color: var(--input-fg);
      border: 1px solid var(--border);
      border-radius: 4px;
      padding: 10px;
      font-family: var(--vscode-editor-font-family, monospace);
      font-size: 13px;
      resize: vertical;
      min-height: 110px;
    }
    .action-row {
      display: flex;
      justify-content: space-between;
      align-items: center;
    }
    button.primary {
      background: var(--accent);
      color: #fff;
      border: none;
      border-radius: 4px;
      padding: 7px 16px;
      font-size: 13px;
      cursor: pointer;
      font-weight: 600;
    }
    button.primary:hover {
      background: var(--accent-hover);
    }
    button.sec {
      background: transparent;
      border: 1px solid var(--border);
      color: var(--fg);
      border-radius: 4px;
      padding: 5px 10px;
      font-size: 12px;
      cursor: pointer;
    }
    .templates {
      display: flex;
      gap: 6px;
    }
    .results-container {
      flex: 1;
      display: flex;
      flex-direction: column;
      overflow-y: auto;
      padding: 16px;
    }
    .status-badge {
      display: inline-block;
      padding: 3px 8px;
      border-radius: 12px;
      font-size: 11px;
      font-weight: bold;
    }
    .status-success {
      background: rgba(40, 167, 69, 0.25);
      color: #4cd964;
      border: 1px solid #4cd964;
    }
    .status-err {
      background: rgba(220, 53, 69, 0.25);
      color: #ff5b57;
      border: 1px solid #ff5b57;
    }
    table {
      width: 100%;
      border-collapse: collapse;
      font-size: 12px;
      margin-top: 10px;
    }
    th, td {
      border: 1px solid var(--border);
      padding: 6px 10px;
      text-align: left;
    }
    th {
      background: var(--card-bg);
      position: sticky;
      top: 0;
    }
    .history-drawer {
      max-height: 140px;
      overflow-y: auto;
      border-top: 1px solid var(--border);
      background: var(--card-bg);
      padding: 8px 16px;
      font-size: 12px;
    }
    .history-item {
      padding: 4px 6px;
      cursor: pointer;
      border-radius: 3px;
      display: flex;
      justify-content: space-between;
    }
    .history-item:hover {
      background: rgba(255, 255, 255, 0.08);
    }
    .hidden {
      display: none !important;
    }
  </style>
</head>
<body>
  <div class="top-bar">
    <div class="tabs">
      <button class="tab-btn active" id="tab-sql" onclick="setMode('sql')">SQL Query</button>
      <button class="tab-btn" id="tab-batch" onclick="setMode('batch')">Batch Operations</button>
    </div>
    <span style="font-size: 12px; color: #888;">Press <strong>Cmd+Enter</strong> / <strong>Ctrl+Enter</strong> to execute</span>
  </div>

  <div class="editor-section">
    <textarea id="editor" placeholder="Write query..."></textarea>
    <div class="action-row">
      <div class="templates" id="batch-templates" class="hidden">
        <button class="sec" onclick="insertTemplate('json_put')">+ JSON Put</button>
        <button class="sec" onclick="insertTemplate('kv_set')">+ KV Set</button>
        <button class="sec" onclick="insertTemplate('graph_node')">+ Graph Node</button>
      </div>
      <div></div>
      <button class="primary" onclick="runCommand()">▶ Execute Command</button>
    </div>
  </div>

  <div class="results-container" id="results-area">
    <p style="color: #888;">Enter a query above and click Execute (or Cmd+Enter).</p>
  </div>

  <div class="history-drawer">
    <strong>Recent History:</strong>
    <div id="history-list">
      <span style="color: #666;">(No commands executed yet)</span>
    </div>
  </div>

  <script>
    const vscode = acquireVsCodeApi();
    let currentMode = 'sql';
    let history = [];

    // Initial placeholder
    document.getElementById('editor').value = 'SELECT * FROM _lilia_json LIMIT 10;';

    window.addEventListener('message', event => {
      const msg = event.data;
      if (msg.type === 'executionResult') {
        renderResult(msg.result, msg.rawQuery);
      } else if (msg.type === 'executionError') {
        renderError(msg.error);
      } else if (msg.type === 'setCommand') {
        setMode(msg.cmd.type);
        document.getElementById('editor').value = msg.cmd.text;
      }
    });

    document.getElementById('editor').addEventListener('keydown', e => {
      if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') {
        e.preventDefault();
        runCommand();
      }
    });

    function setMode(mode) {
      currentMode = mode;
      document.getElementById('tab-sql').classList.toggle('active', mode === 'sql');
      document.getElementById('tab-batch').classList.toggle('active', mode === 'batch');
      document.getElementById('batch-templates').classList.toggle('hidden', mode !== 'batch');

      if (mode === 'sql' && !document.getElementById('editor').value.trim()) {
        document.getElementById('editor').value = 'SELECT * FROM _lilia_json LIMIT 10;';
      } else if (mode === 'batch' && (!document.getElementById('editor').value.trim() || document.getElementById('editor').value.startsWith('SELECT'))) {
        document.getElementById('editor').value = JSON.stringify([
          { "model": "json_put", "space": "notes", "id": "demo", "value": { "text": "Hello LiliaDB" } }
        ], null, 2);
      }
    }

    function insertTemplate(type) {
      const editor = document.getElementById('editor');
      let tpl = {};
      if (type === 'json_put') {
        tpl = { "model": "json_put", "space": "notes", "id": "new_id", "value": { "title": "Example" } };
      } else if (type === 'kv_set') {
        tpl = { "model": "kv_set", "namespace": "cache", "key": "user:1", "value": "active" };
      } else if (type === 'graph_node') {
        tpl = { "model": "graph_set_node", "graph": "knowledge", "id": "node_1", "kind": "Item", "metadata": {} };
      }
      try {
        let current = JSON.parse(editor.value || '[]');
        if (!Array.isArray(current)) current = [current];
        current.push(tpl);
        editor.value = JSON.stringify(current, null, 2);
      } catch {
        editor.value = JSON.stringify([tpl], null, 2);
      }
    }

    function runCommand() {
      const text = document.getElementById('editor').value.trim();
      if (!text) return;

      document.getElementById('results-area').innerHTML = '<p>Executing command...</p>';

      if (currentMode === 'sql') {
        vscode.postMessage({ type: 'executeSql', sql: text });
      } else {
        vscode.postMessage({ type: 'executeBatch', batchJson: text });
      }
    }

    function renderResult(res, rawQuery) {
      addToHistory(res, rawQuery);

      let html = \`<div style="display: flex; gap: 10px; align-items: center; margin-bottom: 12px;">
        <span class="status-badge \${res.ok ? 'status-success' : 'status-err'}">\${res.ok ? 'SUCCESS' : 'ERROR'}</span>
        <span>⚡ \${res.durationMs} ms</span>
      </div>\`;

      if (!res.ok) {
        html += \`<div style="color: #ff5b57; background: rgba(220,53,69,0.1); padding: 12px; border-radius: 4px; font-family: monospace;">\${escapeHtml(res.error)}</div>\`;
      } else if (res.type === 'sql' && res.sqlResult) {
        const { columns, records, rowsAffected } = res.sqlResult;
        html += \`<p><strong>Rows:</strong> \${records ? records.length : rowsAffected} affected/returned</p>\`;
        if (records && records.length > 0) {
          const cols = columns && columns.length > 0 ? columns : Object.keys(records[0] || {});
          html += '<table><thead><tr>' + cols.map(c => '<th>' + escapeHtml(c) + '</th>').join('') + '</tr></thead><tbody>';
          records.forEach(r => {
            html += '<tr>' + cols.map(c => '<td>' + escapeHtml(typeof r[c] === 'object' ? JSON.stringify(r[c]) : r[c]) + '</td>').join('') + '</tr>';
          });
          html += '</tbody></table>';
        }
      } else if (res.type === 'batch' && res.mutations) {
        html += \`<p><strong>Mutations Applied:</strong> \${res.mutations.length}</p>\`;
        html += '<table><thead><tr><th>#</th><th>Version</th><th>Deleted</th></tr></thead><tbody>';
        res.mutations.forEach((m, idx) => {
          html += \`<tr><td>\${idx + 1}</td><td>v\${m.version || 'N/A'}</td><td>\${m.deleted ? 'Yes' : 'No'}</td></tr>\`;
        });
        html += '</tbody></table>';
      }

      document.getElementById('results-area').innerHTML = html;
    }

    function renderError(err) {
      document.getElementById('results-area').innerHTML = \`<div style="color: #ff5b57;">\${escapeHtml(err)}</div>\`;
    }

    function addToHistory(res, rawQuery) {
      history.unshift({ res, rawQuery, time: new Date().toLocaleTimeString() });
      if (history.length > 15) history.pop();

      const container = document.getElementById('history-list');
      container.innerHTML = history.map((item, idx) => \`
        <div class="history-item" onclick="loadFromHistory(\${idx})">
          <span>\${escapeHtml(item.rawQuery.slice(0, 50))}...</span>
          <span style="color: #888;">\${item.res.durationMs}ms - \${item.time}</span>
        </div>
      \`).join('');
    }

    function loadFromHistory(idx) {
      const item = history[idx];
      if (!item) return;
      document.getElementById('editor').value = item.rawQuery;
      setMode(item.res.type);
    }

    function escapeHtml(str) {
      return String(str).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
    }
  </script>
</body>
</html>`;
  }
}
