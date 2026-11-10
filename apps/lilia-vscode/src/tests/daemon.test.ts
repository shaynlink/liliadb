import { test, describe } from "node:test";
import * as assert from "node:assert";
import * as path from "node:path";
import { LiliaDatabaseService } from "../connection/LiliaDatabaseService.js";

describe("LiliaDatabaseService Daemon Mode Tests", () => {
  test("handles connection failure gracefully when daemon endpoint is offline", async () => {
    const service = new LiliaDatabaseService();
    const fakeDb = path.resolve(process.cwd(), "non_existent.lilia");

    await assert.rejects(
      async () => {
        await service.open({
          path: fakeDb,
          mode: "daemon",
          endpoint: "/tmp/non_existent_lilia.sock",
          tokenFile: "/tmp/non_existent_lilia.token",
        });
      },
      (err: Error) => {
        return err.message.length > 0;
      }
    );

    assert.strictEqual(service.isConnected, false);
  });
});
