/**
 * Interactive REPL Console Interface for Knowledge Graph Software
 */

import { createInterface } from "node:readline/promises";
import { stdin as input, stdout as output } from "node:process";

export async function runInteractiveConsole(storage) {
  const rl = createInterface({ input, output });

  console.log("\n=======================================================");
  console.log("🌸 Welcome to LiliaDB Second Brain & Knowledge Graph");
  console.log("=======================================================");

  let running = true;
  while (running) {
    console.log("\n--- Main Menu ---");
    console.log("  [1] List all notes");
    console.log("  [2] View note details & backlinks");
    console.log("  [3] Create / Edit note (with automatic [[wikilinks]])");
    console.log("  [4] Full-Text Search (FTS5 BM25 with snippets)");
    console.log("  [5] Explore Concept Reachability (Multi-hop graph)");
    console.log("  [6] Test Collaborative Lock (KV with TTL)");
    console.log("  [7] SQL Analytics Dashboard");
    console.log("  [8] Seed sample knowledge base");
    console.log("  [0] Exit");

    const choice = (await rl.question("\nEnter choice [0-8]: ")).trim();

    switch (choice) {
      case "1": {
        const notes = await storage.listNotes();
        console.log(`\n📚 Total notes (${notes.length}):`);
        for (const n of notes) {
          console.log(`  - [${n.id}] "${n.title}" (by ${n.author || "Anon"})`);
        }
        break;
      }

      case "2": {
        const id = (await rl.question("Enter note ID: ")).trim();
        if (!id) break;
        const note = await storage.getNote(id);
        if (!note) {
          console.log(`Note [${id}] not found.`);
          break;
        }
        const lock = await storage.getLock(id);
        const backlinks = await storage.getBacklinks(id);

        console.log("\n-------------------------------------------------------");
        console.log(`📄 Title: ${note.title} [${note.id}]`);
        console.log(`👤 Author: ${note.author} | Updated: ${new Date(note.updated_at).toLocaleString()}`);
        if (lock) console.log(`🔒 Active Editor Lock: ${lock}`);
        console.log(`🏷️ Tags: ${(note.tags || []).join(", ") || "(none)"}`);
        console.log("-------------------------------------------------------");
        console.log(note.content);
        console.log("-------------------------------------------------------");
        console.log(`🔗 Backlinks (${backlinks.length}):`);
        for (const b of backlinks) {
          console.log(`   <- [${b.id}] "${b.title}"`);
        }
        console.log("-------------------------------------------------------");
        break;
      }

      case "3": {
        const id = (await rl.question("Note ID (e.g. note_sqlite): ")).trim();
        const title = (await rl.question("Title: ")).trim();
        const author = (await rl.question("Author [Default: You]: ")).trim() || "You";
        console.log("Enter content (use [[target_id]] to link other notes, e.g. 'See [[note_sqlite]]'):");
        const content = (await rl.question("> ")).trim();

        if (id && title) {
          const res = await storage.saveNote({ id, title, content, author });
          console.log(`\n✓ Note [${id}] committed atomically!`);
          console.log(`  Extracted links: [${res.links.join(", ")}]`);
          console.log(`  Extracted tags:  [${res.tags.join(", ")}]`);
        } else {
          console.log("ID and Title are required.");
        }
        break;
      }

      case "4": {
        const q = (await rl.question("Search query: ")).trim();
        if (!q) break;
        console.log(`\n🔎 Searching for "${q}"...`);
        const hits = await storage.searchNotes(q, 5);
        if (hits.length === 0) {
          console.log("  No matches found.");
        } else {
          for (const hit of hits) {
            console.log(`  -> [${hit.docId}] (rank: ${hit.rank.toFixed(4)})`);
            console.log(`     Snippet: "...${hit.snippet}..."`);
          }
        }
        break;
      }

      case "5": {
        const id = (await rl.question("Start note ID (e.g. note_architecture): ")).trim();
        if (!id) break;
        const reachable = await storage.getReachability(id);
        console.log(`\n🌐 Concept Reachability from [${id}] (${reachable.length}):`);
        console.log(`   ${reachable.join(" -> ")}`);
        break;
      }

      case "6": {
        const id = (await rl.question("Note ID to lock: ")).trim();
        const user = (await rl.question("Username: ")).trim() || "User_Alice";
        const ttl = parseInt((await rl.question("TTL in seconds [default 5]: ")).trim() || "5", 10) * 1000;
        await storage.acquireLock(id, user, ttl);
        console.log(`🔒 Acquired ${ttl / 1000}s temporary lock on [${id}] for "${user}".`);
        const check = await storage.getLock(id);
        console.log(`  Current lock status: "${check}"`);
        break;
      }

      case "7": {
        const stats = await storage.getStats();
        console.log("\n📊 Knowledge Graph Analytics Dashboard:");
        console.log(`  Total Notes: ${stats.totalNotes}`);
        console.log("\n  Top Referenced Notes (Inbound Graph Degree):");
        for (const r of stats.topReferenced) {
          console.log(`   - [${r.note_id}]: ${r.in_links} incoming references`);
        }
        console.log("\n  Author Contributions:");
        for (const a of stats.authorStats) {
          console.log(`   - ${a.author}: ${a.total_notes} notes`);
        }
        break;
      }

      case "8": {
        console.log("\nSeeding standard demo notes...");
        await seedDemoArticles(storage);
        console.log("✓ Demo notes seeded!");
        break;
      }

      case "0":
      case "exit":
      case "quit":
        running = false;
        break;

      default:
        console.log("Invalid option, please choose between 0 and 8.");
        break;
    }
  }

  rl.close();
  console.log("\nGoodbye!");
}

export async function seedDemoArticles(storage) {
  const articles = [
    {
      id: "note_architecture",
      title: "LiliaDB System Architecture",
      author: "Ada Lovelace",
      extraTags: ["core", "database"],
      content: "LiliaDB is a local-first multi-model embedded database. It seamlessly unifies SQLite storage with [[note_sqlite]], high speed graph traversals via [[note_graph]], and BM25 text search with [[note_fts]].",
    },
    {
      id: "note_sqlite",
      title: "SQLite ACID Storage Engine",
      author: "Richard Hipp",
      extraTags: ["storage", "engine"],
      content: "SQLite provides write-ahead logging (WAL) and B-Tree indexing. It guarantees strict ACID durability under physical crash conditions and power loss.",
    },
    {
      id: "note_graph",
      title: "Relational & Adjacency Graph Engine",
      author: "Leonhard Euler",
      extraTags: ["graph", "algorithms"],
      content: "The graph engine computes 1-hop neighbor adjacencies and multi-hop reachability paths across nodes. Connects tightly to [[note_sqlite]] for persistence.",
    },
    {
      id: "note_fts",
      title: "Full-Text Search & BM25 Scoring",
      author: "Stephen Robertson",
      extraTags: ["fts", "search"],
      content: "Full-text indexing with SQLite FTS5 offers fast lexical lookup, term frequency BM25 rankings, and highlight snippet generation. Mentions [[note_sqlite]].",
    },
    {
      id: "note_security",
      title: "Security & Encryption at Rest",
      author: "Bruce Schneier",
      extraTags: ["security", "crypto"],
      content: "Authenticated encryption via AES-256-GCM AEAD codecs ensures zero cleartext on disk, protecting sensitive notes.",
    },
  ];

  for (const art of articles) {
    await storage.saveNote(art);
  }
}
