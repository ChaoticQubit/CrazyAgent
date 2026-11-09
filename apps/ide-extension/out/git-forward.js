"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.forward = forward;
exports.pullStep = pullStep;
const node_child_process_1 = require("node:child_process");
const node_net_1 = require("node:net");
const engine_js_1 = require("./engine.js");
const git_js_1 = require("./git.js");
const scm_model_js_1 = require("./scm-model.js");
const ANSWER_GRACE_MS = 5_000;
const SETTING_BYTES_MAX = 4096;
const REBASES = new Set(["true", "yes", "on", "1", "interactive", "i"]);
const REBASE_MERGES = new Set(["merges", "m"]);
const FALSE = new Set(["false", "no", "off", "0"]);
const TRUE = new Set(["true", "yes", "on", "1"]);
const DETACHED = "You are not currently on a branch.\nCrazyAgent pulls only a branch's upstream: switch to a branch that has one.\n";
const NO_UPSTREAM = "There is no tracking information for the current branch.\nCrazyAgent pulls only a branch's upstream: set one with git branch --set-upstream-to=<remote>/<branch>.\n";
async function forward(env, args, folder, stdout, stderr) {
    const step = args[0] === "pull" ? await pullStep(args, folder) : undefined;
    if (step !== undefined && "refused" in step) {
        stderr.write(step.refused);
        return 1;
    }
    const code = await ask(env, args, folder, stdout, stderr);
    if (step === undefined || code !== 0)
        return code;
    return local(step.argv, folder, stdout, stderr);
}
async function pullStep(args, folder) {
    const branch = await setting(folder, ["symbolic-ref", "--quiet", "--short", "HEAD"]);
    if (branch === undefined)
        return { refused: DETACHED };
    const upstream = await setting(folder, ["rev-parse", "--symbolic-full-name", "@{upstream}"]);
    if (upstream === undefined || !upstream.startsWith("refs/"))
        return { refused: NO_UPSTREAM };
    const [remote, ref] = args.slice(1).filter((arg) => !arg.startsWith("-"));
    if (remote !== undefined) {
        const own = await setting(folder, ["config", "--get", `branch.${branch}.remote`]);
        const merge = await setting(folder, ["config", "--get", `branch.${branch}.merge`]);
        if (remote !== own || (ref !== undefined && ref !== merge && `refs/heads/${ref}` !== merge)) {
            return { refused: `CrazyAgent pulls only the upstream of '${branch}' (${upstream}).\n` };
        }
    }
    const autostash = args.includes("--autostash") ? ["--autostash"] : [];
    const ofBranch = await setting(folder, ["config", "--get", `branch.${branch}.rebase`]);
    const rebase = (ofBranch ?? (await setting(folder, ["config", "--get", "pull.rebase"])))?.toLowerCase();
    const ff = (await setting(folder, ["config", "--get", "pull.ff"]))?.toLowerCase();
    const configured = ff !== "only" && rebase !== undefined;
    const merges = configured && REBASE_MERGES.has(rebase) && !args.includes("-r");
    if (args.includes("-r") || merges || (configured && REBASES.has(rebase))) {
        const fork = await setting(folder, ["merge-base", "--fork-point", upstream]);
        const from = fork === undefined || fork === "" ? [upstream] : ["--onto", upstream, fork];
        return { argv: ["rebase", ...(merges ? ["--rebase-merges"] : []), ...autostash, ...from] };
    }
    const mode = ff === "only"
        ? ["--ff-only"]
        : ff !== undefined && FALSE.has(ff)
            ? ["--no-ff"]
            : ff !== undefined && TRUE.has(ff)
                ? ["--ff"]
                : rebase === undefined
                    ? ["--ff-only"]
                    : [];
    return { argv: ["merge", ...mode, ...autostash, upstream] };
}
function setting(folder, args) {
    return new Promise((resolve) => {
        (0, node_child_process_1.execFile)("git", args, { cwd: folder, maxBuffer: SETTING_BYTES_MAX }, (error, stdout) => {
            resolve(error ? undefined : stdout.trim());
        });
    });
}
function local(args, folder, stdout, stderr) {
    return new Promise((resolve) => {
        const child = (0, node_child_process_1.spawn)("git", args, { cwd: folder, stdio: ["ignore", "pipe", "pipe"] });
        child.stdout.setEncoding("utf8").on("data", (text) => stdout.write(text));
        child.stderr.setEncoding("utf8").on("data", (text) => stderr.write(text));
        child.on("error", () => {
            stderr.write("fatal: git did not start for the local step of this pull\n");
            resolve(128);
        });
        child.on("close", (code) => resolve(code ?? 128));
    });
}
function ask(env, args, folder, stdout, stderr) {
    const socket = env[git_js_1.GIT_SOCKET_ENV];
    if (!socket) {
        stderr.write("fatal: CrazyAgent runs this git command in its engine, and the IDE has not connected to it yet\n");
        return Promise.resolve(128);
    }
    return new Promise((resolve) => {
        const connection = (0, node_net_1.createConnection)(socket);
        const reader = new engine_js_1.FrameReader();
        let done = false;
        let sent = false;
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
        const lost = () => finish(128, sent
            ? `fatal: ${(0, scm_model_js_1.unconfirmed)(git_js_1.GIT_OUTCOME_UNKNOWN) ?? git_js_1.GIT_OUTCOME_UNKNOWN}\n`
            : "fatal: the CrazyAgent IDE relay is not reachable\n");
        const timer = setTimeout(lost, git_js_1.GIT_REQUEST_MS + ANSWER_GRACE_MS);
        connection.on("connect", () => {
            connection.write((0, engine_js_1.encodeFrame)({ args, folder, secret: env[git_js_1.GIT_SECRET_ENV] }));
            sent = true;
        });
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
                finish(128, `fatal: ${(0, scm_model_js_1.unconfirmed)(reply.code) ?? `CrazyAgent did not run this git command (${reply.code})`}\n`);
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
        connection.on("error", lost);
        connection.on("close", lost);
    });
}
if (require.main === module) {
    const env = { ...process.env };
    delete process.env[git_js_1.GIT_SECRET_ENV];
    void forward(env, process.argv.slice(2), process.cwd(), process.stdout, process.stderr).then((code) => {
        process.exitCode = code;
    });
}
