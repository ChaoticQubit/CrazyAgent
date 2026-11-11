"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.activate = activate;
exports.deactivate = deactivate;
const base_content_js_1 = require("./base-content.js");
const comments_js_1 = require("./comments.js");
const engine_js_1 = require("./engine.js");
const extensions_js_1 = require("./extensions.js");
const git_js_1 = require("./git.js");
const lock_js_1 = require("./lock.js");
const readonly_js_1 = require("./readonly.js");
const reveal_js_1 = require("./reveal.js");
const agents_js_1 = require("./sidebar/agents.js");
const changes_js_1 = require("./sidebar/changes.js");
const context_js_1 = require("./sidebar/context.js");
const memories_js_1 = require("./sidebar/memories.js");
const scm_js_1 = require("./scm.js");
const terminal_js_1 = require("./terminal.js");
const trust_js_1 = require("./trust.js");
const features = [
    base_content_js_1.registerBaseContent,
    scm_js_1.registerChanges,
    comments_js_1.registerComments,
    memories_js_1.sidebarMemories,
    agents_js_1.sidebarAgents,
    changes_js_1.sidebarChanges,
    context_js_1.sidebarContext,
    readonly_js_1.readonly,
    extensions_js_1.extensions,
    reveal_js_1.reveal,
    terminal_js_1.sandboxedTerminal,
    terminal_js_1.terminalMenu,
    terminal_js_1.terminalGit,
    trust_js_1.registerTrust,
    git_js_1.remoteGit,
];
function activate(context) {
    (0, lock_js_1.startLock)(context, process.env);
    const engine = engine_js_1.Engine.fromEnv(process.env, context.secrets);
    if (engine === undefined)
        return;
    context.subscriptions.push(engine);
    for (const feature of features)
        feature(context, engine);
}
function deactivate() { }
