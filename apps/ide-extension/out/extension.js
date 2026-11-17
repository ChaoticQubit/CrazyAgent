"use strict";
var __createBinding = (this && this.__createBinding) || (Object.create ? (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    var desc = Object.getOwnPropertyDescriptor(m, k);
    if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
      desc = { enumerable: true, get: function() { return m[k]; } };
    }
    Object.defineProperty(o, k2, desc);
}) : (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    o[k2] = m[k];
}));
var __setModuleDefault = (this && this.__setModuleDefault) || (Object.create ? (function(o, v) {
    Object.defineProperty(o, "default", { enumerable: true, value: v });
}) : function(o, v) {
    o["default"] = v;
});
var __importStar = (this && this.__importStar) || (function () {
    var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function (o) {
            var ar = [];
            for (var k in o) if (Object.prototype.hasOwnProperty.call(o, k)) ar[ar.length] = k;
            return ar;
        };
        return ownKeys(o);
    };
    return function (mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        __setModuleDefault(result, mod);
        return result;
    };
})();
Object.defineProperty(exports, "__esModule", { value: true });
exports.SESSION_TAKEN = void 0;
exports.activate = activate;
exports.deactivate = deactivate;
const vscode = __importStar(require("vscode"));
const base_content_js_1 = require("./base-content.js");
const comments_js_1 = require("./comments.js");
const engine_js_1 = require("./engine.js");
const extensions_js_1 = require("./extensions.js");
const git_js_1 = require("./git.js");
const lock_js_1 = require("./lock.js");
const readonly_js_1 = require("./readonly.js");
const reveal_js_1 = require("./reveal.js");
const run_command_js_1 = require("./run-command.js");
const agents_js_1 = require("./sidebar/agents.js");
const changes_js_1 = require("./sidebar/changes.js");
const context_js_1 = require("./sidebar/context.js");
const memories_js_1 = require("./sidebar/memories.js");
const scm_js_1 = require("./scm.js");
const terminal_js_1 = require("./terminal.js");
const trust_js_1 = require("./trust.js");
exports.SESSION_TAKEN = "CrazyAgent ended this IDE's connection because its access was used twice: by another extension in it, by a second window of it opening at the same moment, or after CrazyAgent could not remove its access file. Close the IDE and open it again from CrazyAgent. If it happens again with one window, an extension you approved for this project may not be safe.";
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
    run_command_js_1.runCommand,
    terminal_js_1.sandboxedTerminal,
    terminal_js_1.terminalMenu,
    terminal_js_1.terminalGit,
    trust_js_1.registerTrust,
    git_js_1.remoteGit,
];
function activate(context) {
    (0, lock_js_1.startLock)(context, process.env);
    const taken = () => void vscode.window.showErrorMessage(exports.SESSION_TAKEN);
    const engine = engine_js_1.Engine.fromEnv(process.env, context.secrets, taken);
    if (engine === undefined)
        return;
    context.subscriptions.push(engine);
    for (const feature of features)
        feature(context, engine);
}
function deactivate() { }
