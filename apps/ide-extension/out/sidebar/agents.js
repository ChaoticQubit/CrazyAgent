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
exports.sidebarAgents = sidebarAgents;
const vscode = __importStar(require("vscode"));
const model_js_1 = require("./model.js");
const tree_js_1 = require("./tree.js");
const MESSAGE_BYTES_MAX = 16384;
const REFUSALS = {
    team_message_invalid: "The message is empty or too long.",
    team_agent_not_found: "That agent is not a running sub-agent of this project.",
    team_agent_not_running: "That agent's terminal is not running.",
    team_agent_unreachable: "That agent's terminal did not take the message.",
};
function sidebarAgents(context, engine) {
    const tree = new tree_js_1.RowTree("crazyagent.agents", (row, item) => {
        const agent = row.agent;
        if (agent === undefined)
            return;
        item.iconPath = new vscode.ThemeIcon(icon(agent));
        item.tooltip = `${row.label} (${agent.harness}): ${row.description}`;
    });
    const load = async () => {
        const reply = await (0, tree_js_1.ask)(engine, "ide.agents");
        if (reply?.kind !== "ok") {
            tree.say((0, tree_js_1.trouble)(reply));
            return;
        }
        tree.say(reply.data.length === 0 ? "No agents yet." : undefined);
        tree.show((0, model_js_1.agentGroups)(reply.data));
    };
    const refresh = (0, tree_js_1.coalesced)(load);
    const worktree = async (row) => {
        const path = row?.agent?.worktree;
        if (path === undefined || path === null)
            return;
        const folder = vscode.Uri.file(path);
        if (vscode.workspace.getWorkspaceFolder(folder) === undefined) {
            void vscode.window.showInformationMessage("This agent's worktree is not in the workspace yet. Show all agents to add it.");
            return;
        }
        await vscode.commands.executeCommand("revealInExplorer", folder);
    };
    const message = async (row) => {
        const agent = row?.agent;
        if (agent === undefined || !agent.message)
            return;
        const text = await vscode.window.showInputBox({
            title: `Message ${row?.label ?? "agent"}`,
            prompt: "Typed into the agent's terminal as one message. The master hears about it on its next turn.",
            validateInput: (value) => {
                if (value.trim() === "")
                    return "Type a message.";
                if (Buffer.byteLength(value, "utf8") > MESSAGE_BYTES_MAX)
                    return "The message is too long.";
                return undefined;
            },
        });
        if (text === undefined)
            return;
        const reply = await (0, tree_js_1.ask)(engine, "ide.agent_message", { agent: agent.id, text });
        if (reply?.kind === "ok") {
            void vscode.window.showInformationMessage(`Sent to ${row?.label ?? "the agent"}.`);
        }
        else {
            void vscode.window.showWarningMessage(reply?.kind === "rejected" ? REFUSALS[reply.code] ?? (0, tree_js_1.trouble)(reply) : (0, tree_js_1.trouble)(reply));
        }
    };
    context.subscriptions.push(tree, engine.on("agents_changed", refresh), vscode.commands.registerCommand("crazyagent.agents.message", message), vscode.commands.registerCommand("crazyagent.agents.openWorktree", worktree), vscode.commands.registerCommand("crazyagent.agents.refresh", refresh));
    refresh();
}
function icon(agent) {
    if (agent.role === "master")
        return "star-full";
    if (agent.status === "working")
        return "sync";
    if (agent.status === "stopped" || agent.status === "failed")
        return "debug-stop";
    return "person";
}
