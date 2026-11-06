/**
 * CLI Command Router for Knowledge Graph Software
 */

export async function handleCliCommand(storage, command, args) {
  switch (command) {
    case "list": {
      const notes = await storage.listNotes();
      console.log(`\n📚 Total notes (${notes.length}):`);
      for (const n of notes) {
        console.log(`  - [${n.id}] "${n.title}" (by ${n.author || "Anon"})`);
      }
      return;
    }

    case "view": {
      const id = args[0];
      if (!id) {
        console.error("Usage: knowledge-graph view <note_id>");
        return;
      }
      const note = await storage.getNote(id);
      if (!note) {
        console.log(`Note [${id}] not found.`);
        return;
      }
      const lock = await storage.getLock(id);
      const backlinks = await storage.getBacklinks(id);

      console.log("\n=======================================================");
      console.log(`📄 Note: ${note.title} [${note.id}]`);
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
      console.log("=======================================================\n");
      return;
    }

    case "add": {
      const id = args[0];
      const title = args[1];
      const content = args.slice(2).join(" ");
      if (!id || !title || !content) {
        console.error("Usage: knowledge-graph add <id> <title> <content...>");
        return;
      }
      const result = await storage.saveNote({ id, title, content, author: process.env.USER || "CLI_User" });
      console.log(`✓ Note [${id}] saved! Extracted links: [${result.links.join(", ")}], tags: [${result.tags.join(", ")}]`);
      return;
    }

    case "search": {
      const query = args.join(" ");
      if (!query) {
        console.error("Usage: knowledge-graph search <query>");
        return;
      }
      console.log(`\n🔎 FTS5 Search for: "${query}"`);
      const hits = await storage.searchNotes(query, 5);
      if (hits.length === 0) {
        console.log("  (no matches found)");
        return;
      }
      for (const hit of hits) {
        console.log(`  -> [${hit.docId}] (rank: ${hit.rank.toFixed(4)})`);
        console.log(`     Snippet: "...${hit.snippet}..."`);
      }
      return;
    }

    case "backlinks": {
      const id = args[0];
      if (!id) {
        console.error("Usage: knowledge-graph backlinks <note_id>");
        return;
      }
      const links = await storage.getBacklinks(id);
      console.log(`\n🔗 Backlinks pointing to [${id}] (${links.length}):`);
      for (const l of links) {
        console.log(`   <- [${l.id}] "${l.title}" (by ${l.author})`);
      }
      return;
    }

    case "reachability": {
      const id = args[0];
      if (!id) {
        console.error("Usage: knowledge-graph reachability <note_id>");
        return;
      }
      const reachable = await storage.getReachability(id);
      console.log(`\n🌐 Concept Reachability from [${id}] (${reachable.length}):`);
      console.log(`   ${reachable.join(" -> ")}`);
      return;
    }

    case "stats": {
      const stats = await storage.getStats();
      console.log("\n📊 Knowledge Graph Statistics:");
      console.log(`  Total Notes: ${stats.totalNotes}`);
      console.log("\n  Top Referenced Concepts:");
      for (const r of stats.topReferenced) {
        console.log(`   - [${r.note_id}]: ${r.in_links} incoming references`);
      }
      console.log("\n  Author Contributions:");
      for (const a of stats.authorStats) {
        console.log(`   - ${a.author}: ${a.total_notes} notes`);
      }
      return;
    }

    case "lock": {
      const id = args[0];
      const user = args[1] || process.env.USER || "Editor";
      await storage.acquireLock(id, user, 10000);
      console.log(`🔒 Acquired 10-second lock on [${id}] for user "${user}".`);
      return;
    }

    case "unlock": {
      const id = args[0];
      await storage.releaseLock(id);
      console.log(`🔓 Released lock on [${id}].`);
      return;
    }

    case "help":
    default: {
      console.log(`
LiliaDB Knowledge Graph Software CLI
Usage:
  knowledge-graph list                     List all notes
  knowledge-graph view <id>                View note details & backlinks
  knowledge-graph add <id> <title> <body..> Create or update a note
  knowledge-graph search <query>           FTS5 BM25 search with snippet highlight
  knowledge-graph backlinks <id>           Find all notes pointing to <id>
  knowledge-graph reachability <id>        Multi-hop graph concept discovery
  knowledge-graph stats                    SQL analytics dashboard
  knowledge-graph lock <id> [user]         Acquire temporary collaborative lock
  knowledge-graph unlock <id>              Release collaborative lock
  knowledge-graph --demo                   Run automated demo suite
`);
    }
  }
}
