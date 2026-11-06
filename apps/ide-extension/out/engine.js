"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.Engine = exports.FrameReader = exports.SOCKET_ENV = exports.CAPABILITY_ENV = exports.FRAME_BYTES_MAX = void 0;
exports.encodeFrame = encodeFrame;
exports.handshake = handshake;
exports.readCapability = readCapability;
const node_fs_1 = require("node:fs");
const node_net_1 = require("node:net");
const node_path_1 = require("node:path");
exports.FRAME_BYTES_MAX = 65536;
exports.CAPABILITY_ENV = "CRAZYAGENT_IDE_CAPABILITY_FILE";
exports.SOCKET_ENV = "CRAZYAGENT_IDE_SOCKET";
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
const CHANGES = ["workspace_changed", "change_sets_changed", "agents_changed", "memories_changed"];
function encodeFrame(value) {
    const body = Buffer.from(JSON.stringify(value), "utf8");
    if (body.length === 0 || body.length > exports.FRAME_BYTES_MAX)
        throw new Error("ide_frame_bounds");
    const frame = Buffer.alloc(4 + body.length);
    frame.writeUInt32BE(body.length, 0);
    body.copy(frame, 4);
    return frame;
}
class FrameReader {
    #pending = Buffer.alloc(0);
    #text = new TextDecoder("utf-8", { fatal: true });
    push(chunk) {
        this.#pending = Buffer.concat([this.#pending, chunk]);
        const frames = [];
        while (this.#pending.length >= 4) {
            const length = this.#pending.readUInt32BE(0);
            if (length === 0 || length > exports.FRAME_BYTES_MAX)
                throw new Error("ide_frame_bounds");
            if (this.#pending.length < 4 + length)
                break;
            try {
                frames.push(JSON.parse(this.#text.decode(this.#pending.subarray(4, 4 + length))));
            }
            catch {
                throw new Error("ide_frame_invalid");
            }
            this.#pending = this.#pending.subarray(4 + length);
        }
        return frames;
    }
}
exports.FrameReader = FrameReader;
function handshake(token) {
    if (token.length !== TOKEN_BYTES)
        throw new Error("ide_capability_invalid");
    return Buffer.concat([MAGIC, Buffer.from([0]), token]);
}
function readCapability(path) {
    const text = (0, node_fs_1.readFileSync)(path, "utf8");
    if (!HEX_TOKEN.test(text))
        throw new Error("ide_capability_invalid");
    return Buffer.from(text, "hex");
}
class Engine {
    project;
    #socket;
    #capability;
    #store;
    #handlers = new Map();
    #session;
    #taking = false;
    #subscribed = false;
    #subscription;
    #retry;
    #closed = false;
    constructor(socket, capability, store) {
        this.#socket = socket;
        this.#capability = capability;
        this.#store = store;
        this.project = (0, node_path_1.basename)((0, node_path_1.dirname)(capability));
    }
    static fromEnv(env, store) {
        const capability = env[exports.CAPABILITY_ENV];
        const socket = env[exports.SOCKET_ENV];
        return capability && socket ? new Engine(socket, capability, store) : undefined;
    }
    request(name, args = {}, timeMs = REQUEST_TIME_MS) {
        return this.#token().then((token) => this.#exchange(token, name, args, timeMs));
    }
    stream(name, args) {
        return this.#token().then((token) => this.#dial(token, name, args));
    }
    #token() {
        if (this.#session === undefined || (!this.#taking && (0, node_fs_1.existsSync)(this.#capability))) {
            this.#taking = true;
            const session = this.#take().finally(() => {
                this.#taking = false;
            });
            session.catch(() => {
                if (this.#session === session)
                    this.#session = undefined;
            });
            this.#session = session;
        }
        return this.#session;
    }
    async #take() {
        const key = `crazyagent.session.${this.project}`;
        if (!(0, node_fs_1.existsSync)(this.#capability)) {
            const stored = await this.#store?.get(key);
            if (stored === undefined || !HEX_TOKEN.test(stored))
                throw new Error("ide_session_missing");
            return Buffer.from(stored, "hex");
        }
        const reply = await this.#exchange(readCapability(this.#capability), "ide.session", {});
        if (reply.kind === "rejected")
            throw new Error(reply.code);
        const token = reply.kind === "ok" ? reply.data?.token : undefined;
        if (typeof token !== "string" || !HEX_TOKEN.test(token))
            throw new Error("ide_session_invalid");
        await this.#store?.store(key, token);
        return Buffer.from(token, "hex");
    }
    #exchange(token, name, args, timeMs = REQUEST_TIME_MS) {
        return new Promise((resolve, reject) => {
            const socket = this.#dial(token, name, args);
            const reader = new FrameReader();
            const timer = setTimeout(() => socket.destroy(new Error("ide_request_timeout")), timeMs);
            socket.on("data", (chunk) => {
                try {
                    const [reply] = reader.push(chunk);
                    if (reply === undefined)
                        return;
                    clearTimeout(timer);
                    resolve(reply);
                    socket.end();
                }
                catch (error) {
                    socket.destroy(error);
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
    on(name, handler) {
        const handlers = this.#handlers.get(name) ?? new Set();
        const listener = handler;
        handlers.add(listener);
        this.#handlers.set(name, handlers);
        if (!this.#subscribed) {
            this.#subscribed = true;
            this.#subscribe();
        }
        return { dispose: () => handlers.delete(listener) };
    }
    dispose() {
        this.#closed = true;
        clearTimeout(this.#retry);
        this.#retry = undefined;
        this.#subscription?.destroy();
    }
    #dial(token, name, args) {
        const packet = handshake(token);
        const frame = encodeFrame({ project: this.project, call: { name, args } });
        const socket = (0, node_net_1.createConnection)(this.#socket);
        socket.write(packet);
        socket.write(frame);
        return socket;
    }
    #subscribe() {
        if (this.#closed)
            return;
        this.#retry = undefined;
        this.stream("ide.events", {}).then((socket) => this.#listen(socket), (error) => {
            if (error instanceof Error && FINAL_CODES.has(error.message))
                this.#closed = true;
            this.#later();
        });
    }
    #listen(socket) {
        if (this.#closed) {
            socket.destroy();
            return;
        }
        this.#subscription = socket;
        const reader = new FrameReader();
        socket.on("data", (chunk) => {
            try {
                for (const pushed of reader.push(chunk))
                    this.#deliver(pushed);
            }
            catch {
                socket.destroy();
            }
        });
        socket.on("error", () => undefined);
        socket.on("close", () => {
            this.#subscription = undefined;
            this.#later();
        });
    }
    #deliver(pushed) {
        if (pushed.kind === "event") {
            this.#emit(pushed.event);
        }
        else if (pushed.kind === "rejected" && FINAL_CODES.has(pushed.code)) {
            this.#closed = true;
        }
        else if (pushed.kind === "rejected" && pushed.code === "ide_events_lagged") {
            for (const name of CHANGES)
                this.#emit({ name });
        }
    }
    #emit(event) {
        for (const handler of this.#handlers.get(event.name) ?? [])
            handler(event);
    }
    #later() {
        if (this.#closed || this.#retry !== undefined)
            return;
        this.#retry = setTimeout(() => this.#subscribe(), RECONNECT_MS);
    }
}
exports.Engine = Engine;
