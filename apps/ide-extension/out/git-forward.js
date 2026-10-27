"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.forward = forward;
const node_net_1 = require("node:net");
const engine_js_1 = require("./engine.js");
const git_js_1 = require("./git.js");
const ANSWER_GRACE_MS = 5_000;
function forward(env, args, folder, stdout, stderr) {
    const socket = env[git_js_1.GIT_SOCKET_ENV];
    if (!socket) {
        stderr.write("fatal: CrazyAgent runs this git command in its engine, and the IDE has not connected to it yet\n");
        return Promise.resolve(128);
    }
    return new Promise((resolve) => {
        const connection = (0, node_net_1.createConnection)(socket);
        const reader = new engine_js_1.FrameReader();
        let done = false;
        const finish = (code, message) => {
            if (done)
                return;
            done = true;
            clearTimeout(timer);
            if (message !== undefined)
                stderr.write(message);
            connection.destroy();
            resolve(code);
        };
        const timer = setTimeout(() => finish(128, "fatal: the CrazyAgent engine did not answer this git command in time\n"), git_js_1.GIT_REQUEST_MS + ANSWER_GRACE_MS);
        connection.on("connect", () => connection.write((0, engine_js_1.encodeFrame)({ args, folder })));
        connection.on("data", (chunk) => {
            let replies;
            try {
                replies = reader.push(chunk);
            }
            catch {
                finish(128, "fatal: the CrazyAgent engine sent an unreadable answer\n");
                return;
            }
            const [reply] = replies;
            if (reply === undefined)
                return;
            if (reply.kind !== "ok") {
                finish(128, `fatal: CrazyAgent did not run this git command (${reply.code})\n`);
                return;
            }
            const outcome = (0, git_js_1.gitOutcome)(reply.data);
            if (outcome === undefined) {
                finish(128, "fatal: the CrazyAgent engine sent an unreadable answer\n");
                return;
            }
            stdout.write(outcome.stdout);
            stderr.write(outcome.stderr);
            finish(outcome.code);
        });
        connection.on("error", () => finish(128, "fatal: the CrazyAgent IDE relay is not reachable\n"));
        connection.on("close", () => finish(128, "fatal: the CrazyAgent IDE relay closed before it answered\n"));
    });
}
if (require.main === module) {
    void forward(process.env, process.argv.slice(2), process.cwd(), process.stdout, process.stderr).then((code) => {
        process.exitCode = code;
    });
}
