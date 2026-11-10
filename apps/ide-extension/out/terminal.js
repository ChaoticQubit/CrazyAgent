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
exports.FULL_TITLE = exports.OWN_BRANCH_TITLE = exports.OWN_BRANCH_PROFILE = exports.TITLE = exports.PROFILE = void 0;
exports.terminalMenu = terminalMenu;
exports.terminalGit = terminalGit;
exports.sandboxedTerminal = sandboxedTerminal;
const vscode = __importStar(require("vscode"));
const pty_js_1 = require("./pty.js");
exports.PROFILE = "crazyagent.sandboxed";
exports.TITLE = "Sandboxed";
exports.OWN_BRANCH_PROFILE = "crazyagent.ownBranch";
exports.OWN_BRANCH_TITLE = "Own branch";
exports.FULL_TITLE = "Full access";
const OFFERED = [
    { id: exports.PROFILE, name: exports.TITLE, level: "sandboxed", command: "crazyagent.terminal.sandboxed" },
    { id: exports.OWN_BRANCH_PROFILE, name: exports.OWN_BRANCH_TITLE, level: "own_branch", command: "crazyagent.terminal.ownBranch" },
];
const ACCESS_KEY = "crazyagent.ideAccess";
const FULL_COMMAND = "crazyagent.terminal.full";
const ABOVE = ["crazyagent.terminal.ownBranchAbove", "crazyagent.terminal.fullAbove"];
function engineTerminal(engine, name, level) {
    return { name, pty: new pty_js_1.SandboxedPty((call, args) => engine.stream(call, args), level) };
}
function terminalMenu(context, engine) {
    void vscode.commands.executeCommand("setContext", ACCESS_KEY, (0, pty_js_1.accessOf)(process.env[pty_js_1.ACCESS_ENV]));
    for (const { command, name, level } of OFFERED) {
        context.subscriptions.push(vscode.commands.registerCommand(command, () => {
            vscode.window.createTerminal(engineTerminal(engine, name, level));
        }));
    }
    context.subscriptions.push(vscode.commands.registerCommand(FULL_COMMAND, () => vscode.commands.executeCommand("workbench.action.terminal.newWithProfile", { profileName: exports.FULL_TITLE })));
    for (const command of ABOVE) {
        context.subscriptions.push(vscode.commands.registerCommand(command, () => {
            void vscode.window.showInformationMessage(pty_js_1.ABOVE_PROJECT);
        }));
    }
}
function terminalGit(context) {
    (0, pty_js_1.gitFirstOnPath)(context.environmentVariableCollection, vscode.workspace.getConfiguration("git").get("path"));
}
function sandboxedTerminal(context, engine) {
    for (const { id, name, level } of OFFERED) {
        context.subscriptions.push(vscode.window.registerTerminalProfileProvider(id, {
            provideTerminalProfile: () => new vscode.TerminalProfile(engineTerminal(engine, name, level)),
        }));
    }
    context.subscriptions.push(vscode.window.onDidOpenTerminal((terminal) => {
        if (OFFERED.some(({ name }) => name === terminal.name))
            terminal.show(false);
    }));
}
