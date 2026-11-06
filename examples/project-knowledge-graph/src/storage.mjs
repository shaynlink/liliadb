/**
 * LiliaDB Storage Layer for Second Brain & Knowledge Graph Software
 */

import { Database } from "../../../packages/node/dist/index.js";
import { parseWikilinks, parseTags } from "./parser.mjs";

const enc = new TextEncoder();
const dec = new TextDecoder();

export class KnowledgeStorage {
  constructor(db) {
    this.db = db;
  }

  static async open(path = "./knowledge_brain.lilia") {
    const db = await Database.open({
      path,
      durability: "durable",
    });
    return new KnowledgeStorage(db);
  }

  /**
   * Commit a note atomically across JSON, FTS5, and Graph.
   */
  async saveNote({ id, title, content, author = "Anonymous", extraTags = [] }) {
    const extractedLinks = parseWikilinks(content);
    const extractedTags = parseTags(content);
    const tags = Array.from(new Set([...extractedTags, ...extraTags]));

    const batchOps = [
      // 1. JSON Document
      {
        model: "json_put",
        space: "notes",
        id,
        value: {
          title,
          content,
          author,
          tags,
          updated_at: Date.now(),
        },
      },
      // 2. FTS5 Indexing
      {
        model: "fts_index",
        collection: "notes_fts",
        docId: id,
        content: `${title}\n${content}\n${tags.join(" ")}`,
      },
      // 3. Graph Node
      {
        model: "graph_set_node",
        graph: "knowledge",
        id,
        kind: "Note",
        metadata: { title, author },
      },
      // 4. Author Graph Edge
      {
        model: "graph_add_edge",
        graph: "knowledge",
        source: author,
        target: id,
        kind: "AUTHORED",
      },
    ];

    // 5. Wikilinks Graph Edges
    for (const targetId of extractedLinks) {
      batchOps.push({
        model: "graph_add_edge",
        graph: "knowledge",
        source: id,
        target: targetId,
        kind: "LINKS_TO",
      });
    }

    await this.db.batch(batchOps);
    return { id, title, links: extractedLinks, tags };
  }

  async getNote(id) {
    const entry = await this.db.json.get("notes", id);
    if (!entry) return null;
    return {
      id: entry.id,
      ...entry.value,
      version: entry.version,
    };
  }

  async listNotes(limit = 100) {
    const entries = await this.db.json.scan("notes", { limit });
    return entries.map((e) => ({
      id: e.id,
      ...e.value,
      version: e.version,
    }));
  }

  async searchNotes(query, limit = 5) {
    const hits = await this.db.fts.search("notes_fts", query, { limit });
    return hits;
  }

  async getBacklinks(noteId) {
    const sources = await this.db.graph.neighbors("knowledge", noteId, {
      direction: "incoming",
      kind: "LINKS_TO",
    });
    const results = [];
    for (const src of sources) {
      const doc = await this.getNote(src);
      results.push({
        id: src,
        title: doc?.title || src,
        author: doc?.author || "Unknown",
      });
    }
    return results;
  }

  async getReachability(startNodeId) {
    return this.db.graph.reachability("knowledge", [startNodeId], {
      direction: "outgoing",
    });
  }

  async acquireLock(noteId, userId, ttlMs = 5000) {
    const key = enc.encode(`lock:${noteId}`);
    const val = enc.encode(userId);
    const expiresAtMs = Date.now() + ttlMs;
    await this.db.kv.set("presence", key, val, { expiresAtMs });
    return { noteId, userId, expiresAtMs };
  }

  async getLock(noteId) {
    const key = enc.encode(`lock:${noteId}`);
    const entry = await this.db.kv.get("presence", key);
    if (!entry) return null;
    return dec.decode(entry.value);
  }

  async releaseLock(noteId) {
    const key = enc.encode(`lock:${noteId}`);
    await this.db.kv.delete("presence", key);
  }

  async getStats() {
    const topReferenced = await this.db.sql.query(`
      SELECT target AS note_id, COUNT(*) AS in_links
      FROM lilia_graph_edges
      WHERE kind = 'LINKS_TO'
      GROUP BY target
      ORDER BY in_links DESC
      LIMIT 10
    `);

    const authorStats = await this.db.sql.query(`
      SELECT
        json_extract(value, '$.author') AS author,
        COUNT(*) AS total_notes
      FROM lilia_json
      WHERE space = 'notes'
      GROUP BY author
      ORDER BY total_notes DESC
    `);

    const totalNotes = await this.db.sql.query(`
      SELECT COUNT(*) as count FROM lilia_json WHERE space = 'notes'
    `);

    return {
      totalNotes: totalNotes[0]?.count || 0,
      topReferenced,
      authorStats,
    };
  }

  async watch(callback) {
    return this.db.watch("notes", callback);
  }

  async integrityCheck() {
    return this.db.integrityCheck();
  }

  async close() {
    await this.db.close();
  }
}
