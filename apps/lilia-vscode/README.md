# LiliaDB for Visual Studio Code

Official Visual Studio Code extension for **LiliaDB**, the high-performance local-first multi-model database.

## Features

- **Dual Connection Modes**:
  - **Embedded (Direct File)**: Open and query any `.lilia` or SQLite database directly with zero configuration or external daemon needed.
  - **Daemon Mode**: Connect to a running `lilia-daemon` over IPC Unix domain sockets, Windows named pipes, or HTTP REST gateway.
- **Multi-Model Data Explorer**:
  - **JSON Documents**: Browse spaces, inspect documents, view secondary indexes, and edit/delete documents in real time.
  - **Key-Value Store**: Explore namespaces, inspect binary/text keys and values, check versions, and manage TTLs.
  - **Graph Engine**: View graph nodes, edges, traverse neighbors (`outgoing`, `incoming`, `both`), and analyze graph reachability.
  - **Full-Text Search (FTS)**: Query collections with instant highlighted snippets and BM25 rank scores.
- **Interactive Visualizer**:
  - Dynamic table/grid views with sorting, live search filter, and pagination.
  - Built-in JSON editor modal with validation and version conflict checks.
- **Command & Query Console**:
  - Run custom SQL queries (`SELECT`, `PRAGMA`, etc.) against views and tables.
  - Execute multi-model batch mutations (`json_put`, `kv_set`, `graph_set_node`, `fts_index`).
  - Real-time execution metrics (`⚡ 1.2 ms`) and history drawer.
  - Run active `.sql` and `.json` files directly from the editor (`Cmd+Enter` / `Ctrl+Enter`).
- **Database Tools**:
  - Doctor: Run full database integrity checks.
  - Backup: Create instant point-in-time database backups.

## Usage

1. Open a workspace containing `.lilia` database files.
2. The extension automatically detects databases and connects in **Embedded** mode.
3. Open the **LiliaDB** activity bar tab on the left to explore namespaces, spaces, graphs, and search collections.
4. Click **Data Visualizer** or **Command Console** to query and edit records.

## License

Apache-2.0
