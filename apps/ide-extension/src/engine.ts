import { existsSync, readFileSync } from "node:fs";
import { createConnection, type Socket } from "node:net";
import { basename, dirname } from "node:path";

/** Largest frame body, in bytes, the engine reads or writes. */
export const FRAME_BYTES_MAX = 65536;

/** Environment variable holding the path of this IDE server's capability file. */
export const CAPABILITY_ENV = "CRAZYAGENT_IDE_CAPABILITY_FILE";

/** Environment variable holding the path of the engine's owner socket. */
export const SOCKET_ENV = "CRAZYAGENT_IDE_SOCKET";

const MAGIC = Buffer.from("CAI1", "latin1");
const TOKEN_BYTES = 32;
const REQUEST_TIME_MS = 30_000;
const RECONNECT_MS = 2_000;
const HEX_TOKEN = /^[0-9a-f]{64}$/;
const FINAL_CODES = new Set([
  "ide_capability_refused",
  "ide_capability_revoked",
  "ide_project_refused",
  "ide_session_missing",
  "ide_session_required",
  "ide_session_taken",
]);
const CHANGES = ["workspace_changed", "change_sets_changed", "agents_changed", "memories_changed"] as const;

/**
 * One engine answer to a bridge request: `ok` with its data, a stable refusal code, or a
 * stale write refused with the current version, value and writer (code `state_stale`).
 */
export type Reply<T> =
  | { kind: "ok"; data: T }
  | { kind: "rejected"; code: string }
  | { kind: "stale"; code: "state_stale"; version: number; current: unknown; writer: string };

/** One event the engine pushes on an `ide.events` subscription. */
export type EngineEvent =
  | { name: (typeof CHANGES)[number] }
  | { name: "reveal"; request: string; path: string };

/** What a subscription receives: its confirmation, events, or the refusal that ends it. */
type Pushed = Reply<unknown> | { kind: "event"; event: EngineEvent };

/** Something that stops when disposed, the shape VS Code subscriptions take. */
export interface Disposable {
  dispose(): void;
}

/**
 * Where the extension keeps its session token across window reloads, the shape of VS
 * Code's `SecretStorage`: `get` reads a key, `store` writes one.
 */
export interface TokenStore {
  get(key: string): PromiseLike<string | undefined>;
  store(key: string, value: string): PromiseLike<void>;
}

/**
 * Encodes one value as an engine frame: a 4-byte big-endian length, then compact JSON.
 * Input: any JSON value. Output: the frame. Throws `ide_frame_bounds` for an empty body
 * or one over `FRAME_BYTES_MAX` bytes, which the engine would refuse.
 */
export function encodeFrame(value: unknown): Buffer {
  const body = Buffer.from(JSON.stringify(value), "utf8");
  if (body.length === 0 || body.length > FRAME_BYTES_MAX) throw new Error("ide_frame_bounds");
  const frame = Buffer.alloc(4 + body.length);
  frame.writeUInt32BE(body.length, 0);
  body.copy(frame, 4);
  return frame;
}

/**
 * Collects engine frames from socket chunks split anywhere, holding at most one partial
 * frame. `push` takes the next chunk and returns every frame it completed, parsed; it
 * throws `ide_frame_bounds` for a length the engine never sends and `ide_frame_invalid`
 * for a body that is not UTF-8 JSON.
 */
export class FrameReader {
  #pending: Buffer = Buffer.alloc(0);
  readonly #text = new TextDecoder("utf-8", { fatal: true });

  push(chunk: Buffer): unknown[] {
    this.#pending = Buffer.concat([this.#pending, chunk]);
    const frames: unknown[] = [];
    while (this.#pending.length >= 4) {
      const length = this.#pending.readUInt32BE(0);
      if (length === 0 || length > FRAME_BYTES_MAX) throw new Error("ide_frame_bounds");
      if (this.#pending.length < 4 + length) break;
      try {
        frames.push(JSON.parse(this.#text.decode(this.#pending.subarray(4, 4 + length))));
      } catch {
        throw new Error("ide_frame_invalid");
      }
      this.#pending = this.#pending.subarray(4 + length);
    }
    return frames;
  }
}

/**
 * Builds the 37-byte handshake of an IDE capability connection: `CAI1`, slot byte zero,
 * then the 32-byte token. Throws `ide_capability_invalid` for a token of another size.
 */
export function handshake(token: Buffer): Buffer {
  if (token.length !== TOKEN_BYTES) throw new Error("ide_capability_invalid");
  return Buffer.concat([MAGIC, Buffer.from([0]), token]);
}

/**
 * Reads the token from a capability file, which holds exactly 64 lowercase hex digits
 * and nothing else. Input: the file path. Output: the 32 token bytes. Throws
 * `ide_capability_invalid` for any other contents.
 */
export function readCapability(path: string): Buffer {
  const text = readFileSync(path, "utf8");
  if (!HEX_TOKEN.test(text)) throw new Error("ide_capability_invalid");
  return Buffer.from(text, "hex");
}

/**
 * The extension's bridge to the engine, scoped to the one project its capability names.
 * The capability file holds a one-time token: the first call swaps it with `ide.session`
 * for a session token, which the engine then removes the file for, and which the bridge
 * keeps in memory and in its token store for a window reload. A new file means a new
 * launch and is swapped again. `request` sends one call on its own connection; `on`
 * delivers events from one shared subscription that reconnects after a drop and stops
 * for good once the capability is refused or revoked. Dispose it to close the
 * subscription.
 */
export class Engine implements Disposable {
  readonly project: string;
  readonly #socket: string;
  readonly #capability: string;
  readonly #store: TokenStore | undefined;
  readonly #handlers = new Map<EngineEvent["name"], Set<(event: EngineEvent) => void>>();
  #session: Promise<Buffer> | undefined;
  #taking = false;
  #subscribed = false;
  #subscription: Socket | undefined;
  #retry: ReturnType<typeof setTimeout> | undefined;
  #closed = false;

  constructor(socket: string, capability: string, store?: TokenStore) {
    this.#socket = socket;
    this.#capability = capability;
    this.#store = store;
    this.project = basename(dirname(capability));
  }

  /**
   * The bridge the engine set up for this IDE server through its launch environment.
   * Input: the environment and where to keep the session token. Output: the bridge, or
   * undefined when either variable is missing, which means CrazyAgent did not launch
   * this server.
   */
  static fromEnv(env: Record<string, string | undefined>, store?: TokenStore): Engine | undefined {
    const capability = env[CAPABILITY_ENV];
    const socket = env[SOCKET_ENV];
    return capability && socket ? new Engine(socket, capability, store) : undefined;
  }

  /**
   * Sends one bridge call and waits for its reply. Input: the call name, such as
   * `ide.change_sets`, its arguments and how long to wait, 30 seconds unless the call runs
   * longer in the engine. Output: the reply. Rejects without a session, on a broken
   * connection, a malformed frame or no reply in time.
   */
  request<T>(name: string, args: Record<string, unknown> = {}, timeMs = REQUEST_TIME_MS): Promise<Reply<T>> {
    return this.#token().then((token) => this.#exchange<T>(token, name, args, timeMs));
  }

  /**
   * Opens a long-lived connection for one streaming call, such as `ide.terminal`, and
   * sends the call. Input: the call name and its arguments. Output: the connected socket,
   * which the caller reads and closes. Rejects without a session.
   */
  stream(name: string, args: Record<string, unknown>): Promise<Socket> {
    return this.#token().then((token) => this.#dial(token, name, args));
  }

  /** The session token, swapping a capability file that is there first, one swap at a time. */
  #token(): Promise<Buffer> {
    if (this.#session === undefined || (!this.#taking && existsSync(this.#capability))) {
      this.#taking = true;
      const session = this.#take().finally(() => {
        this.#taking = false;
      });
      session.catch(() => {
        if (this.#session === session) this.#session = undefined;
      });
      this.#session = session;
    }
    return this.#session;
  }

  /**
   * Swaps the capability file's token for a session token and stores it, or, with no
   * file, reads the stored one. Output: the token. Rejects with the engine's refusal,
   * `ide_session_missing` or `ide_session_invalid`.
   */
  async #take(): Promise<Buffer> {
    const key = `crazyagent.session.${this.project}`;
    if (!existsSync(this.#capability)) {
      const stored = await this.#store?.get(key);
      if (stored === undefined || !HEX_TOKEN.test(stored)) throw new Error("ide_session_missing");
      return Buffer.from(stored, "hex");
    }
    const reply = await this.#exchange<{ token?: unknown }>(readCapability(this.#capability), "ide.session", {});
    if (reply.kind === "rejected") throw new Error(reply.code);
    const token = reply.kind === "ok" ? reply.data?.token : undefined;
    if (typeof token !== "string" || !HEX_TOKEN.test(token)) throw new Error("ide_session_invalid");
    await this.#store?.store(key, token);
    return Buffer.from(token, "hex");
  }

  /** Sends one call with a token on its own connection and waits for the one reply. */
  #exchange<T>(
    token: Buffer,
    name: string,
    args: Record<string, unknown>,
    timeMs = REQUEST_TIME_MS,
  ): Promise<Reply<T>> {
    return new Promise((resolve, reject) => {
      const socket = this.#dial(token, name, args);
      const reader = new FrameReader();
      const timer = setTimeout(() => socket.destroy(new Error("ide_request_timeout")), timeMs);
      socket.on("data", (chunk: Buffer) => {
        try {
          const [reply] = reader.push(chunk);
          if (reply === undefined) return;
          clearTimeout(timer);
          resolve(reply as Reply<T>);
          socket.end();
        } catch (error) {
          socket.destroy(error as Error);
        }
      });
      socket.on("error", (error) => {
        clearTimeout(timer);
        reject(error);
      });
      socket.on("close", () => {
        clearTimeout(timer);
        reject(new Error("ide_engine_closed"));
      });
    });
  }

  /**
   * Calls `handler` for every event with this name until the returned handle is disposed.
   * The first handler opens the shared subscription. After a lagged subscription every
   * change event fires once, so views refresh what they missed.
   */
  on<N extends EngineEvent["name"]>(
    name: N,
    handler: (event: Extract<EngineEvent, { name: N }>) => void,
  ): Disposable {
    const handlers = this.#handlers.get(name) ?? new Set();
    const listener = handler as (event: EngineEvent) => void;
    handlers.add(listener);
    this.#handlers.set(name, handlers);
    if (!this.#subscribed) {
      this.#subscribed = true;
      this.#subscribe();
    }
    return { dispose: () => handlers.delete(listener) };
  }

  /** Closes the event subscription and stops reconnecting. */
  dispose(): void {
    this.#closed = true;
    clearTimeout(this.#retry);
    this.#retry = undefined;
    this.#subscription?.destroy();
  }

  /** Opens a connection and sends the handshake with a token and one call. */
  #dial(token: Buffer, name: string, args: Record<string, unknown>): Socket {
    const packet = handshake(token);
    const frame = encodeFrame({ project: this.project, call: { name, args } });
    const socket = createConnection(this.#socket);
    socket.write(packet);
    socket.write(frame);
    return socket;
  }

  /**
   * Opens the shared `ide.events` subscription, or schedules another try when it cannot;
   * a refused session stops it for good.
   */
  #subscribe(): void {
    if (this.#closed) return;
    this.#retry = undefined;
    this.stream("ide.events", {}).then(
      (socket) => this.#listen(socket),
      (error: unknown) => {
        if (error instanceof Error && FINAL_CODES.has(error.message)) this.#closed = true;
        this.#later();
      },
    );
  }

  /** Delivers what one subscription connection pushes, then reconnects after it drops. */
  #listen(socket: Socket): void {
    if (this.#closed) {
      socket.destroy();
      return;
    }
    this.#subscription = socket;
    const reader = new FrameReader();
    socket.on("data", (chunk: Buffer) => {
      try {
        for (const pushed of reader.push(chunk) as Pushed[]) this.#deliver(pushed);
      } catch {
        socket.destroy();
      }
    });
    socket.on("error", () => undefined);
    socket.on("close", () => {
      this.#subscription = undefined;
      this.#later();
    });
  }

  /** Hands one pushed frame to the handlers, or stops or refreshes on a refusal. */
  #deliver(pushed: Pushed): void {
    if (pushed.kind === "event") {
      this.#emit(pushed.event);
    } else if (pushed.kind === "rejected" && FINAL_CODES.has(pushed.code)) {
      this.#closed = true;
    } else if (pushed.kind === "rejected" && pushed.code === "ide_events_lagged") {
      for (const name of CHANGES) this.#emit({ name });
    }
  }

  /** Calls every handler registered for the event's name. */
  #emit(event: EngineEvent): void {
    for (const handler of this.#handlers.get(event.name) ?? []) handler(event);
  }

  /** Schedules one reconnect unless the bridge is closed or one is already due. */
  #later(): void {
    if (this.#closed || this.#retry !== undefined) return;
    this.#retry = setTimeout(() => this.#subscribe(), RECONNECT_MS);
  }
}
