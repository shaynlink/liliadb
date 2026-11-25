// The worker's side of a database run in a worker (see `messages.ts`).

import type { Endpoint, Reply, Request } from "./messages.js";
import { loadModule, ModuleDatabase } from "./module.js";

/** The message of `error`: the JSON of an engine error, as the module throws it. */
function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/** Serves the database `endpoint` asks for: it opens it, runs its operations, closes it. */
export function serve(endpoint: Endpoint): void {
  let database: ModuleDatabase | undefined;
  // One request at a time, in order: the module runs one call at a time.
  let queue = Promise.resolve();

  const handle = async (request: Request): Promise<Reply> => {
    switch (request.type) {
      case "open":
        await loadModule(request.wasm);
        database = await ModuleDatabase.open(request.options);
        return { id: request.id };
      case "execute": {
        if (!database)
          throw new Error(
            '{"code":"CLOSED","message":"the database is not open","retryable":false}',
          );
        const frame = await database.execute(new Uint8Array(request.request));
        return { id: request.id, frame: frame.buffer as ArrayBuffer };
      }
      case "close":
        database?.close();
        database?.free();
        database = undefined;
        return { id: request.id };
    }
  };

  const respond = async (request: Request): Promise<void> => {
    let reply: Reply;
    try {
      reply = await handle(request);
    } catch (error) {
      reply = { id: request.id, error: message(error) };
    }
    endpoint.postMessage(reply, "frame" in reply && reply.frame ? [reply.frame] : []);
  };

  endpoint.addEventListener("message", (event: MessageEvent) => {
    const request = event.data as Request;
    queue = queue.then(() => respond(request));
  });
  endpoint.start?.();
}
