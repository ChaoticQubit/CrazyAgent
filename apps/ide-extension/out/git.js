"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.GIT_ARGS_MAX = exports.GIT_REQUEST_MS = exports.GIT_FORWARD_ENV = exports.GIT_NODE_ENV = exports.GIT_SOCKET_ENV = void 0;
exports.gitCall = gitCall;
exports.gitOutcome = gitOutcome;
exports.startRelay = startRelay;
exports.remoteGit = remoteGit;
const node_crypto_1 = require("node:crypto");
const node_net_1 = require("node:net");
const node_os_1 = require("node:os");
const node_path_1 = require("node:path");
const engine_js_1 = require("./engine.js");
exports.GIT_SOCKET_ENV = "CRAZYAGENT_IDE_GIT_SOCKET";
exports.GIT_NODE_ENV = "CRAZYAGENT_IDE_GIT_NODE";
exports.GIT_FORWARD_ENV = "CRAZYAGENT_IDE_GIT_FORWARD";
exports.GIT_REQUEST_MS = 700_000;
exports.GIT_ARGS_MAX = 16;
const ARG_BYTES_MAX = 1024;
const FOLDER_BYTES_MAX = 4096;
const CONNECTIONS_MAX = 4;
const PLAIN_CODE = /^[a-z][a-z0-9_]{0,63}$/;
function gitCall(value) {
    if (typeof value !== "object" || value === null)
        return undefined;
    const { args, folder } = value;
    const plain = (text, max) => typeof text === "string" && text.length > 0 && Buffer.byteLength(text) <= max && !text.includes("\0");
    if (!Array.isArray(args) || args.length === 0 || args.length > exports.GIT_ARGS_MAX)
        return undefined;
    if (!args.every((arg) => plain(arg, ARG_BYTES_MAX)))
        return undefined;
    if (!plain(folder, FOLDER_BYTES_MAX) || !folder.startsWith("/"))
        return undefined;
    return { args, folder };
}
function gitOutcome(value) {
    if (typeof value !== "object" || value === null)
        return undefined;
    const { code, stdout, stderr } = value;
    if (typeof code !== "number" || !Number.isInteger(code) || code < 0 || code > 255)
        return undefined;
    if (typeof stdout !== "string" || typeof stderr !== "string")
        return undefined;
    return { code, stdout, stderr };
}
async function startRelay(engine, env, forwarder) {
    const path = (0, node_path_1.join)((0, node_os_1.tmpdir)(), `cai-git-${(0, node_crypto_1.randomBytes)(8).toString("hex")}.sock`);
    const server = (0, node_net_1.createServer)((connection) => relay(engine, connection));
    server.maxConnections = CONNECTIONS_MAX;
    await new Promise((resolve, reject) => {
        server.once("error", reject);
        server.listen(path, () => {
            server.off("error", reject);
            resolve();
        });
    });
    env[exports.GIT_SOCKET_ENV] = path;
    env[exports.GIT_NODE_ENV] = process.execPath;
    env[exports.GIT_FORWARD_ENV] = forwarder;
    return {
        dispose: () => {
            if (env[exports.GIT_SOCKET_ENV] === path) {
                delete env[exports.GIT_SOCKET_ENV];
                delete env[exports.GIT_NODE_ENV];
                delete env[exports.GIT_FORWARD_ENV];
            }
            server.close();
        },
    };
}
function relay(engine, connection) {
    const reader = new engine_js_1.FrameReader();
    let asked = false;
    connection.on("error", () => undefined);
    connection.on("data", (chunk) => {
        if (asked)
            return;
        let frames;
        try {
            frames = reader.push(chunk);
        }
        catch {
            connection.destroy();
            return;
        }
        if (frames.length === 0)
            return;
        asked = true;
        const call = gitCall(frames[0]);
        if (call === undefined) {
            answer(connection, { kind: "rejected", code: "ide_git_refused" });
            return;
        }
        engine.request("ide.git", { args: call.args, folder: call.folder }, exports.GIT_REQUEST_MS).then((reply) => answer(connection, reply.kind === "ok" ? reply : { kind: "rejected", code: reply.code }), (error) => {
            const code = error instanceof Error && PLAIN_CODE.test(error.message) ? error.message : "ide_git_bridge_failed";
            answer(connection, { kind: "rejected", code });
        });
    });
}
function answer(connection, reply) {
    if (!connection.destroyed)
        connection.end((0, engine_js_1.encodeFrame)(reply));
}
function remoteGit(context, engine) {
    startRelay(engine, process.env, (0, node_path_1.join)(context.extensionPath, "out", "git-forward.js")).then((relay) => context.subscriptions.push(relay), () => undefined);
}
