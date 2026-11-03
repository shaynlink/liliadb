import { Database } from "@liliadb/wasm";
// Cloudflare Workers bundler automatically provides the compiled WebAssembly.Module
// when importing a .wasm file.
// @ts-ignore
import wasmModule from "@liliadb/wasm/pkg/lilia_wasm_bg.wasm";

export default {
  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);

    // Initialize in-memory LiliaDB instance for the Edge request
    const db = await Database.open({
      path: ":memory:",
      vfs: "memory",
      wasmModule,
    });

    try {
      if (url.pathname === "/") {
        const versionResult = await db.sql.query("SELECT sqlite_version() as version");
        const integrityOk = await db.integrityCheck();
        return Response.json({
          status: "online",
          runtime: "cloudflare-workers",
          sqliteVersion: versionResult[0],
          integrity: integrityOk,
        });
      }

      if (url.pathname === "/query" && request.method === "POST") {
        const body = (await request.json()) as { sql: string; params?: any[] };
        const rows = await db.sql.query(body.sql, body.params ?? []);
        return Response.json({ rows });
      }

      return new Response("Not Found", { status: 404 });
    } finally {
      await db.close();
    }
  },
};
