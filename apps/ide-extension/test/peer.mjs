import { once } from "node:events";
import { mkdirSync, mkdtempSync, rmSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";

/**
 * Runs `check` with a real Unix socket server in a private temporary folder that also
 * holds `ide/alpha/`, the folder of project alpha's capability file. Input: the check,
 * given the server, the socket path and the capability path. Output: its result.
 */
export async function withServer(check) {
  const root = mkdtempSync(join(tmpdir(), "ca-ide-peer-"));
  const server = createServer();
  try {
    mkdirSync(join(root, "ide", "alpha"), { recursive: true });
    const socket = join(root, "owner.sock");
    server.listen(socket);
    await once(server, "listening");
    return await check(server, socket, join(root, "ide", "alpha", "capability"));
  } finally {
    server.close();
    rmSync(root, { recursive: true, force: true });
  }
}

/**
 * Reads what one accepted connection sends. `take` waits for a byte count, `frame` for
 * one whole length-prefixed frame, and `left` tells how many bytes are still unread.
 */
export function reading(connection) {
  let pending = Buffer.alloc(0);
  let wake = () => undefined;
  connection.on("data", (chunk) => {
    pending = Buffer.concat([pending, chunk]);
    wake();
  });
  const take = async (count) => {
    while (pending.length < count) await new Promise((resolve) => (wake = resolve));
    const taken = pending.subarray(0, count);
    pending = pending.subarray(count);
    return taken;
  };
  return {
    take,
    async frame() {
      const header = await take(4);
      return Buffer.concat([header, await take(header.readUInt32BE(0))]);
    },
    left: () => pending.length,
  };
}

/** A token store kept in memory, the shape of VS Code's `SecretStorage`. */
export function memoryStore(entries = []) {
  const values = new Map(entries);
  return {
    values,
    get: async (key) => values.get(key),
    store: async (key, value) => {
      values.set(key, value);
    },
  };
}
