# Software: Second Brain & Knowledge Graph

A local-first, interactive software application built with **LiliaDB**, unifying Documents, Graphs, Full-Text Search, Key/Value presence locks, and SQL Analytics into a single local SQLite file.

---

## Features

- **Double User Interface**:
  - **Interactive REPL Terminal Console**: Menu-driven interface (`npm start`) to explore notes, visualize backlinks, search full-text, and monitor live events.
  - **Scriptable CLI**: Direct command-line utility for shell scripting and automation (`knowledge-graph add`, `search`, `backlinks`, `stats`).
- **Automatic Wikilink & Tag Parsing**: Synthesizes incoming/outgoing edges in the graph engine from markdown `[[note_id]]` syntax and `#tags`.
- **Lexical Search with FTS5**: Ranked BM25 queries with context highlights.
- **Collaborative Presence Locks (KV + TTL)**: Prevents conflicting edits with self-expiring TTL lock tokens.
- **Declarative SQL Analytics**: Live analytics on inbound graph degrees and author metrics.

---

## Installation & Setup

From the root of the repository:

```bash
cd examples/project-knowledge-graph
npm link # (Optional: exposes `knowledge-graph` CLI globally)
```

---

## Usage

### 1. Interactive Terminal Console (REPL)
Launch without arguments to open the interactive menu:

```bash
node examples/project-knowledge-graph/bin/knowledge.mjs
# or
npm start
```

Menu options:
```text
🌸 Welcome to LiliaDB Second Brain & Knowledge Graph
---------------------------------------------------
  [1] List all notes
  [2] View note details & backlinks
  [3] Create / Edit note (with automatic [[wikilinks]])
  [4] Full-Text Search (FTS5 BM25 with snippets)
  [5] Explore Concept Reachability (Multi-hop graph)
  [6] Test Collaborative Lock (KV with TTL)
  [7] SQL Analytics Dashboard
  [8] Seed sample knowledge base
  [0] Exit
```

### 2. Command-Line Interface (CLI)
You can invoke individual commands directly from your terminal or scripts:

```bash
# Add a note with automated wikilink detection
knowledge-graph add note_sqlite "SQLite Storage" "Guarantees strict ACID durability for [[note_architecture]]. #storage"

# View a note with its backlinks and active locks
knowledge-graph view note_sqlite

# Full-text search with BM25 snippet extraction
knowledge-graph search "ACID durability"

# Inspect backlinks (incoming edges) pointing to a note
knowledge-graph backlinks note_sqlite

# Multi-hop transitive reachability path
knowledge-graph reachability note_architecture

# SQL analytics dashboard
knowledge-graph stats

# Collaborative lock management
knowledge-graph lock note_architecture Alice
knowledge-graph unlock note_architecture

# Run the automated walkthrough demo suite
knowledge-graph --demo
```

### 3. Custom Database Path
By default, notes are stored in `./knowledge_brain.lilia`. You can specify a custom database file using `--db`:

```bash
knowledge-graph --db /path/to/my_notes.lilia list
```

---

## Architecture

- `bin/knowledge.mjs`: CLI & REPL entry point.
- `src/storage.mjs`: High-level wrapper over `@liliadb/node` multi-model APIs.
- `src/parser.mjs`: Wikilink `[[...]]` and hashtag `#...` regex parsing.
- `src/cli.mjs`: Subcommand dispatcher.
- `src/interactive.mjs`: Interactive console using Node.js `readline`.
