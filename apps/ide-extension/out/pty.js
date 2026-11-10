"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.SandboxedPty = exports.ACCESS_ENV = exports.ABOVE_PROJECT = exports.BUSY = exports.UNAVAILABLE = exports.INPUT_BYTES_MAX = exports.SIZE_MAX = exports.SIZE_MIN = void 0;
exports.accessOf = accessOf;
exports.gitFirstOnPath = gitFirstOnPath;
exports.clampSize = clampSize;
exports.stopped = stopped;
const node_path_1 = require("node:path");
const engine_js_1 = require("./engine.js");
exports.SIZE_MIN = 2;
exports.SIZE_MAX = 1000;
exports.INPUT_BYTES_MAX = 4096;
exports.UNAVAILABLE = "This terminal is unavailable: this system has no qualified sandbox for it.";
exports.BUSY = "This terminal can't start: too many are open. Close one, then open a new terminal.";
exports.ABOVE_PROJECT = "Raise project access to use this";
const SIZE_DEFAULT = { columns: 80, rows: 24 };
const CODE = /^[a-z0-9_]{1,64}$/;
exports.ACCESS_ENV = "CRAZYAGENT_IDE_ACCESS";
function accessOf(_value) {
    return "full";
}
function gitFirstOnPath(collection, gitPath) {
    if (typeof gitPath !== "string" || !(0, node_path_1.isAbsolute)(gitPath))
        return;
    const folder = (0, node_path_1.dirname)(gitPath);
    if (folder.includes(":"))
        return;
    collection.prepend("PATH", `${folder}:`);
}
class Signal {
    #listeners = new Set();
    event = (listener) => {
        this.#listeners.add(listener);
        return { dispose: () => this.#listeners.delete(listener) };
    };
    fire(value) {
        for (const listener of [...this.#listeners])
            listener(value);
    }
}
function clampSize(size) {
    const side = (value, fallback) => Number.isFinite(value) ? Math.min(exports.SIZE_MAX, Math.max(exports.SIZE_MIN, Math.trunc(value))) : fallback;
    return { columns: side(size?.columns, SIZE_DEFAULT.columns), rows: side(size?.rows, SIZE_DEFAULT.rows) };
}
function stopped(code) {
    if (code === "ide_terminal_unavailable")
        return exports.UNAVAILABLE;
    if (code === "ide_terminals_busy")
        return exports.BUSY;
    if (code === "ide_terminal_above_project")
        return exports.ABOVE_PROJECT;
    return `This terminal stopped (${CODE.test(code) ? code : "unknown"}).`;
}
class SandboxedPty {
    #wrote = new Signal();
    #closed = new Signal();
    onDidWrite = this.#wrote.event;
    onDidClose = this.#closed.event;
    #open;
    #level;
    #text = new TextDecoder("utf-8");
    #socket;
    #size = SIZE_DEFAULT;
    #attached = false;
    #ended = false;
    constructor(open, level) {
        this.#open = open;
        this.#level = level;
    }
    open(initial) {
        const opened = clampSize(initial);
        this.#size = opened;
        this.#open("ide.terminal", { cols: opened.columns, rows: opened.rows, level: this.#level }).then((socket) => this.#attach(socket, opened), (error) => this.#stop(error instanceof Error ? error.message : "unknown"));
    }
    #attach(socket, opened) {
        if (this.#ended) {
            socket.destroy();
            return;
        }
        this.#socket = socket;
        const reader = new engine_js_1.FrameReader();
        socket.on("data", (chunk) => {
            let frames;
            try {
                frames = reader.push(chunk);
            }
            catch {
                this.#stop("ide_frame_invalid");
                return;
            }
            for (const frame of frames)
                this.#take((frame ?? {}));
        });
        socket.on("error", () => undefined);
        socket.on("close", () => this.#stop("ide_engine_closed"));
        if (this.#size.columns !== opened.columns || this.#size.rows !== opened.rows)
            this.setDimensions(this.#size);
    }
    handleInput(data) {
        if (this.#ended || this.#socket === undefined)
            return;
        const bytes = Buffer.from(data, "utf8");
        for (let at = 0; at < bytes.length; at += exports.INPUT_BYTES_MAX) {
            this.#send({ kind: "input", bytes: [...bytes.subarray(at, at + exports.INPUT_BYTES_MAX)] });
        }
    }
    setDimensions(size) {
        this.#size = clampSize(size);
        if (this.#ended || this.#socket === undefined)
            return;
        this.#send({ kind: "resize", cols: this.#size.columns, rows: this.#size.rows });
    }
    close() {
        if (this.#ended)
            return;
        this.#ended = true;
        this.#send({ kind: "detach" });
        this.#socket?.end();
    }
    #take(frame) {
        if (this.#ended)
            return;
        if (!this.#attached && frame.kind === "ok") {
            this.#attached = true;
        }
        else if (!this.#attached && frame.kind === "rejected") {
            this.#stop(String(frame.code));
        }
        else if (this.#attached && frame.kind === "output" && Array.isArray(frame.bytes)) {
            const text = this.#text.decode(Buffer.from(frame.bytes), { stream: true });
            if (text.length > 0)
                this.#wrote.fire(text);
        }
        else if (this.#attached && frame.kind === "closed") {
            this.#stop(String(frame.reason));
        }
        else {
            this.#stop("ide_frame_invalid");
        }
    }
    #stop(reason) {
        if (this.#ended)
            return;
        this.#ended = true;
        this.#socket?.destroy();
        const rest = this.#text.decode();
        if (rest.length > 0)
            this.#wrote.fire(rest);
        if (reason === "terminal_ended") {
            this.#closed.fire();
        }
        else {
            this.#wrote.fire(`\r\n${stopped(reason)}\r\n`);
        }
    }
    #send(value) {
        this.#socket?.write((0, engine_js_1.encodeFrame)(value));
    }
}
exports.SandboxedPty = SandboxedPty;
